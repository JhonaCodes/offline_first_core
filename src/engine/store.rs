//! Storage: one LMDB environment (natdb) holding the legacy records, the
//! catalog of table definitions, and one database per table and per index.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

use log::info;
use natdb::{
    Cursor, Database, DatabaseFlags, Environment, EnvironmentFlags, RwTransaction, Transaction,
    WriteFlags,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::{EngineError, EngineResult};
use super::keys::encode_key;
use super::schema::{IndexDef, TableDef};
use super::sync::{self, SYNC_DB_NAME};
use super::value::field;

/// Name of the database holding the records of the legacy API (0.5.0 layout).
const LEGACY_DB_NAME: &str = "main";
/// Name of the database holding table definitions and sequences.
const CATALOG_DB_NAME: &str = "__catalog";
const TABLE_PREFIX: &str = "table:";
const SEQUENCE_PREFIX: &str = "seq:";

/// How commits reach stable storage (RFC-001 §10.16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    /// Every commit is flushed before it returns (LMDB's default; on Apple
    /// platforms `F_FULLFSYNC`). A committed transaction survives a power loss.
    #[default]
    Full,
    /// Skips the flush of the meta page: a system crash may undo the last
    /// transaction, never corrupt the database.
    NoMetaSync,
    /// Leaves flushing to the operating system: a system crash may undo the
    /// last transactions (an application crash loses nothing). The database
    /// stays consistent on file systems that keep write order.
    NoSync,
}

/// Options for opening a database. They apply when the environment is
/// opened; a path already open in the process keeps its options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenOptions {
    /// Maximum number of tables plus indexes, plus two internal databases
    /// (three once a table is synchronized).
    pub max_dbs: u32,
    /// Initial size of the memory map, in bytes. It is reserved address space,
    /// not disk space: the file grows with the data.
    pub initial_map_size: usize,
    /// The map doubles when full, up to this size.
    pub max_map_size: usize,
    /// How commits reach stable storage.
    pub durability: Durability,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            max_dbs: 1024,
            initial_map_size: 64 << 20,
            max_map_size: if cfg!(target_pointer_width = "64") {
                16 << 30
            } else {
                1 << 30
            },
            durability: Durability::Full,
        }
    }
}

/// An index of a table.
#[derive(Debug)]
pub struct Index {
    /// Definition.
    pub def: IndexDef,
    /// LMDB database holding the index entries.
    pub db: Database,
}

/// A table and its LMDB databases.
#[derive(Debug)]
pub struct Table {
    /// Definition.
    pub def: TableDef,
    /// LMDB database holding the rows, keyed by encoded primary key.
    pub db: Database,
    /// Secondary indexes, in definition order.
    pub indexes: Vec<Index>,
}

impl Table {
    /// Encoded primary key of `row`, if present and not `NULL`.
    pub fn primary_key(&self, row: &Value) -> Option<Vec<u8>> {
        field(row, &self.def.primary_key)
            .filter(|value| !value.is_null())
            .map(encode_key)
    }
}

/// Largest encoded key, in bytes, on every platform.
///
/// LMDB 1.0 derives its own limit from the page size (about 2 KB with 4 KB
/// pages, 8 KB with the 16 KB pages of Apple Silicon). The protocol fixes 511
/// bytes, the limit of LMDB 0.9 and of flutter_local_db 1.x, so the same rows
/// fit on every device and server.
pub const PROTOCOL_MAX_KEY_SIZE: usize = 511;

/// Directories with an open [`Store`] in this process.
static OPEN_DIRS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

fn open_dirs() -> std::sync::MutexGuard<'static, HashSet<PathBuf>> {
    OPEN_DIRS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Keeps a directory registered in [`OPEN_DIRS`] while its environment is
/// open. Declared after the environment in [`Store`], so it is released only
/// once the environment is closed.
struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        open_dirs().remove(&self.0);
    }
}

/// An open storage environment.
///
/// A directory can only be open once per process: LMDB forbids opening one
/// environment twice (closing one copy would release the locks of the
/// other). A second [`Store::open`] fails with [`EngineError::AlreadyOpen`];
/// share the first one instead (the FFI layer and [`Db`](super::Db) do).
pub struct Store {
    env: Environment,
    legacy: Database,
    catalog: Database,
    /// The sync records, once a table is synchronized.
    sync: OnceLock<Database>,
    tables: RwLock<HashMap<String, Arc<Table>>>,
    max_key_size: usize,
    max_map_size: usize,
    needs_growth: AtomicBool,
    _dir: DirGuard,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (or creates) the environment in the directory `dir`.
    ///
    /// Fails with [`EngineError::LegacyFormat`], without touching the data,
    /// for a directory written by LMDB 0.9 (offline_first_core 0.5 and older).
    pub fn open(dir: &Path, options: &OpenOptions) -> EngineResult<Self> {
        fs::create_dir_all(dir)?;
        let canonical = dir.canonicalize()?;
        if !open_dirs().insert(canonical.clone()) {
            return Err(EngineError::AlreadyOpen(canonical));
        }
        let guard = DirGuard(canonical);
        let flags = match options.durability {
            Durability::Full => EnvironmentFlags::empty(),
            Durability::NoMetaSync => EnvironmentFlags::NO_META_SYNC,
            Durability::NoSync => EnvironmentFlags::NO_SYNC,
        };
        let env = Environment::new()
            .set_flags(flags)
            .set_max_dbs(options.max_dbs.max(2))
            .set_map_size(options.initial_map_size)
            .open(dir)?;
        let legacy = env.create_db(Some(LEGACY_DB_NAME), DatabaseFlags::empty())?;
        let catalog = env.create_db(Some(CATALOG_DB_NAME), DatabaseFlags::empty())?;
        let sync = OnceLock::new();
        match env.open_db(Some(SYNC_DB_NAME)) {
            Ok(db) => {
                let _ = sync.set(db);
            }
            Err(natdb::Error::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        // SAFETY: `env.env()` is the open environment; the call only reads it.
        let lmdb_max_key_size = unsafe { natdb_sys::mdb_env_get_maxkeysize(env.env()) };
        let store = Self {
            env,
            legacy,
            catalog,
            sync,
            tables: RwLock::new(HashMap::new()),
            max_key_size: usize::try_from(lmdb_max_key_size)
                .map_or(PROTOCOL_MAX_KEY_SIZE, |lmdb| {
                    lmdb.min(PROTOCOL_MAX_KEY_SIZE)
                }),
            max_map_size: options.max_map_size.max(options.initial_map_size),
            needs_growth: AtomicBool::new(false),
            _dir: guard,
        };
        store.load_tables()?;
        info!("Opened storage at {}", dir.display());
        Ok(store)
    }

    fn load_tables(&self) -> EngineResult<()> {
        let mut defs = Vec::new();
        {
            let txn = self.env.begin_ro_txn()?;
            let mut cursor = txn.open_ro_cursor(self.catalog)?;
            for entry in cursor.iter_from(TABLE_PREFIX) {
                let (key, value) = entry?;
                if !key.starts_with(TABLE_PREFIX.as_bytes()) {
                    break;
                }
                let def: TableDef =
                    serde_json::from_slice(value).map_err(|e| EngineError::CorruptRecord {
                        table: CATALOG_DB_NAME.to_string(),
                        detail: e.to_string(),
                    })?;
                defs.push(def);
            }
        }
        let mut tables = self.tables.write().unwrap_or_else(PoisonError::into_inner);
        for def in defs {
            let db = self.env.open_db(Some(&def.db_name()))?;
            let indexes = def
                .indexes
                .iter()
                .map(|index| {
                    Ok(Index {
                        db: self.env.open_db(Some(&def.index_db_name(index)))?,
                        def: index.clone(),
                    })
                })
                .collect::<EngineResult<Vec<_>>>()?;
            tables.insert(def.name.clone(), Arc::new(Table { def, db, indexes }));
        }
        Ok(())
    }

    /// The natdb environment.
    pub fn env(&self) -> &Environment {
        &self.env
    }

    /// Database of the legacy API.
    pub fn legacy_db(&self) -> Database {
        self.legacy
    }

    /// Database of the sync records, if a table was ever synchronized.
    pub(crate) fn sync_db(&self) -> Option<Database> {
        self.sync.get().copied()
    }

    /// Largest key LMDB accepts in this environment.
    pub fn max_key_size(&self) -> usize {
        self.max_key_size
    }

    /// The table `name`.
    pub fn table(&self, name: &str) -> EngineResult<Arc<Table>> {
        self.tables
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned()
            .ok_or_else(|| EngineError::TableNotFound(name.to_string()))
    }

    /// Definitions of all tables, by name.
    pub fn tables(&self) -> Vec<TableDef> {
        let tables = self.tables.read().unwrap_or_else(PoisonError::into_inner);
        let mut defs: Vec<TableDef> = tables.values().map(|t| t.def.clone()).collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Defines `def`, or updates the indexes of an existing table. Returns
    /// whether anything changed.
    ///
    /// New indexes are built over the existing rows in the same transaction;
    /// if a unique index finds duplicates the whole definition is rolled back.
    /// The primary key and `auto_increment` of an existing table cannot
    /// change.
    ///
    /// The caller must guarantee that no other transaction of this process
    /// runs meanwhile (the FFI layer holds its exclusive lock): databases are
    /// created and dropped inside the transaction.
    pub fn define_table(&self, def: TableDef) -> EngineResult<bool> {
        def.validate()?;
        let existing = self.table(&def.name).ok();
        if let Some(existing) = &existing {
            if existing.def == def {
                return Ok(false);
            }
            if existing.def.primary_key != def.primary_key
                || existing.def.auto_increment != def.auto_increment
            {
                return Err(EngineError::SchemaMismatch {
                    table: def.name.clone(),
                    detail: "the primary key and auto_increment cannot change".to_string(),
                });
            }
            if existing.def.sync.is_some() && existing.def.sync != def.sync {
                return Err(EngineError::SchemaMismatch {
                    table: def.name.clone(),
                    detail: "a synchronized table keeps its remote".to_string(),
                });
            }
        }
        let mut txn = self.env.begin_rw_txn()?;
        let created_sync = match (&def.sync, self.sync_db()) {
            // SAFETY: as below.
            (Some(_), None) => {
                Some(unsafe { txn.create_db(Some(SYNC_DB_NAME), DatabaseFlags::empty())? })
            }
            _ => None,
        };
        // SAFETY: the caller guarantees exclusive use of the environment, so no
        // other transaction opens, creates or drops databases concurrently. The
        // handles are only published after the commit.
        let db = unsafe { txn.create_db(Some(&def.db_name()), DatabaseFlags::empty())? };
        let mut indexes = Vec::with_capacity(def.indexes.len());
        let mut built = Vec::new();
        for index in &def.indexes {
            // SAFETY: as above.
            let index_db =
                unsafe { txn.create_db(Some(&def.index_db_name(index)), DatabaseFlags::empty())? };
            let is_new = existing
                .as_ref()
                .is_none_or(|t| !t.indexes.iter().any(|i| i.def == *index));
            if is_new {
                txn.clear_db(index_db)?;
                built.push(indexes.len());
            }
            indexes.push(Index {
                def: index.clone(),
                db: index_db,
            });
        }
        let table = Table {
            def: def.clone(),
            db,
            indexes,
        };
        if !built.is_empty() {
            self.build_indexes(&mut txn, &table, &built)?;
        }
        if let Some(existing) = &existing {
            for old in &existing.indexes {
                if !def.indexes.contains(&old.def) {
                    // SAFETY: as above; the dropped handle is not published.
                    unsafe { txn.drop_db(old.db)? };
                }
            }
        }
        let bytes =
            serde_json::to_vec(&def).map_err(|e| EngineError::InvalidSchema(e.to_string()))?;
        txn.put(
            self.catalog,
            &format!("{TABLE_PREFIX}{}", def.name),
            &bytes,
            WriteFlags::empty(),
        )?;
        txn.commit()?;
        if let Some(created) = created_sync {
            let _ = self.sync.set(created);
        }
        self.tables
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(def.name.clone(), Arc::new(table));
        info!("Defined table `{}`", def.name);
        Ok(true)
    }

    fn build_indexes(
        &self,
        txn: &mut RwTransaction<'_>,
        table: &Table,
        positions: &[usize],
    ) -> EngineResult<()> {
        let rows = {
            let mut cursor = txn.open_ro_cursor(table.db)?;
            let mut rows = Vec::new();
            for entry in cursor.iter_start() {
                let (key, value) = entry?;
                rows.push((
                    key.to_vec(),
                    super::exec::decode_row(&table.def.name, value)?,
                ));
            }
            rows
        };
        for (pk, row) in rows {
            for &position in positions {
                super::exec::add_index_entry(
                    self,
                    txn,
                    table,
                    &table.indexes[position],
                    &row,
                    &pk,
                )?;
            }
        }
        Ok(())
    }

    /// Drops the table `name` with its rows and indexes. Returns whether it
    /// existed. Same exclusivity requirement as [`Store::define_table`].
    pub fn drop_table(&self, name: &str) -> EngineResult<bool> {
        let Ok(table) = self.table(name) else {
            return Ok(false);
        };
        let mut txn = self.env.begin_rw_txn()?;
        // SAFETY: the caller guarantees exclusive use of the environment, and
        // the handles are removed from the cache below.
        unsafe {
            for index in &table.indexes {
                txn.drop_db(index.db)?;
            }
            txn.drop_db(table.db)?;
        }
        if let (Some(_), Some(sync_db)) = (&table.def.sync, self.sync_db()) {
            sync::purge_table(&mut txn, sync_db, name)?;
        }
        for key in [
            format!("{TABLE_PREFIX}{name}"),
            format!("{SEQUENCE_PREFIX}{name}"),
        ] {
            match txn.del(self.catalog, &key, None) {
                Ok(()) | Err(natdb::Error::NotFound) => {}
                Err(e) => return Err(e.into()),
            }
        }
        txn.commit()?;
        self.tables
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        info!("Dropped table `{name}`");
        Ok(true)
    }

    /// Next value of the sequence of `table`, inside `txn`.
    pub(crate) fn next_sequence(
        &self,
        txn: &mut RwTransaction<'_>,
        table: &str,
    ) -> EngineResult<i64> {
        let key = format!("{SEQUENCE_PREFIX}{table}");
        let current = match txn.get(self.catalog, &key) {
            Ok(bytes) => bytes.try_into().map(i64::from_be_bytes).map_err(|_| {
                EngineError::CorruptRecord {
                    table: CATALOG_DB_NAME.to_string(),
                    detail: format!("sequence of `{table}`"),
                }
            })?,
            Err(natdb::Error::NotFound) => 0,
            Err(e) => return Err(e.into()),
        };
        let next = current + 1;
        txn.put(self.catalog, &key, &next.to_be_bytes(), WriteFlags::empty())?;
        Ok(next)
    }

    /// Moves the sequence of `table` past `value` (an explicit key inserted
    /// into an auto-increment table).
    pub(crate) fn bump_sequence(
        &self,
        txn: &mut RwTransaction<'_>,
        table: &str,
        value: i64,
    ) -> EngineResult<()> {
        let key = format!("{SEQUENCE_PREFIX}{table}");
        let current = match txn.get(self.catalog, &key) {
            Ok(bytes) => bytes.try_into().map(i64::from_be_bytes).unwrap_or(0),
            Err(natdb::Error::NotFound) => 0,
            Err(e) => return Err(e.into()),
        };
        if value > current {
            txn.put(
                self.catalog,
                &key,
                &value.to_be_bytes(),
                WriteFlags::empty(),
            )?;
        }
        Ok(())
    }

    /// Records that a write failed with a full map, so that the next exclusive
    /// section grows it.
    pub(crate) fn request_growth(&self) {
        self.needs_growth.store(true, Ordering::SeqCst);
    }

    /// Whether a growth was requested.
    pub(crate) fn growth_requested(&self) -> bool {
        self.needs_growth.load(Ordering::SeqCst)
    }

    /// Doubles the memory map, up to the configured maximum. Returns whether
    /// it grew.
    ///
    /// LMDB only allows resizing while no transaction of this process is
    /// active: the caller must hold the exclusive lock.
    pub fn grow(&self) -> EngineResult<bool> {
        self.needs_growth.store(false, Ordering::SeqCst);
        let current = self.env.info()?.map_size();
        if current >= self.max_map_size {
            return Ok(false);
        }
        let next = current.saturating_mul(2).min(self.max_map_size);
        self.env.set_map_size(next)?;
        info!("Memory map grown from {current} to {next} bytes");
        Ok(true)
    }

    /// Size of the memory map, in bytes.
    pub fn map_size(&self) -> EngineResult<usize> {
        Ok(self.env.info()?.map_size())
    }
}

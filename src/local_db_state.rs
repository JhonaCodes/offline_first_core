//! The legacy key-value API (`push`, `get_by_id`, ...) of 0.5.0.
//!
//! Records are [`LocalDbModel`]s stored as JSON in the database `main` of the
//! environment, keyed by their id — the layout of 0.5.0. The same environment
//! also holds the tables of the query [`engine`](crate::engine), so an
//! application can move from this API to the engine without reopening
//! anything.
//!
//! Semantics kept from 0.5.0 on purpose (RFC-001 §20.2): `push` replaces an
//! existing record with the same id (upsert), `update` only writes existing
//! records.

use std::fs;
use std::io;
use std::path::Path;
use std::str::{self, Utf8Error};

use log::info;
use natdb::{Cursor, Transaction, WriteFlags};
use thiserror::Error;

use crate::engine::{EngineError, OpenOptions, Store};
use crate::local_db_model::LocalDbModel;

/// Errors of the legacy API.
#[derive(Debug, Error)]
pub enum DbError {
    /// A file system operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// LMDB reported an error.
    #[error("storage error: {0}")]
    Storage(#[from] natdb::Error),
    /// Opening the storage failed (for example [`EngineError::LegacyFormat`]
    /// or [`EngineError::AlreadyOpen`]).
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// A record could not be serialized.
    #[error("failed to serialize record: {0}")]
    Serialization(#[source] serde_json::Error),
    /// A stored record is not UTF-8.
    #[error("stored record `{id}` is not valid UTF-8: {source}")]
    Utf8 {
        /// Record id.
        id: String,
        /// Decoding error.
        source: Utf8Error,
    },
    /// A stored record is not a valid [`LocalDbModel`].
    #[error("stored record `{id}` is not a valid record: {source}")]
    Deserialization {
        /// Record id.
        id: String,
        /// Decoding error.
        source: serde_json::Error,
    },
    /// The database was closed, for example by a failed reset.
    #[error("database is closed")]
    Closed,
}

impl DbError {
    /// Whether the operation failed only because the memory map is full.
    pub fn is_map_full(&self) -> bool {
        matches!(
            self,
            Self::Storage(natdb::Error::MapFull) | Self::Engine(EngineError::MapFull)
        )
    }
}

/// Directory of the database `name`: `<name>.lmdb`.
pub(crate) fn db_dir_name(name: &str) -> String {
    format!("{name}.lmdb")
}

fn decode_record(id: &str, bytes: &[u8]) -> Result<LocalDbModel, DbError> {
    let json = str::from_utf8(bytes).map_err(|source| DbError::Utf8 {
        id: id.to_string(),
        source,
    })?;
    serde_json::from_str(json).map_err(|source| DbError::Deserialization {
        id: id.to_string(),
        source,
    })
}

/// A database opened through the legacy API.
///
/// ```no_run
/// use offline_first_core::local_db_model::LocalDbModel;
/// use offline_first_core::local_db_state::{AppDbState, DbError};
/// use serde_json::json;
///
/// let db = AppDbState::init("/absolute/path/app".to_string())?;
/// db.push(LocalDbModel { id: "user-1".into(), hash: "h1".into(), data: json!({"name": "Ada"}) })?;
/// assert!(db.get_by_id("user-1")?.is_some());
/// # Ok::<(), DbError>(())
/// ```
pub struct AppDbState {
    store: Option<Store>,
    path: String,
}

impl AppDbState {
    /// Opens (or creates) the database stored in `<name>.lmdb`, with default
    /// options.
    ///
    /// Fails with [`EngineError::LegacyFormat`] (wrapped in
    /// [`DbError::Engine`]) for a database written by 0.5.0 or older, whose
    /// LMDB 0.9 format LMDB 1.0 cannot read, and with
    /// [`EngineError::AlreadyOpen`] if the directory is already open in this
    /// process.
    pub fn init(name: String) -> Result<Self, DbError> {
        Self::open_dir(&db_dir_name(&name), &OpenOptions::default())
    }

    /// Opens (or creates) the database stored in the directory `dir`.
    pub(crate) fn open_dir(dir: &str, options: &OpenOptions) -> Result<Self, DbError> {
        let store = Store::open(Path::new(dir), options)?;
        info!("Database initialized at {dir}");
        Ok(Self {
            store: Some(store),
            path: dir.to_string(),
        })
    }

    /// The storage of this database, shared with the query engine.
    pub(crate) fn store(&self) -> Result<&Store, EngineError> {
        self.store.as_ref().ok_or(EngineError::Closed)
    }

    fn legacy_store(&self) -> Result<&Store, DbError> {
        self.store.as_ref().ok_or(DbError::Closed)
    }

    /// Stores `model` under its id, replacing any record with the same id.
    pub fn push(&self, model: LocalDbModel) -> Result<LocalDbModel, DbError> {
        let store = self.legacy_store()?;
        let json = serde_json::to_vec(&model).map_err(DbError::Serialization)?;
        let mut txn = store.env().begin_rw_txn()?;
        txn.put(store.legacy_db(), &model.id, &json, WriteFlags::empty())?;
        txn.commit()?;
        Ok(model)
    }

    /// The record `id`, if any.
    pub fn get_by_id(&self, id: &str) -> Result<Option<LocalDbModel>, DbError> {
        let store = self.legacy_store()?;
        let txn = store.env().begin_ro_txn()?;
        match txn.get(store.legacy_db(), &id) {
            Ok(bytes) => decode_record(id, bytes).map(Some),
            Err(natdb::Error::NotFound) => {
                info!("No value found for id {id}");
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// All records, in id order.
    ///
    /// A record that cannot be decoded fails the whole listing with
    /// [`DbError::Utf8`] or [`DbError::Deserialization`], naming its id:
    /// a listing that left it out would look complete while it is not. The
    /// caller recovers with [`AppDbState::delete_by_id`] (or a rewrite) of
    /// that id.
    pub fn get(&self) -> Result<Vec<LocalDbModel>, DbError> {
        let store = self.legacy_store()?;
        let txn = store.env().begin_ro_txn()?;
        let mut cursor = txn.open_ro_cursor(store.legacy_db())?;
        let mut models = Vec::new();
        for entry in cursor.iter_start() {
            let (key, value) = entry?;
            models.push(decode_record(&String::from_utf8_lossy(key), value)?);
        }
        Ok(models)
    }

    /// Deletes the record `id`. Returns whether it existed.
    pub fn delete_by_id(&self, id: &str) -> Result<bool, DbError> {
        let store = self.legacy_store()?;
        let mut txn = store.env().begin_rw_txn()?;
        let existed = match txn.del(store.legacy_db(), &id, None) {
            Ok(()) => true,
            Err(natdb::Error::NotFound) => false,
            Err(error) => return Err(error.into()),
        };
        txn.commit()?;
        Ok(existed)
    }

    /// Replaces the record `model.id` if it exists. Returns `None` (and writes
    /// nothing) when it does not.
    pub fn update(&self, model: LocalDbModel) -> Result<Option<LocalDbModel>, DbError> {
        let store = self.legacy_store()?;
        let mut txn = store.env().begin_rw_txn()?;
        match txn.get(store.legacy_db(), &model.id) {
            Ok(_) => {}
            Err(natdb::Error::NotFound) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let json = serde_json::to_vec(&model).map_err(DbError::Serialization)?;
        txn.put(store.legacy_db(), &model.id, &json, WriteFlags::empty())?;
        txn.commit()?;
        Ok(Some(model))
    }

    /// Deletes every legacy record in one transaction. Returns how many there
    /// were. Tables of the query engine are not touched.
    pub fn clear_all_records(&self) -> Result<usize, DbError> {
        let store = self.legacy_store()?;
        let mut txn = store.env().begin_rw_txn()?;
        let count = txn.stat(store.legacy_db())?.entries();
        txn.clear_db(store.legacy_db())?;
        txn.commit()?;
        Ok(count)
    }

    /// Deletes this database (records and tables) and opens an empty one at
    /// `<name>.lmdb`.
    ///
    /// Steps: close the environment, remove its directory, open the new one.
    /// If a step fails the database stays closed: every later operation,
    /// including another reset, returns [`DbError::Closed`] instead of touching
    /// files it no longer owns.
    ///
    /// This operation is destructive.
    pub fn reset_database(&mut self, name: &str) -> Result<bool, DbError> {
        // A closed database no longer owns `self.path`: after a failed reset
        // another database may already live there, so it must not be removed.
        let store = self.store.take().ok_or(DbError::Closed)?;
        drop(store);
        if Path::new(&self.path).exists() {
            fs::remove_dir_all(&self.path)?;
        }
        let new_dir = db_dir_name(name);
        self.store = Some(Store::open(Path::new(&new_dir), &OpenOptions::default())?);
        self.path = new_dir;
        Ok(true)
    }

    /// Kept for compatibility: resources are released when the value is
    /// dropped (the C API releases them in `close_database`).
    pub fn close_database(&mut self) -> Result<(), DbError> {
        Ok(())
    }
}

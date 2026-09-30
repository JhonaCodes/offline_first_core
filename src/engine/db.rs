//! The Rust API.

use std::path::Path;
use std::sync::Arc;

use natdb::Transaction;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::error::{EngineError, EngineResult};
use super::exec;
use super::schema::TableDef;
use super::stmt::{Output, Select, Statement};
use super::store::OpenOptions;
use super::tx::{self, ReadTx, WriteTx, WriterGuard};
use crate::registry::{self, SharedDb};

/// An open database.
///
/// Handles are cheap to clone and every handle of a process on the same
/// directory shares one LMDB environment, including the handles of the C API
/// used by the Dart SDK.
///
/// ```no_run
/// use offline_first_core::engine::{col, Db, Query, TableDef};
///
/// # fn main() -> Result<(), offline_first_core::engine::EngineError> {
/// let db = Db::open("/path/to/app.lmdb")?;
/// db.define_table(TableDef::new("skills", "id").index("by_priority", &["priority"]))?;
///
/// db.transaction(|tx| {
///     Query::insert_into("skills", [serde_json::json!({"id": "rust", "priority": 3})]).execute_in(tx)?;
///     Ok(())
/// })?;
///
/// let top: Vec<serde_json::Value> = db.load(
///     &Query::table("skills").filter(col("priority").gt(1)).order(col("priority").desc()).limit(10),
/// )?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Db {
    shared: Arc<SharedDb>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db").finish_non_exhaustive()
    }
}

impl Db {
    /// Opens (or creates) the database stored in the directory `dir`, with
    /// default options.
    pub fn open(dir: impl AsRef<Path>) -> EngineResult<Self> {
        Self::open_with(dir, OpenOptions::default())
    }

    /// Opens (or creates) the database stored in the directory `dir`.
    ///
    /// If the directory is already open in this process, the new handle shares
    /// it and `options` are ignored.
    pub fn open_with(dir: impl AsRef<Path>, options: OpenOptions) -> EngineResult<Self> {
        Ok(Self {
            shared: registry::acquire_dir(dir.as_ref(), &options)?,
        })
    }

    /// Defines a table, or adds and removes indexes of an existing one.
    /// Returns whether anything changed. Waits for running transactions.
    pub fn define_table(&self, def: TableDef) -> EngineResult<bool> {
        self.shared.exclusive(|store| store.define_table(def))
    }

    /// Drops a table and all its rows. Returns whether it existed.
    pub fn drop_table(&self, name: &str) -> EngineResult<bool> {
        self.shared.exclusive(|store| store.drop_table(name))
    }

    /// Definitions of all tables.
    pub fn tables(&self) -> EngineResult<Vec<TableDef>> {
        self.shared.run(|store| Ok(store.tables()))
    }

    /// Executes one statement in its own transaction.
    pub fn execute(&self, statement: &Statement) -> EngineResult<Output> {
        self.shared.run(|store| tx::autocommit(store, statement))
    }

    /// Executes `statements` atomically in one transaction.
    pub fn batch(&self, statements: &[Statement]) -> EngineResult<Vec<Output>> {
        self.shared.run(|store| tx::batch(store, statements))
    }

    /// Runs `query` and deserializes every row into `T`.
    pub fn load<T: DeserializeOwned>(&self, query: &Select) -> EngineResult<Vec<T>> {
        let output = self.execute(&Statement::Select(query.clone()))?;
        decode_rows(&output)
    }

    /// Runs `query` with `limit(1)` and returns the first row, if any.
    pub fn first<T: DeserializeOwned>(&self, query: &Select) -> EngineResult<Option<T>> {
        let mut query = query.clone();
        query.limit = Some(1);
        Ok(self.load(&query)?.into_iter().next())
    }

    /// The plan the planner chooses for `query`, without running it.
    pub fn explain(&self, query: &Select) -> EngineResult<Value> {
        self.shared.run(|store| exec::explain(store, query))
    }

    /// Runs `body` in a write transaction: `Ok` commits, `Err` rolls back.
    /// The value is returned only after the commit succeeded (RFC §10.3).
    ///
    /// A write that fails inside `body` makes the transaction rollback-only;
    /// use [`WriteTx::savepoint`] for work whose failure is recoverable.
    pub fn transaction<R>(
        &self,
        body: impl FnOnce(&mut WriteTx<'_>) -> EngineResult<R>,
    ) -> EngineResult<R> {
        self.shared.run_once(|store| {
            let _writer = WriterGuard::acquire(store)?;
            let txn = store.env().begin_rw_txn()?;
            let mut scope = WriteTx {
                store,
                txn,
                rollback_only: false,
            };
            let value = body(&mut scope)?;
            if scope.rollback_only {
                return Err(EngineError::TransactionAborted);
            }
            scope.txn.commit()?;
            Ok(value)
        })
    }

    /// Runs `body` against one consistent read-only snapshot.
    pub fn read_transaction<R>(
        &self,
        body: impl FnOnce(&ReadTx<'_>) -> EngineResult<R>,
    ) -> EngineResult<R> {
        self.shared.run_once(|store| {
            if WriterGuard::active(store) {
                return Err(EngineError::Reentrancy);
            }
            let scope = ReadTx {
                store,
                txn: store.env().begin_ro_txn()?,
            };
            body(&scope)
        })
    }

    /// Size of the memory map, in bytes.
    pub fn map_size(&self) -> EngineResult<usize> {
        self.shared.run(|store| store.map_size())
    }
}

/// Deserializes the rows of `output`.
pub fn decode_rows<T: DeserializeOwned>(output: &Output) -> EngineResult<Vec<T>> {
    let rows = match output {
        Output::Rows(rows) | Output::Affected { rows, .. } => rows.iter().collect::<Vec<_>>(),
        Output::Row(row) => row.iter().collect(),
        Output::Count(_) | Output::Value(_) => Vec::new(),
    };
    rows.into_iter()
        .map(|bytes| {
            serde_json::from_slice(bytes)
                .map_err(|e| EngineError::InvalidRequest(format!("cannot decode row: {e}")))
        })
        .collect()
}

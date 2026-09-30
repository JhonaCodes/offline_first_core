//! Transaction scopes shared by the Rust API and the interactive sessions of
//! the wire protocol.
//!
//! Rules (RFC §10.6 and §10.8):
//!
//! - Every write statement runs in its own nested LMDB transaction, so a
//!   failed statement leaves no partial writes.
//! - A failed write marks its scope *rollback-only*: catching the error does
//!   not make the scope committable again. Recoverable work goes into an
//!   explicit savepoint.
//! - A savepoint is a nested transaction: its writes become durable only when
//!   the root commits, and rolling it back discards them.

use std::cell::RefCell;
use std::collections::HashSet;

use natdb::{RwTransaction, Transaction};

use super::error::{EngineError, EngineResult};
use super::exec;
use super::stmt::{Output, Statement};
use super::store::Store;

/// Runs `statement` in `txn`; writes are atomic and failures poison the
/// scope through `rollback_only`.
pub(crate) fn run_statement(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    statement: &Statement,
    rollback_only: &mut bool,
) -> EngineResult<Output> {
    if *rollback_only {
        return Err(EngineError::TransactionAborted);
    }
    if !statement.is_write() {
        return exec::execute_read(store, &*txn, statement);
    }
    let result = txn
        .begin_nested_txn()
        .map_err(EngineError::from)
        .and_then(|mut child| {
            let output = exec::execute_write(store, &mut child, statement)?;
            child.commit()?;
            Ok(output)
        });
    if let Err(error) = &result {
        *rollback_only = true;
        if error.is_map_full() {
            store.request_growth();
        }
    }
    result
}

thread_local! {
    /// Databases (by store address) with a write transaction on this thread.
    static WRITERS: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
}

/// Marks a write transaction of `store` as running on this thread until
/// dropped; fails if one already is.
pub(crate) struct WriterGuard {
    key: usize,
}

impl WriterGuard {
    pub(crate) fn acquire(store: &Store) -> EngineResult<Self> {
        let key = store as *const Store as usize;
        if WRITERS.with(|writers| writers.borrow_mut().insert(key)) {
            Ok(Self { key })
        } else {
            Err(EngineError::Reentrancy)
        }
    }
}

impl Drop for WriterGuard {
    fn drop(&mut self) {
        WRITERS.with(|writers| {
            writers.borrow_mut().remove(&self.key);
        });
    }
}

/// A write transaction of the Rust API (see [`Db::transaction`](super::Db::transaction)).
pub struct WriteTx<'a> {
    pub(crate) store: &'a Store,
    pub(crate) txn: RwTransaction<'a>,
    pub(crate) rollback_only: bool,
}

impl<'a> WriteTx<'a> {
    /// Executes a statement in this transaction.
    pub fn execute(&mut self, statement: &Statement) -> EngineResult<Output> {
        run_statement(
            self.store,
            &mut self.txn,
            statement,
            &mut self.rollback_only,
        )
    }

    /// Runs `body` in a savepoint. `Ok` keeps its writes (durable when the
    /// root commits); `Err` rolls them back and returns the error, leaving this
    /// transaction usable.
    pub fn savepoint<R>(
        &mut self,
        body: impl FnOnce(&mut WriteTx<'_>) -> EngineResult<R>,
    ) -> EngineResult<R> {
        if self.rollback_only {
            return Err(EngineError::TransactionAborted);
        }
        let child = self.txn.begin_nested_txn()?;
        let mut scope = WriteTx {
            store: self.store,
            txn: child,
            rollback_only: false,
        };
        match body(&mut scope) {
            Ok(value) if !scope.rollback_only => {
                scope.txn.commit()?;
                Ok(value)
            }
            Ok(_) => Err(EngineError::TransactionAborted),
            Err(error) => Err(error),
        }
    }

    /// Whether a failed write made this transaction rollback-only.
    pub fn is_rollback_only(&self) -> bool {
        self.rollback_only
    }
}

/// A read-only snapshot of the Rust API (see [`Db::read_transaction`](super::Db::read_transaction)).
pub struct ReadTx<'a> {
    pub(crate) store: &'a Store,
    pub(crate) txn: natdb::RoTransaction<'a>,
}

impl ReadTx<'_> {
    /// Executes a read statement against this snapshot.
    pub fn execute(&self, statement: &Statement) -> EngineResult<Output> {
        exec::execute_read(self.store, &self.txn, statement)
    }
}

impl WriterGuard {
    /// Whether this thread runs a write transaction on `store`.
    pub(crate) fn active(store: &Store) -> bool {
        let key = store as *const Store as usize;
        WRITERS.with(|writers| writers.borrow().contains(&key))
    }
}

/// Executes `statement` in its own transaction: a snapshot for reads, a
/// write transaction committed on success for writes.
pub(crate) fn autocommit(store: &Store, statement: &Statement) -> EngineResult<Output> {
    if !statement.is_write() {
        if WriterGuard::active(store) {
            return Err(EngineError::Reentrancy);
        }
        let txn = store.env().begin_ro_txn()?;
        return exec::execute_read(store, &txn, statement);
    }
    let _writer = WriterGuard::acquire(store)?;
    let mut txn = store.env().begin_rw_txn()?;
    let output = exec::execute_write(store, &mut txn, statement)?;
    txn.commit()?;
    Ok(output)
}

/// Executes `statements` in order in one write transaction: all of them are
/// committed together, or none is (RFC §10.5). Never split.
pub(crate) fn batch(store: &Store, statements: &[Statement]) -> EngineResult<Vec<Output>> {
    let _writer = WriterGuard::acquire(store)?;
    let mut txn = store.env().begin_rw_txn()?;
    let outputs = statements
        .iter()
        .map(|statement| exec::execute_write(store, &mut txn, statement))
        .collect::<EngineResult<Vec<_>>>()?;
    txn.commit()?;
    Ok(outputs)
}

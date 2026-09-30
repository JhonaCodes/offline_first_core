//! Interactive transactions of the wire protocol.
//!
//! An LMDB transaction belongs to the thread that began it, while Dart may
//! call from any thread. Each session therefore gets an *owner thread* that
//! begins the transaction, runs every statement and savepoint of it, and
//! commits or aborts it (RFC §10.10). Callers talk to it through a channel.
//!
//! A session that receives nothing for its idle timeout is rolled back, so an
//! abandoned transaction (a Dart isolate killed mid-transaction) cannot keep
//! the database writer locked forever.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;

use log::{info, warn};
use natdb::{RoTransaction, RwTransaction, Transaction};

use super::error::{EngineError, EngineResult};
use super::exec;
use super::stmt::{Output, Statement};
use super::store::Store;
use super::tx::run_statement;
use crate::registry::SharedDb;

/// Kind of session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A read-write transaction.
    Write,
    /// A read-only snapshot.
    Read,
}

type Reply<T> = SyncSender<EngineResult<T>>;

enum Request {
    Execute(Statement, Reply<Output>),
    Savepoint(Reply<()>),
    Release(Reply<()>),
    RollbackTo(Reply<()>),
    Commit(Reply<()>),
    Rollback(Reply<()>),
}

enum Entry {
    Active {
        sender: Sender<Request>,
        owner: usize,
    },
    Expired(Duration),
}

static SESSIONS: OnceLock<Mutex<HashMap<u64, Entry>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn sessions() -> MutexGuard<'static, HashMap<u64, Entry>> {
    SESSIONS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn owner_key(shared: &Arc<SharedDb>) -> usize {
    Arc::as_ptr(shared) as usize
}

/// Begins a session on `shared` and returns its id. Blocks while another
/// write transaction holds the database writer.
pub(crate) fn begin(shared: &Arc<SharedDb>, mode: Mode, idle: Duration) -> EngineResult<u64> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed) + 1;
    let (sender, receiver) = mpsc::channel();
    let (ready_sender, ready) = mpsc::sync_channel(1);
    sessions().insert(
        id,
        Entry::Active {
            sender,
            owner: owner_key(shared),
        },
    );
    let owned = Arc::clone(shared);
    let spawned = thread::Builder::new()
        .name(format!("offline-first-core-tx-{id}"))
        .spawn(move || owner_thread(&owned, id, mode, idle, &receiver, &ready_sender));
    if let Err(error) = spawned {
        sessions().remove(&id);
        return Err(error.into());
    }
    match ready.recv() {
        Ok(Ok(())) => Ok(id),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(EngineError::TransactionClosed),
    }
}

/// Sends a request to the session `id` of `shared` and waits for the reply.
fn call<T>(
    shared: &Arc<SharedDb>,
    id: u64,
    make: impl FnOnce(Reply<T>) -> Request,
) -> EngineResult<T> {
    let sender = {
        let mut sessions = sessions();
        match sessions.get(&id) {
            Some(Entry::Active { sender, owner }) if *owner == owner_key(shared) => sender.clone(),
            Some(Entry::Active { .. }) => return Err(EngineError::UnknownTransaction(id)),
            Some(Entry::Expired(idle)) => {
                let idle = *idle;
                sessions.remove(&id);
                return Err(EngineError::TransactionExpired(idle));
            }
            None if id > NEXT_ID.load(Ordering::Relaxed) => {
                return Err(EngineError::UnknownTransaction(id))
            }
            None => return Err(EngineError::TransactionClosed),
        }
    };
    let (reply, response) = mpsc::sync_channel(1);
    sender
        .send(make(reply))
        .map_err(|_| EngineError::TransactionClosed)?;
    response
        .recv()
        .map_err(|_| EngineError::TransactionClosed)?
}

/// Executes `statement` in the session `id`.
pub(crate) fn execute(
    shared: &Arc<SharedDb>,
    id: u64,
    statement: Statement,
) -> EngineResult<Output> {
    call(shared, id, |reply| Request::Execute(statement, reply))
}

/// Opens a savepoint in the session `id`.
pub(crate) fn savepoint(shared: &Arc<SharedDb>, id: u64) -> EngineResult<()> {
    call(shared, id, Request::Savepoint)
}

/// Keeps the writes of the innermost savepoint (durable at the root commit).
pub(crate) fn release(shared: &Arc<SharedDb>, id: u64) -> EngineResult<()> {
    call(shared, id, Request::Release)
}

/// Discards the writes of the innermost savepoint.
pub(crate) fn rollback_to(shared: &Arc<SharedDb>, id: u64) -> EngineResult<()> {
    call(shared, id, Request::RollbackTo)
}

/// Commits the session `id` and ends it.
pub(crate) fn commit(shared: &Arc<SharedDb>, id: u64) -> EngineResult<()> {
    call(shared, id, Request::Commit)
}

/// Rolls back the session `id` and ends it.
pub(crate) fn rollback(shared: &Arc<SharedDb>, id: u64) -> EngineResult<()> {
    call(shared, id, Request::Rollback)
}

/// How a transaction level ended.
enum End {
    Commit(Reply<()>, bool),
    Rollback(Reply<()>),
    Release(Reply<()>, bool),
    RollbackTo(Reply<()>),
    Expired,
    Disconnected,
}

fn owner_thread(
    shared: &Arc<SharedDb>,
    id: u64,
    mode: Mode,
    idle: Duration,
    requests: &Receiver<Request>,
    ready: &SyncSender<EngineResult<()>>,
) {
    let state = shared.read();
    let store = match state.store() {
        Ok(store) => store,
        Err(error) => {
            sessions().remove(&id);
            let _ = ready.send(Err(error));
            return;
        }
    };
    let expired = match mode {
        Mode::Write => match store.env().begin_rw_txn() {
            Ok(mut txn) => {
                let _ = ready.send(Ok(()));
                let end = level(store, &mut txn, requests, 0, idle);
                finish(store, txn, end)
            }
            Err(error) => {
                sessions().remove(&id);
                let _ = ready.send(Err(error.into()));
                return;
            }
        },
        Mode::Read => match store.env().begin_ro_txn() {
            Ok(txn) => {
                let _ = ready.send(Ok(()));
                read_loop(store, &txn, requests, idle)
            }
            Err(error) => {
                sessions().remove(&id);
                let _ = ready.send(Err(error.into()));
                return;
            }
        },
    };
    let mut sessions = sessions();
    if expired {
        warn!("Transaction {id} rolled back after {idle:?} without activity");
        sessions.insert(id, Entry::Expired(idle));
    } else {
        sessions.remove(&id);
    }
}

/// Runs one level of a write transaction (the root or a savepoint).
fn level(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    requests: &Receiver<Request>,
    depth: usize,
    idle: Duration,
) -> End {
    let mut rollback_only = false;
    loop {
        let request = match requests.recv_timeout(idle) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => return End::Expired,
            Err(RecvTimeoutError::Disconnected) => return End::Disconnected,
        };
        match request {
            Request::Execute(statement, reply) => {
                let _ = reply.send(run_statement(store, txn, &statement, &mut rollback_only));
            }
            Request::Savepoint(reply) => {
                if rollback_only {
                    let _ = reply.send(Err(EngineError::TransactionAborted));
                    continue;
                }
                let mut child = match txn.begin_nested_txn() {
                    Ok(child) => child,
                    Err(error) => {
                        let _ = reply.send(Err(error.into()));
                        continue;
                    }
                };
                let _ = reply.send(Ok(()));
                match level(store, &mut child, requests, depth + 1, idle) {
                    End::Release(reply, true) => {
                        let _ = reply.send(child.commit().map_err(EngineError::from));
                    }
                    End::Release(reply, false) => {
                        drop(child);
                        let _ = reply.send(Err(EngineError::TransactionAborted));
                    }
                    End::RollbackTo(reply) => {
                        drop(child);
                        let _ = reply.send(Ok(()));
                    }
                    other => return other,
                }
            }
            Request::Release(reply) if depth == 0 => {
                let _ = reply.send(Err(EngineError::NoSavepoint));
            }
            Request::Release(reply) => return End::Release(reply, !rollback_only),
            Request::RollbackTo(reply) if depth == 0 => {
                let _ = reply.send(Err(EngineError::NoSavepoint));
            }
            Request::RollbackTo(reply) => return End::RollbackTo(reply),
            Request::Commit(reply) if depth > 0 => {
                let _ = reply.send(Err(EngineError::SavepointOpen));
            }
            Request::Commit(reply) => return End::Commit(reply, !rollback_only),
            Request::Rollback(reply) => return End::Rollback(reply),
        }
    }
}

/// Commits or aborts the root transaction. Returns whether it expired.
fn finish(store: &Store, txn: RwTransaction<'_>, end: End) -> bool {
    match end {
        End::Commit(reply, true) => {
            let result = txn.commit().map_err(EngineError::from);
            if result.as_ref().is_err_and(EngineError::is_map_full) {
                store.request_growth();
            }
            let _ = reply.send(result);
            false
        }
        End::Commit(reply, false) => {
            drop(txn);
            let _ = reply.send(Err(EngineError::TransactionAborted));
            false
        }
        End::Rollback(reply) => {
            drop(txn);
            let _ = reply.send(Ok(()));
            false
        }
        End::Release(reply, _) | End::RollbackTo(reply) => {
            drop(txn);
            let _ = reply.send(Err(EngineError::NoSavepoint));
            false
        }
        End::Expired => true,
        End::Disconnected => {
            info!("Transaction abandoned by its caller; rolled back");
            false
        }
    }
}

/// Serves a read-only snapshot. Returns whether it expired.
fn read_loop(
    store: &Store,
    txn: &RoTransaction<'_>,
    requests: &Receiver<Request>,
    idle: Duration,
) -> bool {
    loop {
        let request = match requests.recv_timeout(idle) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) => return false,
        };
        match request {
            Request::Execute(statement, reply) => {
                let _ = reply.send(exec::execute_read(store, txn, &statement));
            }
            Request::Savepoint(reply) | Request::Release(reply) | Request::RollbackTo(reply) => {
                let _ = reply.send(Err(EngineError::ReadOnlyTransaction));
            }
            Request::Commit(reply) | Request::Rollback(reply) => {
                let _ = reply.send(Ok(()));
                return false;
            }
        }
    }
}

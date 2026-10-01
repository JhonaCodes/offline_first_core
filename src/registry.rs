//! Process-wide registry of the databases opened through the C ABI.
//!
//! LMDB must not open the same environment twice in one process (`lmdb.h`,
//! "Caveats": *do not have open an LMDB database twice in the same process at
//! the same time*); [`Store`](crate::engine::Store) refuses a second open with
//! `AlreadyOpen`. Foreign callers, however, legitimately call `create_db` again for a path
//! they already opened: several Dart isolates, or a Flutter hot restart that
//! loses its pointer without closing it.
//!
//! This registry maps the canonical path of every open database to the
//! [`SharedDb`] serving it, so every `create_db` on that path shares a single
//! environment. It only holds [`Weak`] references: the database closes when
//! the last [`DbHandle`](crate::DbHandle) is released.
//!
//! # Locking
//!
//! - The registry mutex is held while a database is opened, released or
//!   reset. Dropping the last strong reference (which closes the environment)
//!   happens under it, so a concurrent `create_db` never meets an environment
//!   that is half closed.
//! - [`SharedDb`] keeps the [`AppDbState`] behind a [`RwLock`]. Record
//!   operations take the read lock: LMDB serializes write transactions itself
//!   on its writer mutex (`mdb_txn_begin` → `LOCK_MUTEX(env->me_wmutex)`), and
//!   natdb's `Environment` is `Send + Sync`. Resets, table definitions and
//!   map growth take the write lock (they must not run next to any other
//!   transaction of the process), so no
//!   transaction, and therefore no borrow of the environment, is alive while
//!   it closes and replaces it.
//! - Lock order: registry mutex, then the database lock. Record operations
//!   never touch the registry, so the order cannot be inverted.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak,
};

use log::info;
use thiserror::Error;

use crate::engine::{EngineError, EngineResult, OpenOptions, Store};
use crate::local_db_state::{db_dir_name, AppDbState, DbError};

type Registry = HashMap<PathBuf, Weak<SharedDb>>;

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

/// Errors of [`reset`].
#[derive(Debug, Error)]
pub(crate) enum ResetError {
    /// The target path is served by another database of this process.
    #[error("database `{}` is already open by another handle", .0.display())]
    AlreadyOpen(PathBuf),

    /// The storage layer failed.
    #[error(transparent)]
    Db(#[from] DbError),
}

/// One open database, shared by every handle opened on its path.
pub(crate) struct SharedDb {
    state: RwLock<AppDbState>,
}

impl SharedDb {
    /// Shared access, for record operations.
    ///
    /// Only a panic under the write lock (during a reset) poisons the lock.
    /// The state stays valid at every point of a reset (the environment is
    /// either open or marked closed, see [`DbError::Closed`]), and the panic
    /// itself is reported by the FFI guard, so a poisoned lock is recovered
    /// instead of making the database unusable for good.
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, AppDbState> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Exclusive access, for a reset. Poisoning is handled as in [`Self::read`].
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, AppDbState> {
        let state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        #[cfg(feature = "fault-injection")]
        crate::fault_injection::trip_in_lock(crate::fault_injection::LockSite::Database);
        state
    }

    /// Runs `op` on the store under the shared lock. When it fails because
    /// the memory map is full, grows the map under the exclusive lock and runs
    /// it again, until it succeeds or the map reached its maximum size.
    ///
    /// `op` must be retryable: every caller runs a whole transaction in it,
    /// which LMDB aborts when the map is full.
    pub(crate) fn run<R>(&self, mut op: impl FnMut(&Store) -> EngineResult<R>) -> EngineResult<R> {
        self.grow_if_requested()?;
        loop {
            let result = {
                let state = self.read();
                op(state.store()?)
            };
            match result {
                Err(error) if error.is_map_full() => {
                    if !self.exclusive(Store::grow)? {
                        return Err(error);
                    }
                }
                other => return other,
            }
        }
    }

    /// Runs `op` once under the shared lock (for callbacks that cannot be
    /// replayed). A full map is grown before the next operation.
    pub(crate) fn run_once<R>(
        &self,
        op: impl FnOnce(&Store) -> EngineResult<R>,
    ) -> EngineResult<R> {
        self.grow_if_requested()?;
        let result = {
            let state = self.read();
            op(state.store()?)
        };
        if result.as_ref().is_err_and(EngineError::is_map_full) {
            self.exclusive(Store::grow)?;
        }
        result
    }

    /// Runs `op` on the store under the exclusive lock: no other transaction
    /// of this process runs meanwhile (table definitions, map growth).
    pub(crate) fn exclusive<R>(
        &self,
        op: impl FnOnce(&Store) -> EngineResult<R>,
    ) -> EngineResult<R> {
        let state = self.write();
        op(state.store()?)
    }

    /// Grows the map if a transaction asked for it after hitting its limit.
    pub(crate) fn grow_if_requested(&self) -> EngineResult<()> {
        let requested = self.read().store().is_ok_and(Store::growth_requested);
        if requested {
            self.exclusive(Store::grow)?;
        }
        Ok(())
    }

    /// Runs a legacy operation, growing the map and retrying it when full.
    pub(crate) fn run_legacy<R>(
        &self,
        mut op: impl FnMut(&AppDbState) -> Result<R, DbError>,
    ) -> Result<R, DbError> {
        loop {
            let result = op(&self.read());
            match result {
                Err(error) if error.is_map_full() => match self.exclusive(Store::grow) {
                    Ok(true) => {}
                    Ok(false) | Err(_) => return Err(error),
                },
                other => return other,
            }
        }
    }
}

/// Locks the registry.
///
/// A poisoned lock is recovered: each critical section changes the map with
/// single inserts and removals of `Weak` entries, so it is consistent even if
/// a panic (for example while closing an environment) interrupted one.
fn lock_registry() -> MutexGuard<'static, Registry> {
    let registry = REGISTRY
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    #[cfg(feature = "fault-injection")]
    crate::fault_injection::trip_in_lock(crate::fault_injection::LockSite::Registry);
    registry
}

/// Returns the database served at `name` (see [`db_dir_name`]), opening it
/// if no handle of this process has it open.
pub(crate) fn acquire(name: &str) -> Result<Arc<SharedDb>, DbError> {
    acquire_dir(Path::new(&db_dir_name(name)), &OpenOptions::default()).map_err(DbError::from)
}

/// Returns the database served from the directory `dir`, opening it with
/// `options` unless the process already has it open.
pub(crate) fn acquire_dir(dir: &Path, options: &OpenOptions) -> EngineResult<Arc<SharedDb>> {
    let mut registry = lock_registry();

    // A path can only be canonicalized once it exists; opening would create
    // the directory anyway.
    fs::create_dir_all(dir)?;
    let key = dir.canonicalize()?;

    if let Some(shared) = registry.get(&key).and_then(Weak::upgrade) {
        info!("Sharing the database already open at {}", key.display());
        return Ok(shared);
    }

    let dir = dir.to_str().ok_or_else(|| {
        EngineError::InvalidRequest(format!("path `{}` is not valid UTF-8", dir.display()))
    })?;
    let state = AppDbState::open_dir(dir, options).map_err(|error| match error {
        DbError::Engine(error) => error,
        DbError::Io(error) => EngineError::Io(error),
        DbError::Storage(error) => EngineError::from(error),
        other => EngineError::InvalidRequest(other.to_string()),
    })?;
    let shared = Arc::new(SharedDb {
        state: RwLock::new(state),
    });
    registry.insert(key, Arc::downgrade(&shared));
    Ok(shared)
}

/// Releases one strong reference.
///
/// If it is the last one, the environment closes here, while the registry is
/// locked, and its entry is removed.
pub(crate) fn release(shared: Arc<SharedDb>) {
    let mut registry = lock_registry();
    drop(shared);
    registry.retain(|_, entry| entry.strong_count() > 0);
}

/// Resets `shared` onto the database `name` and moves its registry entry
/// there.
///
/// Fails with [`ResetError::AlreadyOpen`], leaving everything unchanged, when
/// another database of this process is open at `name`. Other failures follow
/// [`AppDbState::reset_database`]: the database is left closed, and it is no
/// longer registered under any path.
pub(crate) fn reset(shared: &Arc<SharedDb>, name: &str) -> Result<(), ResetError> {
    let dir = PathBuf::from(db_dir_name(name));
    let mut registry = lock_registry();

    // A path that does not exist yet cannot be open.
    if dir.exists() {
        let key = dir.canonicalize().map_err(DbError::from)?;
        let open_elsewhere = registry
            .get(&key)
            .and_then(Weak::upgrade)
            .is_some_and(|other| !Arc::ptr_eq(&other, shared));
        if open_elsewhere {
            return Err(ResetError::AlreadyOpen(key));
        }
    }

    let mut state = shared.write();
    // The reset gives up the current path whatever its outcome.
    registry.retain(|_, entry| !ptr::eq(entry.as_ptr(), Arc::as_ptr(shared)));
    state.reset_database(name)?;

    let key = dir.canonicalize().map_err(DbError::from)?;
    info!("Database reset onto {}", key.display());
    registry.insert(key, Arc::downgrade(shared));
    Ok(())
}

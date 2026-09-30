//! The opaque database handle handed to foreign callers.

use std::mem::ManuallyDrop;
use std::sync::{Arc, RwLockReadGuard};

use std::path::Path;

use crate::engine::{EngineResult, OpenOptions};
use crate::local_db_state::{AppDbState, DbError};
use crate::registry::{self, ResetError, SharedDb};

/// Opaque handle to an open database, returned by [`create_db`](crate::create_db).
///
/// Every `create_db` call returns its own handle; handles opened on the same
/// path share one database (see the `registry` module). A handle is released
/// with [`close_database`](crate::close_database), and the database closes
/// with its last handle.
///
/// Foreign code only ever holds a pointer to it and must treat it as opaque.
/// A handle may be used from several threads at once: the entry points only
/// take shared references to it.
pub struct DbHandle {
    /// In `ManuallyDrop` so that `Drop` can hand the reference over to the
    /// registry, which releases it under its lock.
    shared: ManuallyDrop<Arc<SharedDb>>,
}

impl DbHandle {
    /// Opens the database `name`, sharing it if it is already open.
    pub(crate) fn open(name: &str) -> Result<Self, DbError> {
        Ok(Self {
            shared: ManuallyDrop::new(registry::acquire(name)?),
        })
    }

    /// Shared access to the database, for record operations.
    /// Opens the directory `dir` with `options` (the path is used as is).
    pub(crate) fn open_dir(dir: &Path, options: &OpenOptions) -> EngineResult<Self> {
        Ok(Self {
            shared: ManuallyDrop::new(registry::acquire_dir(dir, options)?),
        })
    }

    /// The database shared by every handle on this path.
    pub(crate) fn shared(&self) -> &Arc<SharedDb> {
        &self.shared
    }

    /// Runs a legacy write, growing the memory map and retrying when full.
    pub(crate) fn legacy<R>(
        &self,
        op: impl FnMut(&AppDbState) -> Result<R, DbError>,
    ) -> Result<R, DbError> {
        self.shared.run_legacy(op)
    }

    pub(crate) fn db(&self) -> RwLockReadGuard<'_, AppDbState> {
        self.shared.read()
    }

    /// Resets the shared database onto `name`, for every handle sharing it.
    pub(crate) fn reset(&self, name: &str) -> Result<(), ResetError> {
        registry::reset(&self.shared, name)
    }
}

impl Drop for DbHandle {
    fn drop(&mut self) {
        // SAFETY: `drop` runs exactly once and `self.shared` is never used
        // after being taken here.
        let shared = unsafe { ManuallyDrop::take(&mut self.shared) };
        registry::release(shared);
    }
}

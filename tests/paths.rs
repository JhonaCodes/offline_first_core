//! Database paths handed to `create_db` are used exactly as given.
//!
//! The test changes the process working directory, which is shared by every
//! thread, so this binary holds a single test.

mod common;

use std::env;
use std::fs;
use std::path::PathBuf;

use common::{c_string, FfiDb, TestDir};
use offline_first_core::create_db;

/// Restores the working directory when dropped, even if an assertion fails.
struct CwdGuard {
    original: PathBuf,
}

impl CwdGuard {
    fn enter(dir: &TestDir) -> Self {
        let original = env::current_dir().expect("the working directory must be readable");
        env::set_current_dir(dir.path()).expect("failed to change the working directory");
        Self { original }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        // Best effort: a failure here must not mask the test outcome.
        let _ = env::set_current_dir(&self.original);
    }
}

#[test]
fn test_ffi_create_db_absolute_path_ignores_working_directory() {
    let cwd = TestDir::new("paths_cwd");
    let target = TestDir::new("paths_target");
    let _cwd_guard = CwdGuard::enter(&cwd);
    let name = c_string(&target.db_name("db"));

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let db = FfiDb::adopt(unsafe { create_db(name.as_ptr()) });

    assert!(!db.ptr().is_null(), "create_db returned a null pointer");
    assert!(
        target.db_dir("db").is_dir(),
        "the database must be created exactly at `<absolute path>.lmdb`"
    );
    let stray_entries: Vec<_> = fs::read_dir(cwd.path())
        .expect("the working directory must be listable")
        .collect();
    assert!(
        stray_entries.is_empty(),
        "nothing may be created under the working directory, found {stray_entries:?}"
    );
}

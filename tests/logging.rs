//! `create_db` logs what actually happened: it never reports a database as
//! initialized when opening it failed.
//!
//! This binary installs a capturing logger. Records are tagged with the thread
//! that emitted them, so tests running in parallel only look at their own.

mod common;

use std::fs;
use std::sync::{Mutex, Once, PoisonError};
use std::thread::{self, ThreadId};

use common::{c_string, FfiDb, TestDir};
use log::{set_logger, set_max_level, Level, LevelFilter, Log, Metadata, Record};
use offline_first_core::create_db;

type Captured = (ThreadId, Level, String);

struct CaptureLogger {
    records: Mutex<Vec<Captured>>,
}

impl Log for CaptureLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((
                thread::current().id(),
                record.level(),
                record.args().to_string(),
            ));
    }

    fn flush(&self) {}
}

static LOGGER: CaptureLogger = CaptureLogger {
    records: Mutex::new(Vec::new()),
};
static INSTALL: Once = Once::new();

fn install_logger() {
    INSTALL.call_once(|| {
        set_logger(&LOGGER).expect("no other logger may be installed in this binary");
        set_max_level(LevelFilter::Trace);
    });
}

/// Records emitted so far by the calling thread.
fn logs_of_this_thread() -> Vec<(Level, String)> {
    let me = thread::current().id();
    LOGGER
        .records
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|(thread, _, _)| *thread == me)
        .map(|(_, level, message)| (*level, message.clone()))
        .collect()
}

#[test]
fn test_create_db_failure_is_not_logged_as_initialized() {
    install_logger();
    let dir = TestDir::new("log_failure");
    // A regular file where the database directory should go
    fs::write(dir.db_dir("db"), b"not a directory").expect("failed to create the blocker");
    let name = c_string(&dir.db_name("db"));

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let db = FfiDb::adopt(unsafe { create_db(name.as_ptr()) });

    assert!(db.ptr().is_null(), "opening over a regular file must fail");
    let logs = logs_of_this_thread();
    assert!(
        !logs
            .iter()
            .any(|(_, message)| message.contains("initialized")),
        "a failed create_db must not be logged as initialized: {logs:?}"
    );
    assert!(
        logs.iter()
            .any(|(level, message)| *level == Level::Warn && message.contains("Failed")),
        "a failed create_db must log a warning: {logs:?}"
    );
}

#[test]
fn test_create_db_success_is_logged_as_initialized() {
    install_logger();
    let dir = TestDir::new("log_success");
    let name = c_string(&dir.db_name("db"));

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let db = FfiDb::adopt(unsafe { create_db(name.as_ptr()) });

    assert!(!db.ptr().is_null());
    let logs = logs_of_this_thread();
    assert!(
        logs.iter()
            .any(|(level, message)| *level == Level::Info
                && message.starts_with("Database initialized")),
        "a successful create_db must be logged: {logs:?}"
    );
}

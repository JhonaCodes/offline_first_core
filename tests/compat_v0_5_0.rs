//! Databases written by offline_first_core 0.5.0 are detected, not damaged.
//!
//! 0.5.0 stored its data with LMDB 0.9; this version runs on LMDB 1.0.2, whose
//! on-disk format is incompatible ("versions 0.9 and 1.0 are mutually
//! incompatible", LMDB `upgrading.doc`). Opening such a database must fail
//! with the typed `LegacyFormat` error and leave its data file untouched, so
//! the application can migrate it (the Dart SDK exports with 1.x and imports
//! with 2.x).
//!
//! The fixture under `tests/fixtures/v0_5_0/` was produced by
//! `examples/gen_fixture_v0_5_0.rs` on the 0.5.0 storage layer. It is copied
//! to a per-test directory before opening, so the committed files are never
//! modified.

mod common;

use std::ffi::{c_char, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;

use common::TestDir;
use offline_first_core::engine::EngineError;
use offline_first_core::local_db_state::{AppDbState, DbError};
use offline_first_core::{create_db, ofc_free_string, ofc_open, DbHandle};
use serde_json::Value;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/v0_5_0");
const FIXTURE_DB: &str = "compat";

/// Copies the committed fixture into `dir` and returns the copy's data file.
fn copy_fixture(dir: &TestDir) -> PathBuf {
    let source = Path::new(FIXTURE_DIR).join(format!("{FIXTURE_DB}.lmdb"));
    let target = dir.db_dir(FIXTURE_DB);
    fs::create_dir_all(&target).expect("failed to create the fixture copy directory");
    for file in ["data.mdb", "lock.mdb"] {
        fs::copy(source.join(file), target.join(file)).expect("failed to copy a fixture file");
    }
    target.join("data.mdb")
}

#[test]
fn test_v0_5_0_database_is_reported_as_legacy_format() {
    let dir = TestDir::new("compat_v0_5_0_rust");
    let data = copy_fixture(&dir);
    let before = fs::read(&data).expect("read data.mdb");

    let opened = AppDbState::init(dir.db_name(FIXTURE_DB));

    assert!(
        matches!(opened, Err(DbError::Engine(EngineError::LegacyFormat))),
        "expected LegacyFormat, got {:?}",
        opened.err()
    );
    assert_eq!(
        fs::read(&data).expect("read data.mdb"),
        before,
        "data.mdb must stay byte-identical"
    );
}

#[test]
fn test_v0_5_0_database_through_ofc_open() {
    let dir = TestDir::new("compat_v0_5_0_ffi");
    let data = copy_fixture(&dir);
    let before = fs::read(&data).expect("read data.mdb");
    let path = CString::new(dir.db_name(FIXTURE_DB)).expect("path without NUL");
    let mut handle: *mut DbHandle = ptr::dangling_mut();

    // SAFETY: `path` is a live CString and `handle` is writable.
    let response = unsafe { ofc_open(path.as_ptr(), ptr::null(), &mut handle) };
    // SAFETY: `response` was returned by `ofc_open` and is released once below.
    let text = unsafe { std::ffi::CStr::from_ptr(response) }
        .to_str()
        .expect("UTF-8")
        .to_owned();
    // SAFETY: as above.
    unsafe { ofc_free_string(response.cast_mut()) };
    let json: Value = serde_json::from_str(&text).expect("JSON response");

    assert_eq!(json["error"]["code"], "LegacyFormat", "{text}");
    assert!(handle.is_null(), "no handle on failure");
    assert_eq!(
        fs::read(&data).expect("read data.mdb"),
        before,
        "data.mdb must stay byte-identical"
    );

    // The legacy entry point cannot report the reason: it returns null.
    // SAFETY: `path` is a live CString.
    let legacy: *mut DbHandle = unsafe { create_db(path.as_ptr() as *const c_char) };
    assert!(legacy.is_null());
}

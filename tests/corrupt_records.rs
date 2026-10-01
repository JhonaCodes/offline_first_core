//! A stored record that cannot be decoded is reported with its id, never
//! skipped: a listing that leaves a record out must not look complete
//! (RFC §9.6). The id is what lets the caller delete or repair the record.

mod common;

use std::fs;
use std::path::Path;

use common::{create_test_model, FfiDb, FfiResponse, TestDir};
use natdb::{DatabaseFlags, Environment, Transaction, WriteFlags};
use offline_first_core::get_all;
use offline_first_core::local_db_state::{AppDbState, DbError};

/// Writes `value` under `id` straight into the key-value database (`main`)
/// of the environment directory `dir`, as a damaged file or a foreign
/// writer would leave it. The environment is closed again on return.
fn seed_raw(dir: &Path, id: &str, value: &[u8]) {
    fs::create_dir_all(dir).expect("failed to create the environment directory");
    let env = Environment::new()
        .set_max_dbs(4)
        .open(dir)
        .expect("failed to open the environment");
    let db = env
        .create_db(Some("main"), DatabaseFlags::empty())
        .expect("failed to create the key-value database");
    let mut txn = env.begin_rw_txn().expect("failed to begin");
    txn.put(db, &id, &value, WriteFlags::empty())
        .expect("failed to write the raw record");
    txn.commit().expect("failed to commit");
}

/// A database holding one healthy record and the raw record `id` = `value`.
fn with_raw_record(dir: &TestDir, id: &str, value: &[u8]) -> AppDbState {
    seed_raw(&dir.db_dir("db"), id, value);
    let state = AppDbState::init(dir.db_name("db")).expect("open must succeed");
    state
        .push(create_test_model("healthy", None))
        .expect("push must succeed");
    state
}

#[test]
fn a_record_that_is_not_json_fails_the_listing_with_its_id() {
    let dir = TestDir::new("corrupt_json");
    let state = with_raw_record(&dir, "broken", b"{not json");

    match state.get() {
        Err(DbError::Deserialization { id, .. }) => assert_eq!(id, "broken"),
        other => panic!("the corrupt record must be reported, got {other:?}"),
    }
}

#[test]
fn a_record_that_is_not_utf8_fails_the_listing_with_its_id() {
    let dir = TestDir::new("corrupt_utf8");
    let state = with_raw_record(&dir, "binary", &[0xff, 0xfe, 0xfd]);

    match state.get() {
        Err(DbError::Utf8 { id, .. }) => assert_eq!(id, "binary"),
        other => panic!("the corrupt record must be reported, got {other:?}"),
    }
}

#[test]
fn get_all_over_the_c_abi_names_the_corrupt_record() {
    let dir = TestDir::new("corrupt_ffi");
    seed_raw(&dir.db_dir("db"), "broken", b"{not json");
    let db = FfiDb::open(&dir, "db");
    assert_eq!(db.push("healthy").variant, "Ok");

    // SAFETY: `db.ptr()` is a live handle returned by `create_db`.
    let response = FfiResponse::take(unsafe { get_all(db.ptr()) });

    assert_eq!(response.variant, "DatabaseError", "{response:?}");
    assert!(
        response.payload.contains("`broken`"),
        "the error must name the record: {response:?}"
    );
}

#[test]
fn deleting_the_reported_record_makes_the_listing_complete_again() {
    let dir = TestDir::new("corrupt_recovery");
    let state = with_raw_record(&dir, "broken", b"{not json");
    assert!(state.get().is_err());

    assert!(state.delete_by_id("broken").expect("delete must succeed"));

    let ids: Vec<String> = state
        .get()
        .expect("the listing is complete again")
        .into_iter()
        .map(|model| model.id)
        .collect();
    assert_eq!(ids, ["healthy"]);
}

//! A panic inside an entry point must never cross the C ABI: it is reported
//! as an error instead of aborting the process.
//!
//! No public API can make an entry point panic, so these tests use the
//! test-only `fault-injection` feature (declared as required by this target in
//! `Cargo.toml`): `ofc_fault_injection_arm` makes the next entry point called
//! on the same thread panic inside its guard, before its body runs. Run with
//! `cargo test --features fault-injection --test panic_boundary`.

mod common;

use std::ffi::c_char;

use common::{c_string, FfiDb, FfiResponse, TestDir};
use offline_first_core::{
    clear_all_records, close_database, create_db, delete_by_id, get_all, get_by_id,
    ofc_fault_injection_arm, ofc_free_string, push_data, reset_database, update_data,
};

/// Requires the error envelope produced for a panic in `entry_point`.
fn assert_contained(entry_point: &str, response: &FfiResponse) {
    assert_eq!(
        response.variant, "DatabaseError",
        "{entry_point}: {response:?}"
    );
    assert_eq!(
        response.payload,
        format!("internal panic in {entry_point}: fault injected in {entry_point}")
    );
}

#[test]
fn test_panic_in_response_entry_points_becomes_database_error() {
    let dir = TestDir::new("panic_responses");
    let db = FfiDb::open(&dir, "db");
    assert_eq!(db.push("record").variant, "Ok");
    let json = c_string(r#"{"id":"record","hash":"h","data":{}}"#);
    let id = c_string("record");
    let new_name = c_string(&dir.db_name("reset"));

    type EntryPoint<'a> = &'a dyn Fn() -> *const c_char;
    let calls: [(&str, EntryPoint); 7] = [
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        ("push_data", &|| unsafe {
            push_data(db.ptr(), json.as_ptr())
        }),
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        ("get_by_id", &|| unsafe { get_by_id(db.ptr(), id.as_ptr()) }),
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        ("get_all", &|| unsafe { get_all(db.ptr()) }),
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        ("update_data", &|| unsafe {
            update_data(db.ptr(), json.as_ptr())
        }),
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        ("delete_by_id", &|| unsafe {
            delete_by_id(db.ptr(), id.as_ptr())
        }),
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        ("clear_all_records", &|| unsafe {
            clear_all_records(db.ptr())
        }),
        ("reset_database", &|| {
            // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
            unsafe { reset_database(db.ptr(), new_name.as_ptr()) }
        }),
    ];

    for (entry_point, call) in calls {
        ofc_fault_injection_arm();
        assert_contained(entry_point, &FfiResponse::take(call()));
    }

    // Every fault fired before a body ran: the database is intact and usable.
    assert_eq!(db.ids(), ["record"]);
}

#[test]
fn test_panic_in_close_database_becomes_database_error() {
    let dir = TestDir::new("panic_close");
    let db = FfiDb::open(&dir, "db");
    let handle = db.into_raw();

    ofc_fault_injection_arm();
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { close_database(handle) });

    assert_contained("close_database", &response);
    // The pointer is invalid after `close_database` whatever the response;
    // the fault fired before the handle was released, so it is leaked here.
}

#[test]
fn test_panic_in_create_db_returns_null() {
    let dir = TestDir::new("panic_create");
    let name = c_string(&dir.db_name("db"));

    ofc_fault_injection_arm();
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let failed = FfiDb::adopt(unsafe { create_db(name.as_ptr()) });

    assert!(
        failed.ptr().is_null(),
        "a panic in create_db must yield null"
    );
    // The fault is consumed: the next call on this thread succeeds.
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let db = FfiDb::adopt(unsafe { create_db(name.as_ptr()) });
    assert!(!db.ptr().is_null());
}

#[test]
fn test_panic_in_free_string_returns() {
    let dir = TestDir::new("panic_free");
    let db = FfiDb::open(&dir, "db");
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = unsafe { get_all(db.ptr()) };

    ofc_fault_injection_arm();
    // SAFETY: `response` was returned by `get_all` and not released yet.
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    unsafe { ofc_free_string(response.cast_mut()) };

    // The call returned instead of aborting. The fault fired before the
    // release, so the string is still valid: decode and release it for real.
    let listed = FfiResponse::take(response);
    assert_eq!(listed.variant, "Ok");
}

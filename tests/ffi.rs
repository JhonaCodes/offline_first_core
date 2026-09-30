//! The C ABI exposed to Flutter: every `extern "C"` entry point, on success
//! and on invalid input. Responses are decoded from their JSON envelope.

mod common;

use std::ffi::c_char;
use std::ptr;

use common::{c_string, FfiDb, FfiResponse, TestDir};
use offline_first_core::{
    clear_all_records, close_database, create_db, delete_by_id, get_all, get_by_id,
    ofc_free_string, push_data, reset_database, update_data,
};
use serde_json::json;

/// Pushes `json` through `push_data` and requires an `Ok` response.
fn push_ok(db: &FfiDb, json: &str) {
    let json = c_string(json);
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { push_data(db.ptr(), json.as_ptr()) });
    assert_eq!(response.variant, "Ok", "setup push failed: {response:?}");
}

#[test]
fn test_ffi_create_db_success() {
    let dir = TestDir::new("ffi_create_db");
    let name = c_string(&dir.relative_db_name("db"));

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let db = FfiDb::adopt(unsafe { create_db(name.as_ptr()) });

    assert!(!db.ptr().is_null(), "Database pointer should not be null");
    assert!(
        dir.db_dir("db").is_dir(),
        "create_db must create the .lmdb directory at the requested path"
    );
}

#[test]
fn test_ffi_create_db_null_pointer() {
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let db = FfiDb::adopt(unsafe { create_db(ptr::null()) });
    assert!(db.ptr().is_null(), "Should return null for null input");
}

#[test]
fn test_ffi_create_db_invalid_utf8() {
    // Invalid UTF-8 sequence followed by the NUL terminator
    let invalid_bytes: [u8; 4] = [0xFF, 0xFE, 0xFD, 0x00];

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let db = FfiDb::adopt(unsafe { create_db(invalid_bytes.as_ptr().cast::<c_char>()) });

    assert!(db.ptr().is_null(), "Should return null for invalid UTF-8");
}

#[test]
fn test_ffi_push_data_success() {
    let dir = TestDir::new("ffi_push");
    let db = FfiDb::open(&dir, "db");
    let json = c_string(r#"{"id":"test1","hash":"hash1","data":{"key":"value"}}"#);

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { push_data(db.ptr(), json.as_ptr()) });

    assert_eq!(response.variant, "Ok", "Should be a success response");
    let model = response.model();
    assert_eq!(model.id, "test1");
    assert_eq!(model.data, json!({"key": "value"}));
}

#[test]
fn test_ffi_push_data_null_pointers() {
    let dir = TestDir::new("ffi_push_null");
    let db = FfiDb::open(&dir, "db");

    // Null state pointer
    let json = c_string(r#"{"id":"test1","hash":"hash1","data":{}}"#);
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { push_data(ptr::null_mut(), json.as_ptr()) });
    assert_eq!(response.variant, "BadRequest");

    // Null JSON pointer
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { push_data(db.ptr(), ptr::null()) });
    assert_eq!(response.variant, "BadRequest");
}

#[test]
fn test_ffi_push_data_invalid_json() {
    let dir = TestDir::new("ffi_push_invalid");
    let db = FfiDb::open(&dir, "db");
    let invalid_json = c_string(r#"{"invalid": json structure"#);

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { push_data(db.ptr(), invalid_json.as_ptr()) });

    assert_eq!(response.variant, "SerializationError");
}

#[test]
fn test_ffi_get_by_id_success() {
    let dir = TestDir::new("ffi_get");
    let db = FfiDb::open(&dir, "db");
    push_ok(
        &db,
        r#"{"id":"test1","hash":"hash1","data":{"key":"value"}}"#,
    );
    let id = c_string("test1");

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { get_by_id(db.ptr(), id.as_ptr()) });

    assert_eq!(response.variant, "Ok");
    let model = response.model();
    assert_eq!(model.id, "test1");
    assert_eq!(model.data, json!({"key": "value"}));
}

#[test]
fn test_ffi_get_by_id_not_found() {
    let dir = TestDir::new("ffi_get_not_found");
    let db = FfiDb::open(&dir, "db");
    let id = c_string("nonexistent");

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { get_by_id(db.ptr(), id.as_ptr()) });

    assert_eq!(response.variant, "NotFound");
}

#[test]
fn test_ffi_get_by_id_null_pointers() {
    let dir = TestDir::new("ffi_get_null");
    let db = FfiDb::open(&dir, "db");

    // Null state pointer
    let id = c_string("test1");
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { get_by_id(ptr::null_mut(), id.as_ptr()) });
    assert_eq!(response.variant, "BadRequest");

    // Null id pointer
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { get_by_id(db.ptr(), ptr::null()) });
    assert_eq!(response.variant, "BadRequest");
}

#[test]
fn test_ffi_get_all_success() {
    let dir = TestDir::new("ffi_get_all");
    let db = FfiDb::open(&dir, "db");
    for i in 1..=3 {
        push_ok(
            &db,
            &format!(r#"{{"id":"test{i}","hash":"hash{i}","data":{{"number":{i}}}}}"#),
        );
    }

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { get_all(db.ptr()) });

    assert_eq!(response.variant, "Ok");
    let mut ids: Vec<String> = response.models().into_iter().map(|m| m.id).collect();
    ids.sort();
    assert_eq!(ids, ["test1", "test2", "test3"]);
}

#[test]
fn test_ffi_get_all_null_pointer() {
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { get_all(ptr::null_mut()) });
    assert_eq!(response.variant, "BadRequest");
}

#[test]
fn test_ffi_update_data_success() {
    let dir = TestDir::new("ffi_update");
    let db = FfiDb::open(&dir, "db");
    push_ok(&db, r#"{"id":"test1","hash":"hash1","data":{"value":1}}"#);
    let updated_json = c_string(r#"{"id":"test1","hash":"hash2","data":{"value":2}}"#);

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { update_data(db.ptr(), updated_json.as_ptr()) });

    assert_eq!(response.variant, "Ok");
    let model = response.model();
    assert_eq!(model.hash, "hash2");
    assert_eq!(model.data, json!({"value": 2}));
}

#[test]
fn test_ffi_update_data_not_found() {
    let dir = TestDir::new("ffi_update_not_found");
    let db = FfiDb::open(&dir, "db");
    let json = c_string(r#"{"id":"nonexistent","hash":"hash1","data":{"value":1}}"#);

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { update_data(db.ptr(), json.as_ptr()) });

    assert_eq!(response.variant, "NotFound");
}

#[test]
fn test_ffi_update_data_null_pointers() {
    let dir = TestDir::new("ffi_update_null");
    let db = FfiDb::open(&dir, "db");

    // Null state pointer
    let json = c_string(r#"{"id":"test1","hash":"hash1","data":{}}"#);
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { update_data(ptr::null_mut(), json.as_ptr()) });
    assert_eq!(response.variant, "BadRequest");

    // Null JSON pointer
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { update_data(db.ptr(), ptr::null()) });
    assert_eq!(response.variant, "BadRequest");
}

#[test]
fn test_ffi_delete_by_id_success() {
    let dir = TestDir::new("ffi_delete");
    let db = FfiDb::open(&dir, "db");
    push_ok(
        &db,
        r#"{"id":"test1","hash":"hash1","data":{"key":"value"}}"#,
    );
    let id = c_string("test1");

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { delete_by_id(db.ptr(), id.as_ptr()) });

    assert_eq!(response.variant, "Ok");
    assert!(response.payload.contains("successfully"));
}

#[test]
fn test_ffi_delete_by_id_not_found() {
    let dir = TestDir::new("ffi_delete_not_found");
    let db = FfiDb::open(&dir, "db");
    let id = c_string("nonexistent");

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { delete_by_id(db.ptr(), id.as_ptr()) });

    assert_eq!(response.variant, "NotFound");
}

#[test]
fn test_ffi_delete_by_id_null_pointers() {
    let dir = TestDir::new("ffi_delete_null");
    let db = FfiDb::open(&dir, "db");

    // Null state pointer
    let id = c_string("test1");
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { delete_by_id(ptr::null_mut(), id.as_ptr()) });
    assert_eq!(response.variant, "BadRequest");

    // Null id pointer
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { delete_by_id(db.ptr(), ptr::null()) });
    assert_eq!(response.variant, "BadRequest");
}

#[test]
fn test_ffi_clear_all_records_success() {
    let dir = TestDir::new("ffi_clear");
    let db = FfiDb::open(&dir, "db");
    for i in 1..=3 {
        push_ok(
            &db,
            &format!(r#"{{"id":"test{i}","hash":"hash{i}","data":{{"number":{i}}}}}"#),
        );
    }

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { clear_all_records(db.ptr()) });

    assert_eq!(response.variant, "Ok");
    assert!(response.payload.contains("cleared"));
}

#[test]
fn test_ffi_clear_all_records_null_pointer() {
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { clear_all_records(ptr::null_mut()) });
    assert_eq!(response.variant, "BadRequest");
}

#[test]
fn test_ffi_reset_database_success() {
    let dir = TestDir::new("ffi_reset");
    let db = FfiDb::open(&dir, "db");
    push_ok(
        &db,
        r#"{"id":"test1","hash":"hash1","data":{"key":"value"}}"#,
    );
    let new_name = c_string(&dir.relative_db_name("reset_new"));

    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { reset_database(db.ptr(), new_name.as_ptr()) });

    assert_eq!(response.variant, "Ok");
    assert!(response.payload.contains("reset successfully"));
}

#[test]
fn test_ffi_reset_database_null_pointers() {
    let dir = TestDir::new("ffi_reset_null");
    let db = FfiDb::open(&dir, "db");

    // Null state pointer
    let new_name = c_string(&dir.relative_db_name("new_db"));
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { reset_database(ptr::null_mut(), new_name.as_ptr()) });
    assert_eq!(response.variant, "BadRequest");

    // Null name pointer
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { reset_database(db.ptr(), ptr::null()) });
    assert_eq!(response.variant, "BadRequest");
}

#[test]
fn test_ffi_close_database_success() {
    let dir = TestDir::new("ffi_close");
    let db = FfiDb::open(&dir, "db");

    // `close_database` takes ownership of the handle: `close` hands it over
    // exactly once instead of leaving the guard to close it again on drop.
    let response = db.close();

    assert_eq!(response.variant, "Ok");
    assert!(response.payload.contains("closed successfully"));
}

#[test]
fn test_ffi_close_database_null_pointer() {
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let response = FfiResponse::take(unsafe { close_database(ptr::null_mut()) });
    assert_eq!(response.variant, "BadRequest");
}

#[test]
fn test_ffi_free_string_null_is_noop() {
    // SAFETY: null is explicitly allowed by `ofc_free_string`.
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    unsafe { ofc_free_string(ptr::null_mut()) };
}

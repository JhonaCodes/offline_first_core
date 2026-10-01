//! The C ABI v2 (RFC-001 §12.2, §19.3): `u64` handles validated against a
//! registry, buffers released by the library. A second close or release, a
//! use after close, a handle of the other kind or one never issued answer
//! `LDB_INVALID_HANDLE`, never undefined behaviour; a hot restart (handles
//! opened again on the same path without closing the old ones) shares one
//! database.

mod common;

use std::ffi::CString;
use std::mem::size_of;
use std::ptr;
use std::slice;
use std::sync::Arc;
use std::thread;

use common::{take_wire, TestDir};
use offline_first_core::abi_v2::{
    ldb_abi_version, ldb_buffer_release, ldb_buffer_view, ldb_close, ldb_execute, ldb_open,
    LdbBufferHandle, LdbHandle, LDB_ABI_VERSION, LDB_INVALID_HANDLE, LDB_INVALID_UTF8,
    LDB_MAX_REQUEST_BYTES, LDB_NULL_POINTER, LDB_OK, LDB_REQUEST_TOO_LARGE,
};
use offline_first_core::ofc_open;
use serde_json::{json, Value};

/// Opens `name` in `dir`; answers the handle and the open response.
fn open(dir: &TestDir, name: &str) -> (LdbHandle, Value) {
    let path = dir.db_name(name);
    let mut handle = 0;
    let mut response = 0;
    // SAFETY: `path` is live for the call; the outs are writable.
    let status = unsafe {
        ldb_open(
            path.as_ptr(),
            path.len(),
            ptr::null(),
            0,
            &mut handle,
            &mut response,
        )
    };
    assert_eq!(status, LDB_OK);
    (handle, take(response))
}

/// The bytes of `buffer` as JSON; releases it.
fn take(buffer: LdbBufferHandle) -> Value {
    let mut data = ptr::null();
    let mut len = 0;
    // SAFETY: the outs are writable.
    assert_eq!(
        unsafe { ldb_buffer_view(buffer, &mut data, &mut len) },
        LDB_OK
    );
    // SAFETY: `ldb_buffer_view` answered `len` readable bytes at `data`,
    // valid until the release below.
    let bytes = unsafe { slice::from_raw_parts(data, len) }.to_vec();
    assert_eq!(ldb_buffer_release(buffer), LDB_OK);
    serde_json::from_slice(&bytes).expect("a response is JSON")
}

/// Sends `request` on `handle`; answers the status and the response.
fn execute(handle: LdbHandle, request: &Value) -> (i32, Option<Value>) {
    let text = request.to_string();
    let mut response = 0;
    // SAFETY: `text` is live for the call; `response` is writable.
    let status = unsafe { ldb_execute(handle, text.as_ptr(), text.len(), &mut response) };
    let answer = (status == LDB_OK).then(|| take(response));
    (status, answer)
}

fn ok(handle: LdbHandle, request: Value) -> Value {
    match execute(handle, &request) {
        (LDB_OK, Some(answer)) => answer["ok"].clone(),
        other => panic!("{request} failed: {other:?}"),
    }
}

fn define_and_insert(handle: LdbHandle) {
    ok(
        handle,
        json!({"v": 1, "op": "define_table", "table": {"name": "notes", "primary_key": "id"}}),
    );
    ok(
        handle,
        json!({"v": 1, "op": "execute", "statement": {"op": "insert", "table": "notes",
            "rows": [{"id": "a", "text": "nul \u{0} inside"}]}}),
    );
}

fn count(handle: LdbHandle) -> Value {
    ok(
        handle,
        json!({"v": 1, "op": "execute", "statement": {"op": "count", "table": "notes"}}),
    )["count"]
        .clone()
}

#[test]
fn the_abi_reports_its_version_and_fixed_layout() {
    assert_eq!(ldb_abi_version(), LDB_ABI_VERSION);
    assert_eq!(LDB_ABI_VERSION, 2);
    assert_eq!(size_of::<LdbHandle>(), 8);
    assert_eq!(size_of::<LdbBufferHandle>(), 8);
}

#[test]
fn a_request_runs_and_its_response_is_a_buffer() {
    let dir = TestDir::new("abi_round_trip");
    let (handle, opened) = open(&dir, "db");
    assert_eq!(opened, json!({"v": 1, "ok": {}}));
    assert_ne!(handle, 0);

    define_and_insert(handle);
    let row = ok(
        handle,
        json!({"v": 1, "op": "execute", "statement": {"op": "find", "table": "notes", "key": "a"}}),
    );
    assert_eq!(
        row["row"]["text"], "nul \u{0} inside",
        "bytes with a length"
    );
    let error = execute(handle, &json!({"v": 1, "op": "nope"})).1;
    assert_eq!(error.expect("answer")["error"]["code"], "InvalidRequest");
    assert_eq!(ldb_close(handle), LDB_OK);
}

#[test]
fn closing_twice_and_using_after_close_answer_invalid_handle() {
    let dir = TestDir::new("abi_close");
    let (handle, _) = open(&dir, "db");
    define_and_insert(handle);

    assert_eq!(ldb_close(handle), LDB_OK);
    assert_eq!(ldb_close(handle), LDB_INVALID_HANDLE, "a second close");
    let (status, answer) = execute(handle, &json!({"v": 1, "op": "tables"}));
    assert_eq!(status, LDB_INVALID_HANDLE, "a use after close");
    assert!(answer.is_none());
}

#[test]
fn releasing_twice_and_foreign_handles_answer_invalid_handle() {
    let dir = TestDir::new("abi_release");
    let (handle, _) = open(&dir, "db");
    let request = json!({"v": 1, "op": "tables"}).to_string();
    let mut buffer = 0;
    // SAFETY: `request` is live; `buffer` is writable.
    let status = unsafe { ldb_execute(handle, request.as_ptr(), request.len(), &mut buffer) };
    assert_eq!(status, LDB_OK);

    assert_eq!(ldb_buffer_release(buffer), LDB_OK);
    assert_eq!(
        ldb_buffer_release(buffer),
        LDB_INVALID_HANDLE,
        "a second release"
    );
    let mut data = ptr::null();
    let mut len = 0;
    // SAFETY: the outs are writable.
    let viewed = unsafe { ldb_buffer_view(buffer, &mut data, &mut len) };
    assert_eq!(viewed, LDB_INVALID_HANDLE, "a view after release");
    assert!(data.is_null());

    // Kinds do not cross, and made-up handles are refused.
    assert_eq!(
        ldb_close(buffer),
        LDB_INVALID_HANDLE,
        "a buffer is no database"
    );
    assert_eq!(
        ldb_buffer_release(handle),
        LDB_INVALID_HANDLE,
        "a database is no buffer"
    );
    for made_up in [0, 1, u64::MAX, handle ^ 1] {
        assert_eq!(
            execute(made_up, &json!({"v": 1, "op": "tables"})).0,
            LDB_INVALID_HANDLE
        );
    }
    assert_eq!(ldb_close(handle), LDB_OK);
}

#[test]
fn bad_arguments_answer_a_status_without_running() {
    let dir = TestDir::new("abi_arguments");
    let (handle, _) = open(&dir, "db");
    let mut response = 0;

    // SAFETY: null pointers are what is under test; `response` is writable.
    unsafe {
        assert_eq!(
            ldb_execute(handle, ptr::null(), 4, &mut response),
            LDB_NULL_POINTER
        );
        assert_eq!(
            ldb_execute(handle, b"{}".as_ptr(), 2, ptr::null_mut()),
            LDB_NULL_POINTER
        );
        let invalid = [0xff_u8, 0xfe];
        assert_eq!(
            ldb_execute(handle, invalid.as_ptr(), invalid.len(), &mut response),
            LDB_INVALID_UTF8
        );
        assert_eq!(
            ldb_execute(
                handle,
                b"{}".as_ptr(),
                LDB_MAX_REQUEST_BYTES + 1,
                &mut response
            ),
            LDB_REQUEST_TOO_LARGE
        );
        let mut out = 0;
        assert_eq!(
            ldb_open(ptr::null(), 0, ptr::null(), 0, &mut out, ptr::null_mut()),
            LDB_NULL_POINTER
        );
    }
    assert_eq!(response, 0, "nothing was answered");
    assert_eq!(ldb_close(handle), LDB_OK);
}

#[test]
fn a_hot_restart_opens_the_same_database_again() {
    let dir = TestDir::new("abi_hot_restart");
    let (before, _) = open(&dir, "app");
    define_and_insert(before);

    // The app restarts without closing: the old handle is still issued, and
    // the new one shares the same environment.
    let (after, _) = open(&dir, "app");
    assert_ne!(before, after);
    assert_eq!(count(after), 1);

    assert_eq!(ldb_close(before), LDB_OK);
    assert_eq!(count(after), 1, "closing the old handle keeps the database");

    // The v1 handle of the same path shares it too.
    let path = CString::new(dir.db_name("app")).expect("path");
    let mut v1 = ptr::null_mut();
    // SAFETY: `path` is live; `v1` is writable.
    let opened = take_wire(unsafe { ofc_open(path.as_ptr(), ptr::null(), &mut v1) });
    assert_eq!(opened["ok"], json!({}));
    let wire = common::Wire(v1);
    assert_eq!(
        wire.ok(json!({"v": 1, "op": "execute", "statement": {"op": "count", "table": "notes"}})),
        json!({"count": 1})
    );
    drop(wire);
    assert_eq!(ldb_close(after), LDB_OK);
}

#[test]
fn requests_from_many_threads_share_one_handle() {
    let dir = TestDir::new("abi_threads");
    let (handle, _) = open(&dir, "db");
    define_and_insert(handle);
    let handle = Arc::new(handle);

    let readers: Vec<_> = (0..8)
        .map(|_| {
            let handle = Arc::clone(&handle);
            thread::spawn(move || {
                for _ in 0..200 {
                    assert_eq!(count(*handle), 1);
                }
            })
        })
        .collect();
    for reader in readers {
        reader.join().expect("reader");
    }

    assert_eq!(ldb_close(*handle), LDB_OK);
}

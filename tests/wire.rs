//! The wire protocol of `ofc_open` / `ofc_execute`, as the Dart SDK uses it.

mod common;

use std::ffi::{CStr, CString};
use std::ptr;
use std::thread;
use std::time::Duration;

use common::TestDir;
use offline_first_core::{close_database, ofc_execute, ofc_free_string, ofc_open, DbHandle};
use serde_json::{json, Value};

/// A handle opened with `ofc_open`, closed on drop.
struct Wire(*mut DbHandle);

impl Wire {
    fn open(dir: &TestDir, name: &str) -> Self {
        let path = CString::new(dir.db_name(name)).expect("path");
        let mut handle = ptr::null_mut();
        // SAFETY: `path` is a live CString and `handle` is writable.
        let response = take(unsafe { ofc_open(path.as_ptr(), ptr::null(), &mut handle) });
        assert!(response.get("ok").is_some(), "{response}");
        Self(handle)
    }

    fn call(&self, request: Value) -> Value {
        let text = CString::new(request.to_string()).expect("request");
        // SAFETY: `self.0` is a live handle and `text` a live CString.
        take(unsafe { ofc_execute(self.0, text.as_ptr()) })
    }

    fn ok(&self, request: Value) -> Value {
        let response = self.call(request.clone());
        assert_eq!(response["v"], 1, "{response}");
        response
            .get("ok")
            .cloned()
            .unwrap_or_else(|| panic!("{request} failed: {response}"))
    }

    fn error_code(&self, request: Value) -> String {
        let response = self.call(request);
        response["error"]["code"]
            .as_str()
            .unwrap_or_else(|| panic!("expected an error: {response}"))
            .to_string()
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `ofc_open` and is closed exactly once.
        take(unsafe { close_database(self.0) });
    }
}

/// Reads and releases a response string.
fn take(response: *const std::ffi::c_char) -> Value {
    assert!(!response.is_null());
    // SAFETY: `response` is a NUL-terminated string returned by the library.
    let text = unsafe { CStr::from_ptr(response) }
        .to_str()
        .expect("UTF-8")
        .to_owned();
    // SAFETY: released exactly once.
    unsafe { ofc_free_string(response.cast_mut()) };
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn define(db: &Wire) {
    db.ok(json!({"v": 1, "op": "define_table", "table": {
        "name": "notes", "primary_key": "id", "auto_increment": true,
        "indexes": [{"name": "by_tag", "fields": ["tag"]}]
    }}));
}

fn insert(tag: &str) -> Value {
    json!({"op": "insert", "table": "notes", "rows": [{"tag": tag}]})
}

fn count_all() -> Value {
    json!({"v": 1, "op": "execute", "statement": {"op": "count", "table": "notes"}})
}

#[test]
fn test_statements_batch_explain_and_info() {
    let dir = TestDir::new("wire_basic");
    let db = Wire::open(&dir, "db");
    define(&db);

    let inserted = db.ok(json!({"v": 1, "op": "execute", "statement": insert("a")}));
    assert_eq!(inserted["affected"], 1);
    assert_eq!(
        inserted["rows"][0],
        json!({"id": 1, "tag": "a"}),
        "inserted rows come back with their key"
    );

    let batch = db.ok(
        json!({"v": 1, "op": "batch", "statements": [insert("b"), insert("b"), {
            "op": "update", "table": "notes", "filter": {"op": "eq", "field": "tag", "value": "b"},
            "set": {"done": true}
        }]}),
    );
    assert_eq!(batch["results"][2]["affected"], 2);

    let rows = db.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "select", "table": "notes",
        "filter": {"op": "eq", "field": "done", "value": true},
        "order": [{"field": "id", "desc": true}], "limit": 1
    }}));
    assert_eq!(rows["rows"], json!([{"id": 3, "tag": "b", "done": true}]));

    let found = db.ok(
        json!({"v": 1, "op": "execute", "statement": {"op": "find", "table": "notes", "key": 1}}),
    );
    assert_eq!(found["row"]["tag"], "a");

    let plan = db.ok(json!({"v": 1, "op": "explain", "query": {
        "table": "notes", "filter": {"op": "eq", "field": "tag", "value": "b"}
    }}));
    assert_eq!(plan["plan"]["index"], "by_tag");
    assert_eq!(
        db.ok(json!({"v": 1, "op": "tables"}))["tables"][0]["name"],
        "notes"
    );
    let info = db.ok(json!({"v": 1, "op": "info"}));
    assert_eq!(info["lmdb"], "1.0.2");
    assert_eq!(info["protocol"], 1);
}

#[test]
fn test_typed_errors() {
    let dir = TestDir::new("wire_errors");
    let db = Wire::open(&dir, "db");
    define(&db);

    assert_eq!(
        db.error_code(json!({"v": 2, "op": "tables"})),
        "UnsupportedProtocol"
    );
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "nope"})),
        "InvalidRequest"
    );
    assert_eq!(
        db.error_code(
            json!({"v": 1, "op": "execute", "statement": {"op": "count", "table": "missing"}})
        ),
        "TableNotFound"
    );
    db.ok(json!({"v": 1, "op": "execute", "statement": {"op": "insert", "table": "notes", "rows": [{"id": 7}]}}));
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "execute", "statement": {"op": "insert", "table": "notes", "rows": [{"id": 7}]}})),
        "DuplicateKey"
    );
    // SAFETY: a null handle and a null request are rejected by the callee.
    let null_handle = take(unsafe { ofc_execute(ptr::null_mut(), ptr::null()) });
    assert_eq!(null_handle["error"]["code"], "InvalidRequest");
    let text = CString::new("not json").expect("request");
    // SAFETY: `db.0` is live and `text` is a live CString.
    let not_json = take(unsafe { ofc_execute(db.0, text.as_ptr()) });
    assert_eq!(not_json["error"]["code"], "InvalidRequest");
}

#[test]
fn test_interactive_transaction_with_savepoints() {
    let dir = TestDir::new("wire_session");
    let db = Wire::open(&dir, "db");
    define(&db);
    let other = Wire::open(&dir, "db");

    let tx = db.ok(json!({"v": 1, "op": "begin"}))["transaction"].clone();
    let exec = |statement: Value| json!({"v": 1, "op": "tx_execute", "transaction": tx, "statement": statement});
    db.ok(exec(insert("kept")));
    // Read-your-writes inside, isolation outside.
    assert_eq!(
        db.ok(exec(json!({"op": "count", "table": "notes"})))["count"],
        1
    );
    assert_eq!(
        other.ok(count_all())["count"],
        0,
        "uncommitted rows are invisible to other handles"
    );

    db.ok(json!({"v": 1, "op": "savepoint", "transaction": tx}));
    db.ok(exec(insert("discarded")));
    db.ok(json!({"v": 1, "op": "rollback_to", "transaction": tx}));
    db.ok(json!({"v": 1, "op": "savepoint", "transaction": tx}));
    db.ok(exec(insert("released")));
    db.ok(json!({"v": 1, "op": "release", "transaction": tx}));
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "release", "transaction": tx})),
        "NoSavepoint"
    );
    db.ok(json!({"v": 1, "op": "commit", "transaction": tx}));

    let rows = other.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "select", "table": "notes", "order": [{"field": "id"}]
    }}));
    let tags: Vec<&str> = rows["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .filter_map(|r| r["tag"].as_str())
        .collect();
    assert_eq!(tags, ["kept", "released"]);
    assert_eq!(db.error_code(exec(insert("late"))), "TransactionClosed");
}

#[test]
fn test_failed_write_makes_the_transaction_rollback_only() {
    let dir = TestDir::new("wire_rollback_only");
    let db = Wire::open(&dir, "db");
    define(&db);
    let tx = db.ok(json!({"v": 1, "op": "begin"}))["transaction"].clone();
    let exec = |statement: Value| json!({"v": 1, "op": "tx_execute", "transaction": tx, "statement": statement});
    db.ok(exec(
        json!({"op": "insert", "table": "notes", "rows": [{"id": 1}]}),
    ));
    assert_eq!(
        db.error_code(exec(
            json!({"op": "insert", "table": "notes", "rows": [{"id": 1}]})
        )),
        "DuplicateKey"
    );
    assert_eq!(db.error_code(exec(insert("more"))), "TransactionAborted");
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "commit", "transaction": tx})),
        "TransactionAborted"
    );
    assert_eq!(db.ok(count_all())["count"], 0);

    let tx = db.ok(json!({"v": 1, "op": "begin"}))["transaction"].clone();
    db.ok(json!({"v": 1, "op": "tx_execute", "transaction": tx, "statement": insert("x")}));
    db.ok(json!({"v": 1, "op": "rollback", "transaction": tx}));
    assert_eq!(db.ok(count_all())["count"], 0);
}

#[test]
fn test_idle_transaction_expires_and_releases_the_writer() {
    let dir = TestDir::new("wire_expiry");
    let db = Wire::open(&dir, "db");
    define(&db);
    let tx = db.ok(json!({"v": 1, "op": "begin", "timeout_ms": 100}))["transaction"].clone();
    db.ok(json!({"v": 1, "op": "tx_execute", "transaction": tx, "statement": insert("abandoned")}));
    thread::sleep(Duration::from_millis(400));

    // The writer is free again: an autocommit write does not wait forever.
    db.ok(json!({"v": 1, "op": "execute", "statement": insert("after")}));
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "commit", "transaction": tx})),
        "TransactionExpired"
    );
    assert_eq!(
        db.ok(count_all())["count"],
        1,
        "the expired transaction was rolled back"
    );
}

#[test]
fn test_read_snapshot_and_foreign_transactions() {
    let dir = TestDir::new("wire_read");
    let db = Wire::open(&dir, "db");
    define(&db);
    let elsewhere = Wire::open(&dir, "other");
    db.ok(json!({"v": 1, "op": "execute", "statement": insert("before")}));

    let snapshot = db.ok(json!({"v": 1, "op": "begin", "mode": "read"}))["transaction"].clone();
    db.ok(json!({"v": 1, "op": "execute", "statement": insert("after")}));
    let seen = db.ok(json!({"v": 1, "op": "tx_execute", "transaction": snapshot,
        "statement": {"op": "count", "table": "notes"}}));
    assert_eq!(seen["count"], 1, "the snapshot keeps its version");
    assert_eq!(
        db.error_code(
            json!({"v": 1, "op": "tx_execute", "transaction": snapshot, "statement": insert("x")})
        ),
        "ReadOnlyTransaction"
    );
    assert_eq!(
        elsewhere.error_code(json!({"v": 1, "op": "commit", "transaction": snapshot})),
        "UnknownTransaction",
        "a transaction is bound to its database"
    );
    db.ok(json!({"v": 1, "op": "commit", "transaction": snapshot}));
}

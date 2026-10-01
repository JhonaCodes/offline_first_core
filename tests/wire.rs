//! The wire protocol of `ofc_open` / `ofc_execute`, as the Dart SDK uses it.

mod common;

use std::ffi::CString;
use std::ptr;
use std::thread;
use std::time::Duration;

use common::{take_wire as take, TestDir, Wire};
use offline_first_core::ofc_execute;
use serde_json::{json, Value};

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

fn people(db: &Wire) {
    db.ok(json!({"v": 1, "op": "define_table", "table": {
        "name": "people", "primary_key": "id"
    }}));
    db.ok(
        json!({"v": 1, "op": "execute", "statement": {"op": "insert", "table": "people", "rows": [
            {"id": 1, "city": "Bogota", "meta": {"stars": 3}},
            {"id": 2, "city": "Bogota", "meta": {"stars": 5}},
            {"id": 3, "city": "Lima", "meta": {"stars": 1}}
        ]}}),
    );
}

#[test]
fn test_select_projection_and_distinct_over_the_wire() {
    let dir = TestDir::new("wire_projection");
    let db = Wire::open(&dir, "db");
    people(&db);

    let projected = db.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "select", "table": "people", "fields": ["city", "meta.stars"],
        "order": [{"field": "id"}]
    }}));
    assert_eq!(
        projected["rows"],
        json!([
            {"city": "Bogota", "meta": {"stars": 3}},
            {"city": "Bogota", "meta": {"stars": 5}},
            {"city": "Lima", "meta": {"stars": 1}}
        ])
    );

    let distinct = db.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "select", "table": "people", "fields": ["city"], "distinct": true,
        "order": [{"field": "id"}]
    }}));
    assert_eq!(
        distinct["rows"],
        json!([{"city": "Bogota"}, {"city": "Lima"}])
    );

    // An empty projected path is `InvalidRequest`, same as the Rust API.
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "execute", "statement": {
            "op": "select", "table": "people", "fields": [""]
        }})),
        "InvalidRequest"
    );
}

#[test]
fn test_group_statement_over_the_wire() {
    let dir = TestDir::new("wire_group");
    let db = Wire::open(&dir, "db");
    people(&db);

    let grouped = db.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "group", "table": "people", "by": ["city"],
        "aggregates": [
            {"function": "count", "as": "total"},
            {"function": "sum", "field": "meta.stars", "as": "stars"}
        ],
        "order": [{"field": "city"}]
    }}));
    assert_eq!(
        grouped["rows"],
        json!([
            {"city": "Bogota", "total": 2, "stars": 8},
            {"city": "Lima", "total": 1, "stars": 1}
        ])
    );

    // An unknown aggregate function does not deserialize: `InvalidRequest`.
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "execute", "statement": {
            "op": "group", "table": "people", "by": ["city"],
            "aggregates": [{"function": "median", "field": "meta.stars", "as": "m"}]
        }})),
        "InvalidRequest"
    );
}

#[test]
fn test_join_statement_over_the_wire() {
    let dir = TestDir::new("wire_join");
    let db = Wire::open(&dir, "db");
    db.ok(json!({"v": 1, "op": "define_table", "table": {"name": "users", "primary_key": "id"}}));
    db.ok(json!({"v": 1, "op": "define_table", "table": {"name": "posts", "primary_key": "id"}}));
    db.ok(
        json!({"v": 1, "op": "execute", "statement": {"op": "insert", "table": "users", "rows": [
            {"id": 1, "name": "ana"}, {"id": 2, "name": "bob"}
        ]}}),
    );
    db.ok(
        json!({"v": 1, "op": "execute", "statement": {"op": "insert", "table": "posts", "rows": [
            {"id": 10, "author_id": 1, "title": "p1"}
        ]}}),
    );

    let joined = db.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "join",
        "from": {"table": "users", "as": "u"},
        "joins": [{"table": "posts", "as": "p", "kind": "left",
                   "on": {"left": "u.id", "right": "author_id"}}],
        "order": [{"field": "u.id"}]
    }}));
    assert_eq!(
        joined["rows"],
        json!([
            {"u": {"id": 1, "name": "ana"}, "p": {"id": 10, "author_id": 1, "title": "p1"}},
            {"u": {"id": 2, "name": "bob"}, "p": null}
        ])
    );

    // An unknown join kind does not deserialize: `InvalidRequest`.
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "execute", "statement": {
            "op": "join", "from": {"table": "users"},
            "joins": [{"table": "posts", "kind": "outer", "on": {"left": "id", "right": "author_id"}}]
        }})),
        "InvalidRequest"
    );
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "execute", "statement": {
            "op": "join", "from": {"table": "missing"}
        }})),
        "TableNotFound"
    );
}

#[test]
fn test_update_increment_over_the_wire() {
    let dir = TestDir::new("wire_increment");
    let db = Wire::open(&dir, "db");
    db.ok(json!({"v": 1, "op": "define_table", "table": {"name": "pages", "primary_key": "id"}}));
    db.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "insert", "table": "pages", "rows": [{"id": 1, "views": 10}]
    }}));

    let updated = db.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "update", "table": "pages",
        "filter": {"op": "eq", "field": "id", "value": 1},
        "increment": {"views": 5, "score": 0.5}
    }}));
    assert_eq!(updated["affected"], 1);

    let row = db.ok(
        json!({"v": 1, "op": "execute", "statement": {"op": "find", "table": "pages", "key": 1}}),
    );
    assert_eq!(row["row"]["views"], 15);
    assert_eq!(row["row"]["score"], 0.5);

    // `increment` and `set` on the same path is `InvalidRequest`, nothing written.
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "execute", "statement": {
            "op": "update", "table": "pages",
            "filter": {"op": "eq", "field": "id", "value": 1},
            "set": {"views": 100}, "increment": {"views": 1}
        }})),
        "InvalidRequest"
    );
    let unchanged = db.ok(
        json!({"v": 1, "op": "execute", "statement": {"op": "find", "table": "pages", "key": 1}}),
    );
    assert_eq!(
        unchanged["row"]["views"], 15,
        "the failed update wrote nothing"
    );
}

#[test]
fn explain_of_a_join_names_its_strategy_and_every_table() {
    let dir = TestDir::new("wire_explain_join");
    let db = Wire::open(&dir, "db");
    for table in ["users", "posts"] {
        db.ok(json!({"v": 1, "op": "define_table", "table": {"name": table, "primary_key": "id"}}));
    }
    let join = json!({
        "from": {"table": "users", "as": "u"},
        "joins": [{"table": "posts", "as": "p", "kind": "left", "on": {"left": "u.id", "right": "author_id"}}],
        "filter": {"op": "eq", "field": "u.id", "value": 1}
    });

    assert_eq!(
        db.ok(json!({"v": 1, "op": "explain", "query": join})),
        json!({"plan": {
            "strategy": "hash_join",
            "tables": [
                {"table": "users", "as": "u", "access": "full_scan"},
                {"table": "posts", "as": "p", "access": "full_scan"}
            ],
            "filter": "after_join"
        }})
    );

    // Without a filter, and with a missing table.
    let unfiltered = json!({"from": {"table": "users"}, "joins": [
        {"table": "posts", "kind": "inner", "on": {"left": "users.id", "right": "author_id"}}
    ]});
    assert_eq!(
        db.ok(json!({"v": 1, "op": "explain", "query": unfiltered}))["plan"]["filter"],
        "none"
    );
    let missing = json!({"from": {"table": "users"}, "joins": [
        {"table": "nope", "kind": "inner", "on": {"left": "users.id", "right": "x"}}
    ]});
    assert_eq!(
        db.error_code(json!({"v": 1, "op": "explain", "query": missing})),
        "TableNotFound"
    );
}

#[test]
fn a_sync_round_trip_over_the_wire() {
    let dir = TestDir::new("wire_sync");
    let db = Wire::open(&dir, "sync");
    db.ok(json!({"v": 1, "op": "define_table", "table": {
        "name": "notes", "primary_key": "id", "sync": "primary"
    }}));
    let tables = db.ok(json!({"v": 1, "op": "tables"}));
    assert_eq!(tables["tables"][0]["sync"], "primary");
    db.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "insert", "table": "notes", "rows": [{"id": "a", "title": "t"}]
    }}));

    let status = db.ok(json!({"v": 1, "op": "sync_status", "remote": "primary"}));
    assert_eq!(
        status,
        json!({"checkpoint": null, "pending": 1, "conflicts": 0})
    );
    let batch = db.ok(json!({"v": 1, "op": "sync_claim", "remote": "primary", "max_changes": 10}));
    let envelope = &batch["envelopes"][0];
    assert_eq!(envelope["row"], json!({"id": "a", "title": "t"}));
    assert_eq!(envelope["operation"], "upsert");

    let outcome = db.ok(
        json!({"v": 1, "op": "sync_push_result", "remote": "primary",
        "lease_id": batch["lease_id"],
        "acknowledged": [{
            "mutation_id": envelope["mutation_id"], "table": "notes", "key": "a",
            "local_revision": envelope["local_revision"], "server_version": "v1"
        }]}),
    );
    assert_eq!(outcome["acknowledged"], 1);
    let state = db.ok(json!({"v": 1, "op": "sync_state", "table": "notes", "key": "a"}));
    assert_eq!(state["state"]["state"], "synced");
    assert_eq!(state["state"]["server_version"], "v1");

    let applied = db.ok(
        json!({"v": 1, "op": "sync_apply_remote", "remote": "primary",
        "expected_checkpoint": null, "next_checkpoint": 7,
        "changes": [{"table": "notes", "key": "b", "operation": "upsert",
            "row": {"id": "b", "title": "remote"}, "server_version": "v2"}]}),
    );
    assert_eq!(applied["applied"], 1);
    assert_eq!(
        db.error_code(
            json!({"v": 1, "op": "sync_apply_remote", "remote": "primary",
            "expected_checkpoint": null, "next_checkpoint": 8, "changes": []})
        ),
        "StaleCheckpoint"
    );
    let pending = db.ok(json!({"v": 1, "op": "sync_pending", "remote": "primary"}));
    assert_eq!(pending, json!({"count": 0, "changes": []}));
    assert_eq!(
        db.error_code(
            json!({"v": 1, "op": "sync_resolve", "conflict": "conflict-9",
            "expected_row_version": 1, "resolution": {"kind": "accept_remote"}})
        ),
        "ConflictNotFound"
    );
}

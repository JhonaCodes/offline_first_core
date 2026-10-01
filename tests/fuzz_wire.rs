//! Malformed requests of the wire protocol answer an error, never a crash
//! or a panic: truncated JSON, removed fields, wrong types, extreme numbers,
//! unknown operations and nesting past the parser's limit. After thousands
//! of them the database still answers correctly.
//!
//! The mutations come from a fixed seed, so a failure is reproducible; the
//! failing request is printed.

mod common;

use std::ffi::CString;

use common::{take_wire, TestDir, Wire};
use offline_first_core::ofc_execute;
use serde_json::{json, Value};

const SEED: u64 = 0x005e_ed0f_ca11;
const ROUNDS: usize = 4_000;

/// xorshift64*: deterministic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }
}

/// Valid requests of every family except interactive transactions (a
/// mutated `begin` could hold the writer for its whole timeout).
fn corpus() -> Vec<Value> {
    let filter = json!({"op": "and", "exprs": [
        {"op": "eq", "field": "city", "value": "Lima"},
        {"op": "gt", "field": "age", "value": 30}
    ]});
    vec![
        json!({"v": 1, "op": "define_table", "table": {"name": "fuzz", "primary_key": "id",
            "indexes": [{"name": "by_city", "fields": ["city"], "unique": false}]}}),
        json!({"v": 1, "op": "execute", "statement": {"op": "insert", "table": "people",
            "rows": [{"id": "p9", "city": "Lima", "age": 40}]}}),
        json!({"v": 1, "op": "execute", "statement": {"op": "select", "table": "people",
            "filter": filter, "order": [{"field": "age", "desc": true}], "limit": 10, "offset": 1}}),
        json!({"v": 1, "op": "execute", "statement": {"op": "update", "table": "people",
            "filter": {"op": "eq", "field": "id", "value": "p1"}, "set": {"age": 41},
            "increment": {"visits": 1}}}),
        json!({"v": 1, "op": "execute", "statement": {"op": "delete", "table": "people",
            "filter": {"op": "eq_any", "field": "id", "values": ["p7", "p8"]}}}),
        json!({"v": 1, "op": "execute", "statement": {"op": "group", "table": "people",
            "by": ["city"], "aggregates": [{"function": "count", "alias": "n"}],
            "having": {"op": "gt", "field": "n", "value": 0}}}),
        json!({"v": 1, "op": "execute", "statement": {"op": "join",
            "from": {"table": "people", "as": "p"},
            "joins": [{"table": "notes", "as": "n", "kind": "left",
                "on": {"left": "p.id", "right": "author"}}]}}),
        json!({"v": 1, "op": "batch", "statements": [
            {"op": "count", "table": "people"},
            {"op": "aggregate", "table": "people", "function": "max", "field": "age"}]}),
        json!({"v": 1, "op": "explain", "query": {"table": "people", "filter": filter}}),
        json!({"v": 1, "op": "sync_claim", "remote": "primary", "max_changes": 5,
            "max_bytes": 4096, "lease_ms": 0}),
        json!({"v": 1, "op": "sync_push_result", "remote": "primary", "lease_id": 1,
            "acknowledged": [{"mutation_id": "x-1", "table": "notes", "key": "n1",
                "local_revision": 1, "server_version": "v1"}],
            "rejected": [{"mutation_id": "x-2", "reason": "r", "retryable": false}]}),
        json!({"v": 1, "op": "sync_apply_remote", "remote": "primary",
            "expected_checkpoint": null, "next_checkpoint": 1,
            "changes": [{"table": "notes", "key": "n2", "operation": "upsert",
                "row": {"id": "n2", "author": "p1"}, "server_version": "v2"}]}),
        json!({"v": 1, "op": "sync_resolve", "conflict": "conflict-1",
            "expected_row_version": 1, "resolution": {"kind": "merged", "row": {"id": "n1"}}}),
        json!({"v": 1, "op": "sync_pending", "remote": "primary", "table": "notes", "limit": 3}),
        json!({"v": 1, "op": "sync_state", "table": "notes", "key": "n1"}),
        json!({"v": 1, "op": "tables"}),
        json!({"v": 1, "op": "info"}),
    ]
}

/// Odd values for a leaf.
fn odd_value(rng: &mut Rng) -> Value {
    match rng.below(10) {
        0 => json!(u64::MAX),
        1 => json!(i64::MIN),
        2 => json!(1e308),
        3 => json!(-0.0),
        4 => json!(""),
        5 => json!("x".repeat(600)),
        6 => Value::Null,
        7 => json!([]),
        8 => json!({}),
        _ => json!(true),
    }
}

/// Paths to every value of `value`.
fn paths(value: &Value, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    out.push(prefix.clone());
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                prefix.push(key.clone());
                paths(child, prefix, out);
                prefix.pop();
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                prefix.push(index.to_string());
                paths(child, prefix, out);
                prefix.pop();
            }
        }
        _ => {}
    }
}

fn at_mut<'a>(value: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
    path.iter().try_fold(value, |node, step| match node {
        Value::Object(map) => map.get_mut(step),
        Value::Array(items) => step.parse::<usize>().ok().and_then(|i| items.get_mut(i)),
        _ => None,
    })
}

/// One malformed version of `request`, as text.
fn mutate(rng: &mut Rng, request: &Value) -> String {
    let mut value = request.clone();
    let mut all = Vec::new();
    paths(&value, &mut Vec::new(), &mut all);
    let path = all[rng.below(all.len())].clone();
    match rng.below(7) {
        0 => {
            let text = request.to_string();
            let mut cut = rng.below(text.len());
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            return text[..cut].to_string();
        }
        1 => {
            if let Some(node) = at_mut(&mut value, &path) {
                *node = odd_value(rng);
            }
        }
        2 => {
            if let Some((last, parent)) = path.split_last() {
                if let Some(Value::Object(map)) = at_mut(&mut value, parent) {
                    map.remove(last);
                }
            }
        }
        3 => value["op"] = json!(format!("op_{}", rng.next() % 1000)),
        4 => {
            let depth = 50 + rng.below(400);
            let mut nested = json!({"op": "eq", "field": "id", "value": 1});
            for _ in 0..depth {
                nested = json!({"op": "not", "expr": nested});
            }
            value = json!({"v": 1, "op": "execute",
                "statement": {"op": "select", "table": "people", "filter": nested}});
        }
        5 => value["v"] = odd_value(rng),
        _ => {
            if let Some(node) = at_mut(&mut value, &path) {
                *node = json!([node.clone(), node.clone()]);
            }
        }
    }
    value.to_string()
}

fn send(db: &Wire, text: &str) -> Value {
    let request = CString::new(text.replace('\0', "")).expect("no NUL");
    // SAFETY: `db.0` is a live handle and `request` a live CString.
    take_wire(unsafe { ofc_execute(db.0, request.as_ptr()) })
}

#[test]
fn malformed_requests_answer_errors_and_the_database_keeps_working() {
    let dir = TestDir::new("fuzz_wire");
    let db = Wire::open(&dir, "fuzz");
    db.ok(
        json!({"v": 1, "op": "define_table", "table": {"name": "people", "primary_key": "id",
        "indexes": [{"name": "by_city_age", "fields": ["city", "age"], "unique": false}]}}),
    );
    db.ok(
        json!({"v": 1, "op": "define_table", "table": {"name": "notes", "primary_key": "id",
        "sync": "primary"}}),
    );
    db.ok(json!({"v": 1, "op": "execute", "statement": {"op": "insert", "table": "people",
        "rows": [{"id": "p1", "city": "Lima", "age": 36}, {"id": "p2", "city": "Bogotá", "age": 28}]}}));
    let corpus = corpus();
    let mut rng = Rng(SEED);
    let mut errors = 0;

    for round in 0..ROUNDS {
        let original = &corpus[rng.below(corpus.len())];
        let text = mutate(&mut rng, original);
        let response = send(&db, &text);

        assert_eq!(response["v"], 1, "round {round}: {text} -> {response}");
        let ok = response.get("ok").is_some();
        let code = response["error"]["code"].as_str();
        assert!(ok != code.is_some(), "round {round}: {text} -> {response}");
        assert_ne!(
            code,
            Some("InternalPanic"),
            "round {round}: {text} panicked"
        );
        errors += usize::from(code.is_some());
    }

    println!("{ROUNDS} malformed requests, {errors} answered an error");
    assert!(errors > ROUNDS / 2, "most mutations must be refused");
    // A mutation may be a valid write (a delete without its filter deletes
    // every row), so the check writes a row of its own and reads it back.
    db.ok(
        json!({"v": 1, "op": "execute", "statement": {"op": "insert", "table": "people",
        "rows": [{"id": "after-fuzz", "city": "Lima", "age": 50}], "on_conflict": "replace"}}),
    );
    let found = db.ok(json!({"v": 1, "op": "execute", "statement": {"op": "find",
        "table": "people", "key": "after-fuzz"}}));
    assert_eq!(
        found["row"]["age"], 50,
        "the database still answers: {found}"
    );
}

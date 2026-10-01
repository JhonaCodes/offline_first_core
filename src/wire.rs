//! Wire protocol of [`ofc_execute`](crate::ofc_execute), version 1.
//!
//! A request is a JSON object `{"v": 1, "op": "<operation>", ...}`; the
//! response is `{"v": 1, "ok": <payload>}` or
//! `{"v": 1, "error": {"code": "<Code>", "message": "..."}}`, where `code` is
//! stable ([`EngineError::code`]) and `message` is for humans.
//!
//! | `op` | Fields | `ok` payload |
//! |---|---|---|
//! | `define_table` | `table`: [`TableDef`] | `{"changed": bool}` |
//! | `drop_table` | `name` | `{"dropped": bool}` |
//! | `tables` | — | `{"tables": [TableDef]}` |
//! | `execute` | `statement`: [`Statement`] | statement result |
//! | `batch` | `statements`: [`Statement`] list | `{"results": [...]}` |
//! | `explain` | `query`: [`Select`] | `{"plan": {...}}` |
//! | `begin` | `mode`: `"write"`/`"read"`, `timeout_ms` | `{"transaction": id}` |
//! | `tx_execute` | `transaction`, `statement` | statement result |
//! | `savepoint`, `release`, `rollback_to`, `commit`, `rollback` | `transaction` | `{}` |
//! | `info` | — | `{"map_size": n, "lmdb": "1.0.2", "tables": n}` |
//!
//! Statement results: `{"rows": [...]}` (`select`, `group`, `join`),
//! `{"row": {...}|null}` (find), `{"count": n}`, `{"value": v}` (aggregate),
//! and `{"affected": n, "rows": [...]}` for writes (`rows` holds inserted
//! rows). A plain `select` (no `fields`/`distinct`) sends its rows as stored,
//! without re-serialization; a projected `select`, `group` and `join` build
//! new JSON objects, so their rows are freshly encoded instead.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::engine::session::{self, Mode};
use crate::engine::{exec, tx, EngineError, EngineResult, Output, Select, Statement, TableDef};
use crate::registry::SharedDb;

/// Protocol version spoken by this library.
pub const PROTOCOL_VERSION: u64 = 1;

/// Default idle timeout of an interactive transaction.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Handles one request and returns the response JSON.
pub(crate) fn handle(shared: &Arc<SharedDb>, request: &str) -> String {
    match dispatch(shared, request) {
        Ok(payload) => format!(r#"{{"v":{PROTOCOL_VERSION},"ok":{payload}}}"#),
        Err(error) => error_response(error.code(), &error.to_string()),
    }
}

/// An error response.
pub(crate) fn error_response(code: &str, message: &str) -> String {
    json!({"v": PROTOCOL_VERSION, "error": {"code": code, "message": message}}).to_string()
}

fn field<T: DeserializeOwned>(request: &Value, name: &str) -> EngineResult<T> {
    let value = request
        .get(name)
        .cloned()
        .ok_or_else(|| EngineError::InvalidRequest(format!("missing field `{name}`")))?;
    serde_json::from_value(value)
        .map_err(|e| EngineError::InvalidRequest(format!("field `{name}`: {e}")))
}

fn dispatch(shared: &Arc<SharedDb>, request: &str) -> EngineResult<String> {
    let request: Value = serde_json::from_str(request)
        .map_err(|e| EngineError::InvalidRequest(format!("request is not JSON: {e}")))?;
    let version = request.get("v").and_then(Value::as_u64).unwrap_or(0);
    if version != PROTOCOL_VERSION {
        return Err(EngineError::UnsupportedProtocol(version));
    }
    let op: String = field(&request, "op")?;
    match op.as_str() {
        "define_table" => {
            let def: TableDef = field(&request, "table")?;
            let changed = shared.exclusive(|store| store.define_table(def))?;
            Ok(json!({ "changed": changed }).to_string())
        }
        "drop_table" => {
            let name: String = field(&request, "name")?;
            let dropped = shared.exclusive(|store| store.drop_table(&name))?;
            Ok(json!({ "dropped": dropped }).to_string())
        }
        "tables" => {
            let tables = shared.run(|store| Ok(store.tables()))?;
            Ok(json!({ "tables": tables }).to_string())
        }
        "execute" => {
            let statement: Statement = field(&request, "statement")?;
            let output = shared.run(|store| tx::autocommit(store, &statement))?;
            Ok(output_json(&output))
        }
        "batch" => {
            let statements: Vec<Statement> = field(&request, "statements")?;
            let outputs = shared.run(|store| tx::batch(store, &statements))?;
            let results: Vec<String> = outputs.iter().map(output_json).collect();
            Ok(format!(r#"{{"results":[{}]}}"#, results.join(",")))
        }
        "explain" => {
            let query: Select = field(&request, "query")?;
            let plan = shared.run(|store| exec::explain(store, &query))?;
            Ok(json!({ "plan": plan }).to_string())
        }
        "begin" => {
            let mode = match request
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("write")
            {
                "write" => Mode::Write,
                "read" => Mode::Read,
                other => {
                    return Err(EngineError::InvalidRequest(format!(
                        "unknown mode `{other}`"
                    )))
                }
            };
            let timeout = request
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map_or(DEFAULT_TIMEOUT, Duration::from_millis);
            shared.grow_if_requested()?;
            let id = session::begin(shared, mode, timeout)?;
            Ok(json!({ "transaction": id }).to_string())
        }
        "tx_execute" => {
            let id: u64 = field(&request, "transaction")?;
            let statement: Statement = field(&request, "statement")?;
            Ok(output_json(&session::execute(shared, id, statement)?))
        }
        "savepoint" | "release" | "rollback_to" | "commit" | "rollback" => {
            let id: u64 = field(&request, "transaction")?;
            match op.as_str() {
                "savepoint" => session::savepoint(shared, id),
                "release" => session::release(shared, id),
                "rollback_to" => session::rollback_to(shared, id),
                "commit" => session::commit(shared, id),
                _ => session::rollback(shared, id),
            }?;
            Ok("{}".to_string())
        }
        "info" => {
            let (map_size, tables) =
                shared.run(|store| Ok((store.map_size()?, store.tables().len())))?;
            let version = natdb::version();
            Ok(json!({
                "map_size": map_size,
                "tables": tables,
                "lmdb": format!("{}.{}.{}", version.major, version.minor, version.patch),
                "protocol": PROTOCOL_VERSION,
            })
            .to_string())
        }
        other => Err(EngineError::InvalidRequest(format!(
            "unknown operation `{other}`"
        ))),
    }
}

fn raw_list(rows: &[Vec<u8>]) -> String {
    let mut out = String::from("[");
    for (position, row) in rows.iter().enumerate() {
        if position > 0 {
            out.push(',');
        }
        out.push_str(&String::from_utf8_lossy(row));
    }
    out.push(']');
    out
}

/// JSON of a statement result.
fn output_json(output: &Output) -> String {
    match output {
        Output::Rows(rows) => format!(r#"{{"rows":{}}}"#, raw_list(rows)),
        Output::Row(Some(row)) => format!(r#"{{"row":{}}}"#, String::from_utf8_lossy(row)),
        Output::Row(None) => r#"{"row":null}"#.to_string(),
        Output::Count(count) => format!(r#"{{"count":{count}}}"#),
        Output::Value(value) => json!({ "value": value }).to_string(),
        Output::Affected { count, rows } => {
            format!(r#"{{"affected":{count},"rows":{}}}"#, raw_list(rows))
        }
    }
}

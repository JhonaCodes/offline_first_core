//! Benchmarks of the query engine (RFC-001 §18.3), run with:
//!
//! ```text
//! cargo run --release --example bench -- [rows]
//! ```
//!
//! or `tool/bench.sh` for 10 000, 100 000 and 1 000 000 rows into
//! `BENCHMARKS.md`. Every write is durable (the default `Durability::Full`,
//! the same for every row of the table); each number is the median of five
//! runs on a fresh database in a temporary directory. Reads repeat fewer
//! times on larger tables, so a run stays within minutes.

use std::env;
use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::ptr;
use std::time::{Duration, Instant};

use offline_first_core::engine::sync::{Acknowledgement, ClaimLimits, PushResult};
use offline_first_core::engine::{col, Db, EngineError, OpenOptions, Query, TableDef};
use offline_first_core::{close_database, ofc_execute, ofc_free_string, ofc_open, DbHandle};
use serde_json::{json, Value};

const CITIES: [&str; 4] = ["Bogotá", "Auckland", "Lima", "Madrid"];

fn user(id: u64) -> Value {
    json!({
        "id": id,
        "name": format!("user-{id}"),
        "email": format!("user{id}@example.com"),
        "age": 18 + id % 60,
        "city": CITIES[(id % 4) as usize],
        "active": id % 3 != 0,
    })
}

fn post(id: u64, users: u64) -> Value {
    json!({"id": id, "author": id % users, "title": format!("post-{id}"), "likes": id % 100})
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn measure(
    label: &str,
    operations: u64,
    mut run: impl FnMut() -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let mut samples = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        run()?;
        samples.push(start.elapsed());
    }
    let time = median(samples);
    let per_op = time.as_secs_f64() * 1e6 / operations as f64;
    println!(
        "| {label} | {operations} | {:.2} ms | {per_op:.2} µs |",
        time.as_secs_f64() * 1e3
    );
    Ok(())
}

/// Repetitions of a read on a table of `rows`: fewer on larger tables.
fn repeats(rows: u64, at_10k: u64) -> u64 {
    (at_10k * 10_000 / rows.max(1)).clamp(1, at_10k)
}

/// The same database through the C ABI and the JSON wire protocol, as the
/// Dart SDKs call it.
struct Wire(*mut DbHandle);

impl Wire {
    fn open(path: &str) -> Self {
        let path = CString::new(path).unwrap_or_default();
        let mut handle = ptr::null_mut();
        // SAFETY: `path` is a live CString and `handle` is writable.
        let response = unsafe { ofc_open(path.as_ptr(), ptr::null(), &mut handle) };
        // SAFETY: `response` was returned by the library.
        unsafe { ofc_free_string(response.cast_mut()) };
        Self(handle)
    }

    /// Sends `request`; answers the length of the response.
    fn call(&self, request: &CString) -> usize {
        // SAFETY: `self.0` is a live handle and `request` a live CString.
        let response: *const c_char = unsafe { ofc_execute(self.0, request.as_ptr()) };
        // SAFETY: `response` is a NUL-terminated string of the library.
        let length = unsafe { CStr::from_ptr(response) }.to_bytes().len();
        // SAFETY: released exactly once.
        unsafe { ofc_free_string(response.cast_mut()) };
        length
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `ofc_open` and is closed once.
        let response = unsafe { close_database(self.0) };
        // SAFETY: `response` was returned by the library.
        unsafe { ofc_free_string(response.cast_mut()) };
    }
}

fn main() -> Result<(), EngineError> {
    let rows: u64 = env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(10_000);
    let base = env::temp_dir().join(format!("offline_first_core_bench_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let path = base.join("bench");
    let db = Db::open_with(path.with_extension("lmdb"), OpenOptions::default())?;
    db.define_table(
        TableDef::new("users", "id")
            .index("by_city_age", &["city", "age"])
            .unique_index("by_email", &["email"]),
    )?;
    db.define_table(TableDef::new("posts", "id").index("by_author", &["author"]))?;
    db.define_table(
        TableDef::new("notes", "id")
            .index("by_city_age", &["city", "age"])
            .sync_with("primary"),
    )?;

    println!(
        "offline_first_core {} — {rows} rows\n",
        env!("CARGO_PKG_VERSION")
    );
    println!("| Operation | Ops | Median time | Per op |");
    println!("|---|---|---|---|");

    let users: Vec<Value> = (0..rows).map(user).collect();
    measure("insert batch (1 transaction)", rows, || {
        db.execute(&Query::delete("users").into())?;
        Query::insert_into("users", users.clone())
            .execute(&db)
            .map(|_| ())
    })?;
    // A synchronized table keeps the evidence of a deleted key until the
    // server acknowledges it, so each run inserts keys of its own.
    let synced = rows.min(100_000);
    let mut batches: Vec<Vec<Value>> = (0..5u64)
        .map(|round| (0..synced).map(|i| user(round * synced + i)).collect())
        .collect();
    measure(
        "insert batch, synchronized table (row + outbox)",
        synced,
        || {
            let batch = batches.pop().unwrap_or_default();
            Query::insert_into("notes", batch).execute(&db).map(|_| ())
        },
    )?;
    let posts: Vec<Value> = (0..rows / 4).map(|id| post(id, rows)).collect();
    db.execute(&Query::insert_into("posts", posts).into())?;

    measure("insert, 1 transaction per row", 200, || {
        db.execute(&Query::delete("users").filter(col("id").ge(rows)).into())?;
        for id in rows..rows + 200 {
            Query::insert_into("users", [user(id)]).execute(&db)?;
        }
        Ok(())
    })?;

    measure("find by primary key", 1_000, || {
        for id in (0..rows)
            .step_by((rows / 1_000).max(1) as usize)
            .take(1_000)
        {
            db.execute(&Query::find("users", id))?;
        }
        Ok(())
    })?;

    let ranges = repeats(rows, 200);
    measure(
        "composite range (city = x AND age > y, limit 50)",
        ranges,
        || {
            for i in 0..ranges {
                let city = CITIES[(i % 4) as usize];
                Query::table("users")
                    .filter(col("city").eq(city))
                    .filter(col("age").gt(30 + i % 20))
                    .limit(50)
                    .load::<Value>(&db)?;
            }
            Ok(())
        },
    )?;

    measure("top 20 by indexed order", ranges, || {
        for _ in 0..ranges {
            Query::table("users")
                .filter(col("city").eq("Lima"))
                .order(col("age").desc())
                .limit(20)
                .load::<Value>(&db)?;
        }
        Ok(())
    })?;

    measure("indexed count (city = x)", ranges, || {
        for i in 0..ranges {
            Query::table("users")
                .filter(col("city").eq(CITIES[(i % 4) as usize]))
                .count(&db)?;
        }
        Ok(())
    })?;

    let scans = repeats(rows, 10);
    measure("full scan filter (not indexed)", scans, || {
        for _ in 0..scans {
            Query::table("users")
                .filter(col("active").eq(false))
                .count(&db)?;
        }
        Ok(())
    })?;

    measure(
        "join users ⋈ posts (hash join, filter + limit 50)",
        scans,
        || {
            for i in 0..scans {
                Query::join("users")
                    .inner_join("posts", "users.id", "author")
                    .filter(col("users.city").eq(CITIES[(i % 4) as usize]))
                    .limit(50)
                    .load::<Value>(&db)?;
            }
            Ok(())
        },
    )?;

    measure("belonging_to: posts of 50 users (eq_any)", ranges, || {
        for i in 0..ranges {
            let parents: Vec<u64> = (0..50).map(|k| (i * 50 + k) % rows).collect();
            Query::table("posts")
                .filter(col("author").eq_any(parents))
                .load::<Value>(&db)?;
        }
        Ok(())
    })?;

    measure("update 1 row by key", 200, || {
        for id in 0..200u64 {
            Query::update("users")
                .filter(col("id").eq(id))
                .set("age", 40)
                .execute(&db)?;
        }
        Ok(())
    })?;

    measure("sync claim + acknowledge 100 changes", 100, || {
        let batch = db.sync().claim("primary", &ClaimLimits::default())?;
        let acknowledged = batch
            .envelopes
            .iter()
            .map(|envelope| Acknowledgement::of(envelope, json!("v")))
            .collect();
        db.sync().apply_push_result(
            "primary",
            &PushResult {
                lease_id: batch.lease_id,
                acknowledged,
                rejected: Vec::new(),
            },
        )?;
        Ok(())
    })?;

    // The wire: the same reads as JSON requests through `ofc_execute`, as
    // the Dart SDKs send them. The difference with the rows above is the
    // cost of the JSON protocol and the C ABI.
    let wire = Wire::open(&path.to_string_lossy());
    let finds: Vec<CString> = (0..rows)
        .step_by((rows / 1_000).max(1) as usize)
        .take(1_000)
        .map(|id| {
            let request = json!({"v": 1, "op": "execute",
                "statement": {"op": "find", "table": "users", "key": id}});
            CString::new(request.to_string()).unwrap_or_default()
        })
        .collect();
    measure("wire: find by primary key (JSON)", 1_000, || {
        for request in &finds {
            wire.call(request);
        }
        Ok(())
    })?;
    let page = CString::new(
        json!({"v": 1, "op": "execute", "statement": {"op": "select", "table": "users",
            "filter": {"op": "eq", "field": "city", "value": "Lima"}, "limit": 50}})
        .to_string(),
    )
    .unwrap_or_default();
    measure("wire: page of 50 rows (JSON)", ranges, || {
        for _ in 0..ranges {
            wire.call(&page);
        }
        Ok(())
    })?;
    measure("page of 50 rows (Rust API)", ranges, || {
        for _ in 0..ranges {
            Query::table("users")
                .filter(col("city").eq("Lima"))
                .limit(50)
                .load::<Value>(&db)?;
        }
        Ok(())
    })?;
    drop(wire);

    let _ = fs::remove_dir_all(&base);
    Ok(())
}

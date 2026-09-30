//! Benchmarks of the query engine, run with:
//!
//! ```text
//! cargo run --release --example bench -- [rows]
//! ```
//!
//! Every write is durable (LMDB's default sync on commit); the numbers are the
//! median of five runs of each operation on a fresh database in a temporary
//! directory.

use std::env;
use std::time::{Duration, Instant};

use offline_first_core::engine::{col, Db, EngineError, OpenOptions, Query, TableDef};
use serde_json::{json, Value};

const CITIES: [&str; 4] = ["Bogotá", "Auckland", "Lima", "Madrid"];

fn row(id: u64) -> Value {
    let city = CITIES[(id % 4) as usize];
    json!({
        "id": id,
        "name": format!("user-{id}"),
        "email": format!("user{id}@example.com"),
        "age": 18 + id % 60,
        "city": city,
        "active": id % 3 != 0,
    })
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

fn main() -> Result<(), EngineError> {
    let rows: u64 = env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(10_000);
    let dir = env::temp_dir().join(format!("offline_first_core_bench_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = Db::open_with(&dir, OpenOptions::default())?;
    db.define_table(
        TableDef::new("users", "id")
            .index("by_city_age", &["city", "age"])
            .unique_index("by_email", &["email"]),
    )?;

    println!(
        "offline_first_core {} — {rows} rows\n",
        env!("CARGO_PKG_VERSION")
    );
    println!("| Operation | Ops | Median time | Per op |");
    println!("|---|---|---|---|");

    let data: Vec<Value> = (0..rows).map(row).collect();
    let mut round = 0u64;
    measure("insert batch (1 transaction)", rows, || {
        round += 1;
        db.execute(&Query::delete("users").into())?;
        Query::insert_into("users", data.clone())
            .execute(&db)
            .map(|_| ())
    })?;
    let _ = round;

    measure("insert, 1 transaction per row", 200, || {
        db.execute(&Query::delete("users").filter(col("id").ge(rows)).into())?;
        for id in rows..rows + 200 {
            Query::insert_into("users", [row(id)]).execute(&db)?;
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

    measure(
        "indexed query (city = x AND age > y, limit 50)",
        200,
        || {
            for i in 0..200u64 {
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

    measure("top 20 by indexed order", 200, || {
        for _ in 0..200 {
            Query::table("users")
                .filter(col("city").eq("Lima"))
                .order(col("age").desc())
                .limit(20)
                .load::<Value>(&db)?;
        }
        Ok(())
    })?;

    measure("full scan filter (not indexed)", 10, || {
        for _ in 0..10 {
            Query::table("users")
                .filter(col("active").eq(false))
                .count(&db)?;
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

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

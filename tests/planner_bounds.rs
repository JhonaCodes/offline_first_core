//! Range bounds and exact plans: a range scan visits only the keys of its
//! bounds and of the value kind it compares with, and a filter that the chosen
//! keys already satisfy is not evaluated again (`explain()["exact"]`).
//!
//! The data mixes every scalar kind in the indexed field (missing, null,
//! booleans, integers, floats, `-0.0`, integers beyond 2^53 and strings), so a
//! bound that leaks into a neighbouring kind or value shows up as a wrong row.

mod common;

use common::TestDir;
use offline_first_core::engine::{col, Db, Expr, Query, Select, TableDef};
use serde_json::{json, Value};

const HUGE: i64 = 1 << 53;

fn values() -> Vec<Value> {
    vec![
        Value::Null,
        json!(false),
        json!(true),
        json!(-3),
        json!(-1),
        json!(-0.0),
        json!(0),
        json!(1),
        json!(1.5),
        json!(2),
        json!(3),
        json!(HUGE),
        json!(HUGE + 1),
        json!(HUGE + 2),
        json!("a"),
        json!("b"),
        json!("b\u{0}x"),
        json!("c"),
    ]
}

/// Deterministic pseudo-random numbers (no extra dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }

    fn pick(&mut self, values: &[Value]) -> Value {
        values[self.next(values.len() as u64) as usize].clone()
    }
}

fn seeded(dir: &TestDir) -> (Db, Vec<Value>) {
    let db = Db::open(dir.db_dir("db")).expect("open");
    db.define_table(
        TableDef::new("mixed", "id")
            .index("by_v", &["v"])
            .index("by_g_v", &["g", "v"])
            .unique_index("by_u", &["u"]),
    )
    .expect("define");
    let pool = values();
    let mut rng = Lcg(11);
    let rows: Vec<Value> = (0..400)
        .map(|id| {
            let group = if id % 2 == 0 { "x" } else { "y" };
            let mut row = json!({"id": id, "g": group, "u": id});
            // Some rows lack `v`, others hold every kind of scalar.
            if rng.next(10) != 0 {
                row["v"] = rng.pick(&pool);
            }
            // The unique field has NULLs too (they are exempt from uniqueness).
            if id % 9 == 0 {
                row["u"] = Value::Null;
            }
            row
        })
        .collect();
    Query::insert_into("mixed", rows.clone())
        .execute(&db)
        .expect("seed");
    (db, rows)
}

fn matching_ids(rows: &[Value], filter: &Expr) -> Vec<i64> {
    let mut ids: Vec<i64> = rows
        .iter()
        .filter(|row| filter.matches(row))
        .filter_map(|row| row["id"].as_i64())
        .collect();
    ids.sort_unstable();
    ids
}

fn loaded_ids(db: &Db, query: &Select) -> Vec<i64> {
    let rows: Vec<Value> = db.load(query).expect("load");
    rows.iter().filter_map(|row| row["id"].as_i64()).collect()
}

fn random_filter(rng: &mut Lcg, field: &str) -> Expr {
    let pool = values();
    let (a, b) = (rng.pick(&pool), rng.pick(&pool));
    let column = col(field);
    match rng.next(8) {
        0 => column.eq(a),
        1 => column.gt(a),
        2 => column.ge(a),
        3 => column.lt(a),
        4 => column.le(a),
        5 => column.between(a, b),
        6 => column.gt(a).and(col(field).lt(b)),
        _ => column.ge(a).and(col(field).le(b)),
    }
}

#[test]
fn test_ranges_over_mixed_kinds_equal_the_in_memory_filter() {
    let dir = TestDir::new("bounds_mixed");
    let (db, rows) = seeded(&dir);
    let mut rng = Lcg(3);
    for round in 0..400 {
        let field = ["v", "u", "id"][round % 3];
        let mut filter = random_filter(&mut rng, field);
        if field == "v" && rng.next(2) == 0 {
            // Served by the composite index `by_g_v`.
            filter = col("g").eq("y").and(filter);
        }
        let expected = matching_ids(&rows, &filter);
        let query = Query::table("mixed").filter(filter.clone());

        let mut got = loaded_ids(&db, &query);
        got.sort_unstable();
        assert_eq!(got, expected, "load {filter:?}");
        assert_eq!(
            query.count(&db).expect("count"),
            expected.len() as u64,
            "count {filter:?}"
        );

        // Ordered scans in both directions, with and without a limit.
        for descending in [false, true] {
            let order = if descending {
                col(field).desc()
            } else {
                col(field).asc()
            };
            let ordered = query.clone().order(order.clone());
            let mut got = loaded_ids(&db, &ordered);
            got.sort_unstable();
            assert_eq!(got, expected, "{order:?} {filter:?}");

            let limited = ordered.limit(5);
            let got = loaded_ids(&db, &limited);
            assert_eq!(
                got.len(),
                expected.len().min(5),
                "{order:?} limit {filter:?}"
            );
            assert!(
                got.iter().all(|id| expected.contains(id)),
                "{order:?} limit {filter:?}"
            );
        }
    }
}

#[test]
fn test_explain_reports_whether_the_keys_satisfy_the_filter() {
    let dir = TestDir::new("bounds_exact");
    let (db, _) = seeded(&dir);
    let exact = |filter: Expr| -> Value {
        let plan = db
            .explain(&Query::table("mixed").filter(filter))
            .expect("explain");
        plan["exact"].clone()
    };

    // The keys alone decide these filters.
    assert_eq!(exact(col("id").eq(3)), true);
    assert_eq!(exact(col("id").between(2, 30)), true);
    assert_eq!(exact(col("v").gt(1)), true);
    assert_eq!(exact(col("v").ge("b").and(col("v").lt("c"))), true);
    assert_eq!(exact(col("v").eq(true)), true);
    assert_eq!(exact(col("g").eq("x").and(col("v").le(2))), true);
    assert_eq!(exact(col("u").gt(10)), true);

    // Every visited row is checked again.
    assert_eq!(
        exact(col("g").eq("x").and(col("w").eq(1))),
        false,
        "residual"
    );
    assert_eq!(exact(col("v").gt(1).and(col("v").gt(2))), false, "twice");
    assert_eq!(exact(col("v").gt(1).and(col("v").lt("c"))), false, "kinds");
    assert_eq!(exact(col("v").eq(HUGE)), false, "beyond 2^53");
    assert_eq!(exact(col("v").ne(1)), false, "full scan");
    assert_eq!(
        db.explain(&Query::table("mixed")).expect("explain")["exact"],
        true,
        "no filter"
    );
}

#[test]
fn test_negative_zero_equals_zero_with_and_without_an_index() {
    let dir = TestDir::new("bounds_zero");
    let db = Db::open(dir.db_dir("db")).expect("open");
    db.define_table(TableDef::new("zeros", "id").index("by_v", &["v"]))
        .expect("define");
    Query::insert_into(
        "zeros",
        [
            json!({"id": 1, "v": -0.0, "w": -0.0}),
            json!({"id": 2, "v": 0, "w": 0}),
            json!({"id": 3, "v": 1, "w": 1}),
        ],
    )
    .execute(&db)
    .expect("insert");
    for field in ["v", "w"] {
        let query = Query::table("zeros").filter(col(field).eq(0));
        let mut got = loaded_ids(&db, &query);
        got.sort_unstable();
        assert_eq!(got, [1, 2], "{field} = 0");
        let below = Query::table("zeros").filter(col(field).lt(0));
        assert_eq!(loaded_ids(&db, &below), Vec::<i64>::new(), "{field} < 0");
    }
}

#[test]
fn test_keys_are_limited_to_511_bytes_on_every_platform() {
    // LMDB 1.0 derives its own limit from the page size (about 2 KB with 4 KB
    // pages, 8 KB with the 16 KB pages of Apple Silicon); the protocol fixes
    // 511 bytes so the same rows fit on every device and server.
    let dir = TestDir::new("bounds_key_limit");
    let db = Db::open(dir.db_dir("db")).expect("open");
    db.define_table(TableDef::new("keys", "id").index("by_label", &["label"]))
        .expect("define");

    let long = "x".repeat(600);
    let key = Query::insert_into("keys", [json!({"id": long.clone()})]).execute(&db);
    assert!(
        matches!(
            key,
            Err(offline_first_core::engine::EngineError::KeyTooLarge { .. })
        ),
        "{key:?}"
    );
    let label = Query::insert_into("keys", [json!({"id": "ok", "label": long})]).execute(&db);
    assert!(
        matches!(
            label,
            Err(offline_first_core::engine::EngineError::KeyTooLarge { .. })
        ),
        "{label:?}"
    );
    // 508 bytes of text encode to 511 (tag + text + terminator): accepted in
    // a table without indexes. An index key also carries the primary key, so
    // `keys` above would reject it.
    db.define_table(TableDef::new("plain", "id"))
        .expect("define plain");
    let fits = Query::insert_into("plain", [json!({"id": "y".repeat(508)})]).execute(&db);
    assert!(fits.is_ok(), "{fits:?}");
    let over = Query::insert_into("plain", [json!({"id": "y".repeat(509)})]).execute(&db);
    assert!(
        matches!(
            over,
            Err(offline_first_core::engine::EngineError::KeyTooLarge { .. })
        ),
        "{over:?}"
    );
}

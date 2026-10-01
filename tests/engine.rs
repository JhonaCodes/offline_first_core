//! The query engine through its Rust API: statements, planner, indexes,
//! constraints, transactions and map growth.

mod common;

use common::TestDir;
use offline_first_core::engine::value::{field, total_cmp};
use offline_first_core::engine::{
    col, Aggregate, Db, EngineError, Expr, OpenOptions, OrderBy, Output, Query, Select, Statement,
    TableDef,
};
use serde_json::{json, Value};

fn open(dir: &TestDir, name: &str) -> Db {
    Db::open(dir.db_dir(name)).expect("open engine database")
}

fn skills_table() -> TableDef {
    TableDef::new("skills", "id")
        .index("by_language_priority", &["language", "priority"])
        .index("by_priority", &["priority"])
        .unique_index("by_slug", &["slug"])
}

fn skill(id: u32, language: &str, priority: i64, enabled: bool) -> Value {
    json!({
        "id": format!("s{id:03}"),
        "slug": format!("slug-{id}"),
        "language": language,
        "priority": priority,
        "enabled": enabled,
        "meta": {"stars": id % 7},
    })
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
}

fn seeded(db: &Db, count: u32) -> Vec<Value> {
    db.define_table(skills_table()).expect("define skills");
    let languages = ["rust", "dart", "go", "zig"];
    let mut rng = Lcg(42);
    let rows: Vec<Value> = (0..count)
        .map(|id| {
            let language = languages[rng.next(4) as usize];
            skill(id, language, rng.next(10) as i64, rng.next(2) == 0)
        })
        .collect();
    Query::insert_into("skills", rows.clone())
        .execute(db)
        .expect("seed rows");
    rows
}

/// What a query must return, computed in memory from `rows`.
fn oracle(rows: &[Value], query: &Select) -> Vec<Value> {
    let mut matched: Vec<Value> = rows
        .iter()
        .filter(|row| query.filter.as_ref().is_none_or(|f| f.matches(row)))
        .cloned()
        .collect();
    // Primary key order first, then the requested keys (a stable sort keeps
    // ties in primary key order, like the engine's scans).
    matched.sort_by(|a, b| total_cmp(&a["id"], &b["id"]));
    matched.sort_by(|a, b| {
        for key in &query.order {
            let x = field(a, &key.field).unwrap_or(&Value::Null);
            let y = field(b, &key.field).unwrap_or(&Value::Null);
            let ordering = if key.desc {
                total_cmp(y, x)
            } else {
                total_cmp(x, y)
            };
            if ordering.is_ne() {
                return ordering;
            }
        }
        std::cmp::Ordering::Equal
    });
    let offset = query.offset.unwrap_or(0) as usize;
    let limit = query.limit.map_or(usize::MAX, |l| l as usize);
    matched.into_iter().skip(offset).take(limit).collect()
}

fn ids(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .map(|row| row["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn test_insert_find_and_select_like_diesel() {
    let dir = TestDir::new("engine_basic");
    let db = open(&dir, "db");
    db.define_table(skills_table()).expect("define");

    let inserted = Query::insert_into(
        "skills",
        [skill(1, "rust", 5, true), skill(2, "dart", 9, true)],
    )
    .execute(&db)
    .expect("insert");
    assert_eq!(inserted, 2);

    let found = db.execute(&Query::find("skills", "s001")).expect("find");
    assert_eq!(found.rows(), vec![skill(1, "rust", 5, true)]);
    assert_eq!(
        db.execute(&Query::find("skills", "nope")).expect("find"),
        Output::Row(None)
    );

    let rows: Vec<Value> = Query::table("skills")
        .filter(col("enabled").eq(true))
        .order(col("priority").desc())
        .load(&db)
        .expect("load");
    assert_eq!(ids(&rows), ["s002", "s001"]);
}

#[test]
fn test_every_operator_matches_the_in_memory_oracle() {
    let dir = TestDir::new("engine_operators");
    let db = open(&dir, "db");
    let rows = seeded(&db, 120);
    let filters = [
        col("language").eq("rust"),
        col("language").ne("rust"),
        col("priority").gt(5),
        col("priority").ge(5),
        col("priority").lt(3),
        col("priority").le(3),
        col("priority").between(2, 6),
        col("priority").not_between(2, 6),
        col("language").eq_any(["go", "zig"]),
        col("language").ne_all(["go", "zig"]),
        col("language").eq_any(Vec::<&str>::new()),
        col("slug").like("slug-1%"),
        col("slug").ilike("SLUG-_"),
        col("missing").is_null(),
        col("meta.stars").is_not_null(),
        col("meta.stars").eq(3),
        col("language").eq("rust").and(col("priority").ge(4)),
        col("language").eq("go").or(col("priority").eq(0)),
        !col("enabled").eq(true),
        col("priority").gt("text"),
    ];
    for filter in filters {
        let query = Query::table("skills").filter(filter.clone());
        let got: Vec<Value> = db.load(&query).expect("load");
        let mut got_ids = ids(&got);
        got_ids.sort();
        let mut expected = ids(&oracle(&rows, &query));
        expected.sort();
        assert_eq!(got_ids, expected, "filter {filter:?}");
    }
}

#[test]
fn test_planned_queries_equal_full_scans() {
    let dir = TestDir::new("engine_planner");
    let db = open(&dir, "db");
    let rows = seeded(&db, 300);
    let mut rng = Lcg(7);
    let languages = ["rust", "dart", "go", "zig"];
    for _ in 0..200 {
        let language = languages[rng.next(4) as usize];
        let low = rng.next(10) as i64;
        let high = low + rng.next(5) as i64;
        let filter = match rng.next(6) {
            0 => col("language").eq(language),
            1 => col("language").eq(language).and(col("priority").ge(low)),
            2 => col("language")
                .eq(language)
                .and(col("priority").between(low, high)),
            3 => col("priority").gt(low).and(col("priority").le(high)),
            4 => col("id").ge(format!("s{:03}", low * 20)),
            _ => col("slug").eq(format!("slug-{}", rng.next(300))),
        };
        let order = match rng.next(4) {
            0 => vec![],
            1 => vec![col("priority").asc()],
            2 => vec![col("priority").desc()],
            _ => vec![col("id").desc()],
        };
        let mut query = Query::table("skills").filter(filter);
        query.order = order;
        if rng.next(2) == 0 {
            query.limit = Some(rng.next(15) + 1);
            query.offset = Some(rng.next(4));
        }
        let got: Vec<Value> = db.load(&query).expect("load");
        let expected = oracle(&rows, &query);
        if query.order.is_empty() {
            // No order: any matching rows, as many as the unordered result has.
            let all = oracle(
                &rows,
                &Select {
                    limit: None,
                    offset: None,
                    ..query.clone()
                },
            );
            let all_ids = ids(&all);
            assert_eq!(got.len(), expected.len(), "row count of {query:?}");
            assert!(
                ids(&got).iter().all(|id| all_ids.contains(id)),
                "only matching rows for {query:?}"
            );
            if query.limit.is_none() && query.offset.is_none() {
                let mut sorted = ids(&got);
                sorted.sort();
                assert_eq!(sorted, all_ids, "every matching row for {query:?}");
            }
        } else if query.order.iter().any(|key| key.field != "id") {
            // Ties in a non-unique sort key may come in any order: compare the
            // sort keys and the set of rows.
            let key = |rows: &[Value]| -> Vec<Value> {
                rows.iter()
                    .map(|row| row[&query.order[0].field].clone())
                    .collect()
            };
            assert_eq!(key(&got), key(&expected), "order of {query:?}");
            if query.limit.is_none() {
                let (mut a, mut b) = (ids(&got), ids(&expected));
                a.sort();
                b.sort();
                assert_eq!(a, b, "rows of {query:?}");
            }
        } else {
            assert_eq!(ids(&got), ids(&expected), "{query:?}");
        }
    }
}

#[test]
fn test_explain_uses_indexes() {
    let dir = TestDir::new("engine_explain");
    let db = open(&dir, "db");
    seeded(&db, 10);
    let plan = |query: Select| db.explain(&query).expect("explain");

    assert_eq!(
        plan(Query::table("skills").filter(col("id").eq("s001")))["access"],
        "primary_key_lookup"
    );
    let by_language = plan(
        Query::table("skills")
            .filter(col("language").eq("rust"))
            .filter(col("priority").gt(3))
            .order(col("priority").desc()),
    );
    assert_eq!(by_language["index"], "by_language_priority");
    assert_eq!(by_language["presorted"], true);
    assert_eq!(by_language["descending"], true);
    let top_k = plan(
        Query::table("skills")
            .order(col("priority").desc())
            .limit(3),
    );
    assert_eq!(top_k["index"], "by_priority");
    assert_eq!(
        plan(Query::table("skills").filter(col("enabled").eq(true)))["access"],
        "full_scan"
    );
}

#[test]
fn test_constraints_and_conflicts() {
    let dir = TestDir::new("engine_constraints");
    let db = open(&dir, "db");
    db.define_table(skills_table()).expect("define");
    Query::insert_into("skills", [skill(1, "rust", 1, true)])
        .execute(&db)
        .expect("insert");

    let duplicate = Query::insert_into("skills", [skill(1, "go", 2, true)]).execute(&db);
    assert!(
        matches!(duplicate, Err(EngineError::DuplicateKey { .. })),
        "{duplicate:?}"
    );

    let mut same_slug = skill(2, "go", 2, true);
    same_slug["slug"] = json!("slug-1");
    let violation = Query::insert_into("skills", [same_slug]).execute(&db);
    assert!(
        matches!(violation, Err(EngineError::UniqueViolation { .. })),
        "{violation:?}"
    );
    assert_eq!(
        Query::table("skills").count(&db).expect("count"),
        1,
        "a failed insert writes nothing"
    );

    // Several NULLs are allowed in a unique index.
    let mut no_slug_a = skill(3, "go", 1, true);
    no_slug_a["slug"] = Value::Null;
    let mut no_slug_b = skill(4, "go", 1, true);
    no_slug_b["slug"] = Value::Null;
    Query::insert_into("skills", [no_slug_a, no_slug_b])
        .execute(&db)
        .expect("null slugs");

    let ignored = Query::insert_into("skills", [skill(1, "zig", 9, false)])
        .on_conflict_do_nothing()
        .execute(&db)
        .expect("ignore");
    assert_eq!(ignored, 0);
    Query::insert_into("skills", [skill(1, "zig", 9, false)])
        .on_conflict_replace()
        .execute(&db)
        .expect("replace");
    let replaced: Vec<Value> = Query::table("skills")
        .filter(col("language").eq("zig"))
        .load(&db)
        .expect("load");
    assert_eq!(ids(&replaced), ["s001"]);
    let stale: Vec<Value> = Query::table("skills")
        .filter(col("language").eq("rust"))
        .load(&db)
        .expect("load");
    assert!(
        stale.is_empty(),
        "the replaced row left no stale index entry"
    );
}

#[test]
fn test_update_delete_and_expectations() {
    let dir = TestDir::new("engine_writes");
    let db = open(&dir, "db");
    seeded(&db, 50);
    let rust_count = Query::table("skills")
        .filter(col("language").eq("rust"))
        .count(&db)
        .expect("count");

    let updated = Query::update("skills")
        .filter(col("language").eq("rust"))
        .set("language", "rust-lang")
        .set("meta.reviewed", true)
        .execute(&db)
        .expect("update");
    assert_eq!(updated, rust_count);
    assert_eq!(
        Query::table("skills")
            .filter(col("language").eq("rust"))
            .count(&db)
            .expect("count"),
        0
    );
    assert_eq!(
        Query::table("skills")
            .filter(col("meta.reviewed").eq(true))
            .count(&db)
            .expect("count"),
        rust_count
    );

    let pk_change = Query::update("skills").set("id", "x").execute(&db);
    assert!(matches!(pk_change, Err(EngineError::InvalidRequest(_))));

    let mismatch = Query::delete("skills")
        .filter(col("language").eq("go"))
        .expect_affected_rows(9999)
        .execute(&db);
    assert!(matches!(
        mismatch,
        Err(EngineError::AffectedRowsMismatch { .. })
    ));
    let go_count = Query::table("skills")
        .filter(col("language").eq("go"))
        .count(&db)
        .expect("count");
    assert!(go_count > 0, "the failed delete deleted nothing");
    let deleted = Query::delete("skills")
        .filter(col("language").eq("go"))
        .execute(&db)
        .expect("delete");
    assert_eq!(deleted, go_count);
}

#[test]
fn test_auto_increment_and_aggregates() {
    let dir = TestDir::new("engine_auto");
    let db = open(&dir, "db");
    db.define_table(TableDef::new("orders", "id").auto_increment())
        .expect("define");
    let inserted: Vec<Value> =
        Query::insert_into("orders", [json!({"total": 10}), json!({"total": 32})])
            .get_results(&db)
            .expect("insert");
    assert_eq!(inserted[0]["id"], 1);
    assert_eq!(inserted[1]["id"], 2);
    Query::insert_into("orders", [json!({"id": 10, "total": 5})])
        .execute(&db)
        .expect("explicit id");
    let next: Vec<Value> = Query::insert_into("orders", [json!({"total": 1})])
        .get_results(&db)
        .expect("insert");
    assert_eq!(next[0]["id"], 11, "the sequence moves past explicit keys");

    let all = Query::table("orders");
    assert_eq!(
        all.aggregate(&db, Aggregate::Sum, "total").expect("sum"),
        json!(48)
    );
    assert_eq!(
        all.aggregate(&db, Aggregate::Min, "total").expect("min"),
        json!(1)
    );
    assert_eq!(
        all.aggregate(&db, Aggregate::Max, "total").expect("max"),
        json!(32)
    );
    assert_eq!(
        all.aggregate(&db, Aggregate::Avg, "total").expect("avg"),
        json!(12.0)
    );
    let none = Query::table("orders").filter(col("total").gt(1000));
    assert_eq!(
        none.aggregate(&db, Aggregate::Sum, "total").expect("sum"),
        Value::Null
    );
}

#[test]
fn test_transaction_commit_rollback_and_rollback_only() {
    let dir = TestDir::new("engine_tx");
    let db = open(&dir, "db");
    db.define_table(skills_table()).expect("define");

    let value = db
        .transaction(|tx| {
            Query::insert_into("skills", [skill(1, "rust", 1, true)]).execute_in(tx)?;
            let seen: Vec<Value> = Query::table("skills").load_in(tx)?;
            assert_eq!(seen.len(), 1, "read-your-writes");
            Ok(42)
        })
        .expect("commit");
    assert_eq!(value, 42);

    let rolled_back: Result<(), EngineError> = db.transaction(|tx| {
        Query::insert_into("skills", [skill(2, "go", 1, true)]).execute_in(tx)?;
        Err(EngineError::InvalidRequest("abort".into()))
    });
    assert!(rolled_back.is_err());
    assert_eq!(Query::table("skills").count(&db).expect("count"), 1);

    // An ignored failed write poisons the transaction.
    let poisoned = db.transaction(|tx| {
        Query::insert_into("skills", [skill(3, "go", 1, true)]).execute_in(tx)?;
        let _ignored = Query::insert_into("skills", [skill(1, "rust", 1, true)]).execute_in(tx);
        Ok(())
    });
    assert!(
        matches!(poisoned, Err(EngineError::TransactionAborted)),
        "{poisoned:?}"
    );
    assert_eq!(
        Query::table("skills").count(&db).expect("count"),
        1,
        "nothing of it was committed"
    );
}

#[test]
fn test_savepoints() {
    let dir = TestDir::new("engine_savepoints");
    let db = open(&dir, "db");
    db.define_table(skills_table()).expect("define");

    db.transaction(|tx| {
        Query::insert_into("skills", [skill(1, "rust", 1, true)]).execute_in(tx)?;
        // A failing optional block is rolled back alone.
        let optional = tx.savepoint(|sp| {
            Query::insert_into("skills", [skill(2, "go", 1, true)]).execute_in(sp)?;
            Query::insert_into("skills", [skill(1, "dup", 1, true)]).execute_in(sp)
        });
        assert!(matches!(optional, Err(EngineError::DuplicateKey { .. })));
        tx.savepoint(|sp| Query::insert_into("skills", [skill(3, "zig", 1, true)]).execute_in(sp))?;
        Ok(())
    })
    .expect("commit");
    let rows: Vec<Value> = Query::table("skills").load(&db).expect("load");
    assert_eq!(ids(&rows), ["s001", "s003"]);

    // A parent failure discards its successful savepoints.
    let failed: Result<(), EngineError> = db.transaction(|tx| {
        tx.savepoint(|sp| Query::insert_into("skills", [skill(4, "go", 1, true)]).execute_in(sp))?;
        Err(EngineError::InvalidRequest("parent fails".into()))
    });
    assert!(failed.is_err());
    assert_eq!(Query::table("skills").count(&db).expect("count"), 2);
}

#[test]
fn test_batch_is_atomic() {
    let dir = TestDir::new("engine_batch");
    let db = open(&dir, "db");
    db.define_table(skills_table()).expect("define");
    let batch = [
        Query::insert_into("skills", [skill(1, "rust", 1, true)]).into(),
        Query::update("skills")
            .filter(col("id").eq("s001"))
            .set("priority", 9)
            .into(),
        Query::insert_into("skills", [skill(1, "dup", 1, true)]).into(),
    ];
    assert!(matches!(
        db.batch(&batch),
        Err(EngineError::DuplicateKey { .. })
    ));
    assert_eq!(
        Query::table("skills").count(&db).expect("count"),
        0,
        "no statement of a failed batch persists"
    );

    let outputs = db.batch(&batch[..2]).expect("batch");
    assert_eq!(outputs.len(), 2);
    let row: Option<Value> = Query::table("skills").first(&db).expect("first");
    assert_eq!(row.expect("row")["priority"], 9);
}

#[test]
fn test_read_transaction_is_a_snapshot_and_reentrancy_is_rejected() {
    let dir = TestDir::new("engine_snapshot");
    let db = open(&dir, "db");
    db.define_table(skills_table()).expect("define");
    Query::insert_into("skills", [skill(1, "rust", 1, true)])
        .execute(&db)
        .expect("insert");

    let other = db.clone();
    db.read_transaction(|snapshot| {
        let writer = std::thread::spawn(move || {
            Query::insert_into("skills", [skill(2, "go", 1, true)])
                .execute(&other)
                .expect("insert")
        });
        writer.join().expect("writer thread");
        let count =
            |tx: &offline_first_core::engine::ReadTx<'_>| match tx.execute(&Statement::Count {
                table: "skills".into(),
                filter: None,
            }) {
                Ok(Output::Count(count)) => count,
                other => panic!("{other:?}"),
            };
        assert_eq!(
            count(snapshot),
            1,
            "the snapshot does not see the later commit"
        );
        Ok(())
    })
    .expect("snapshot");
    assert_eq!(Query::table("skills").count(&db).expect("count"), 2);

    let nested = db.transaction(|_| db.execute(&Query::find("skills", "s001")));
    assert!(matches!(nested, Err(EngineError::Reentrancy)), "{nested:?}");
}

#[test]
fn test_map_grows_and_respects_its_maximum() {
    let dir = TestDir::new("engine_growth");
    let options = OpenOptions {
        max_dbs: 16,
        initial_map_size: 256 << 10,
        max_map_size: 64 << 20,
        ..OpenOptions::default()
    };
    let db = Db::open_with(dir.db_dir("db"), options).expect("open");
    db.define_table(TableDef::new("blobs", "id"))
        .expect("define");
    let payload = "x".repeat(4096);
    for chunk in 0..20 {
        let rows: Vec<Value> = (0..100)
            .map(|i| json!({"id": chunk * 100 + i, "payload": payload}))
            .collect();
        Query::insert_into("blobs", rows)
            .execute(&db)
            .expect("insert beyond the initial map");
    }
    assert_eq!(Query::table("blobs").count(&db).expect("count"), 2000);
    assert!(db.map_size().expect("map size") > 256 << 10, "the map grew");

    let capped = Db::open_with(
        dir.db_dir("capped"),
        OpenOptions {
            max_dbs: 16,
            initial_map_size: 256 << 10,
            max_map_size: 256 << 10,
            ..OpenOptions::default()
        },
    )
    .expect("open");
    capped
        .define_table(TableDef::new("blobs", "id"))
        .expect("define");
    let rows: Vec<Value> = (0..200)
        .map(|i| json!({"id": i, "payload": payload}))
        .collect();
    let full = Query::insert_into("blobs", rows).execute(&capped);
    assert!(matches!(full, Err(EngineError::MapFull)), "{full:?}");
}

#[test]
fn test_schema_persists_and_indexes_are_built_on_existing_rows() {
    let dir = TestDir::new("engine_schema");
    {
        let db = open(&dir, "db");
        db.define_table(TableDef::new("people", "id"))
            .expect("define");
        Query::insert_into(
            "people",
            [
                json!({"id": 1, "email": "a@x"}),
                json!({"id": 2, "email": "b@x"}),
                json!({"id": 3, "email": "a@x"}),
            ],
        )
        .execute(&db)
        .expect("insert");
        let unique =
            db.define_table(TableDef::new("people", "id").unique_index("by_email", &["email"]));
        assert!(
            matches!(unique, Err(EngineError::UniqueViolation { .. })),
            "{unique:?}"
        );
        assert!(
            db.tables().expect("tables")[0].indexes.is_empty(),
            "the failed definition was rolled back"
        );
        db.define_table(TableDef::new("people", "id").index("by_email", &["email"]))
            .expect("add index");
        let changed = db
            .define_table(TableDef::new("other_pk", "id"))
            .expect("define");
        assert!(changed);
        let mismatch = db.define_table(TableDef::new("people", "email"));
        assert!(matches!(mismatch, Err(EngineError::SchemaMismatch { .. })));
    }
    let db = open(&dir, "db");
    let defs = db.tables().expect("tables");
    assert_eq!(defs.len(), 2);
    let query = Query::table("people").filter(col("email").eq("a@x"));
    assert_eq!(db.explain(&query).expect("explain")["index"], "by_email");
    let rows: Vec<Value> = query.load(&db).expect("load");
    assert_eq!(rows.len(), 2);
    assert!(db.drop_table("other_pk").expect("drop"));
    assert!(matches!(
        db.execute(&Query::find("other_pk", 1)),
        Err(EngineError::TableNotFound(_))
    ));
}

#[test]
fn test_statements_round_trip_as_json() {
    let statement: Statement = Query::table("skills")
        .filter(col("priority").between(1, 3).and(!col("slug").is_null()))
        .order(OrderBy {
            field: "priority".into(),
            desc: true,
        })
        .limit(5)
        .into();
    let text = serde_json::to_string(&statement).expect("serialize");
    assert!(
        text.contains(r#""op":"select""#) && text.contains(r#""op":"between""#),
        "{text}"
    );
    let back: Statement = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(back, statement);
    let expr: Expr =
        serde_json::from_value(json!({"op": "eq_any", "field": "a", "values": [1, 2]}))
            .expect("expr");
    assert!(expr.matches(&json!({"a": 2})));
}

#[test]
fn test_relaxed_durability_still_persists_on_close() {
    let dir = TestDir::new("engine_durability");
    let options = OpenOptions {
        durability: offline_first_core::engine::Durability::NoSync,
        ..OpenOptions::default()
    };
    {
        let db = Db::open_with(dir.db_dir("db"), options).expect("open");
        db.define_table(TableDef::new("t", "id")).expect("define");
        Query::insert_into("t", [json!({"id": 1})])
            .execute(&db)
            .expect("insert");
    }
    let db = open(&dir, "db");
    assert_eq!(Query::table("t").count(&db).expect("count"), 1);
}

#[test]
fn test_or_filter_and_offset_like_diesel() {
    let dir = TestDir::new("or_filter_offset");
    let db = open(&dir, "db");
    db.define_table(TableDef::new("n", "id")).expect("define");
    Query::insert_into(
        "n",
        (1..=10i64).map(|id| json!({"id": id, "even": id % 2 == 0})),
    )
    .execute(&db)
    .expect("insert");

    // `or_filter` ORs with everything so far: (id <= 2 AND even) OR id = 9.
    let rows: Vec<Value> = Query::table("n")
        .filter(col("id").le(2))
        .filter(col("even").eq(true))
        .or_filter(col("id").eq(9))
        .order(col("id").asc())
        .load(&db)
        .expect("load");
    let ids: Vec<i64> = rows
        .iter()
        .map(|row| row["id"].as_i64().expect("id"))
        .collect();
    assert_eq!(ids, [2, 9]);

    // `offset` skips rows after the order, before the limit.
    let page: Vec<Value> = Query::table("n")
        .order(col("id").desc())
        .offset(3)
        .limit(2)
        .load(&db)
        .expect("page");
    let ids: Vec<i64> = page
        .iter()
        .map(|row| row["id"].as_i64().expect("id"))
        .collect();
    assert_eq!(ids, [7, 6]);
}

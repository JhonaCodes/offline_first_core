//! Protocol v1 relational additions: `select` projection/distinct, `group`,
//! `join`, and `update` increment. Oracles are computed independently in
//! plain Rust from the seeded rows, per the protocol specification
//! (`PROTOCOL.md` of db_dsl).

mod common;

use std::collections::HashSet;

use common::TestDir;
use offline_first_core::engine::{col, Db, EngineError, Query, Statement, TableDef};
use serde_json::{json, Value};

fn open(dir: &TestDir, name: &str) -> Db {
    Db::open(dir.db_dir(name)).expect("open engine database")
}

// ---------------------------------------------------------------------------
// Projection and distinct (`select`)
// ---------------------------------------------------------------------------

fn people_table() -> TableDef {
    TableDef::new("people", "id")
}

#[test]
fn test_projection_places_nested_values_and_defaults_missing_to_null() {
    let dir = TestDir::new("rel_projection");
    let db = open(&dir, "db");
    db.define_table(people_table()).expect("define");
    Query::insert_into(
        "people",
        [
            json!({"id": 1, "city": "Bogota", "meta": {"stars": 3}}),
            json!({"id": 2, "meta": {"stars": 5}}),
            json!({"id": 3, "city": "Lima"}),
        ],
    )
    .execute(&db)
    .expect("insert");

    let rows: Vec<Value> = Query::table("people")
        .select(["city", "meta.stars"])
        .order(col("id").asc())
        .load(&db)
        .expect("load");

    assert_eq!(
        rows,
        vec![
            json!({"city": "Bogota", "meta": {"stars": 3}}),
            json!({"city": Value::Null, "meta": {"stars": 5}}),
            json!({"city": "Lima", "meta": {"stars": Value::Null}}),
        ]
    );
    // Projection never leaks fields outside `fields`.
    for row in &rows {
        let object = row.as_object().expect("object");
        assert_eq!(
            object.keys().collect::<HashSet<_>>(),
            HashSet::from([&"city".to_string(), &"meta".to_string()])
        );
    }
}

#[test]
fn test_projection_rejects_empty_repeated_and_prefix_paths() {
    let dir = TestDir::new("rel_projection_invalid");
    let db = open(&dir, "db");
    db.define_table(people_table()).expect("define");
    Query::insert_into("people", [json!({"id": 1, "city": "Bogota"})])
        .execute(&db)
        .expect("insert");

    let empty = Query::table("people").select([""]).load::<Value>(&db);
    assert!(
        matches!(empty, Err(EngineError::InvalidRequest(_))),
        "{empty:?}"
    );

    let repeated = Query::table("people")
        .select(["city", "city"])
        .load::<Value>(&db);
    assert!(
        matches!(repeated, Err(EngineError::InvalidRequest(_))),
        "{repeated:?}"
    );

    let prefix = Query::table("people")
        .select(["a", "a.b"])
        .load::<Value>(&db);
    assert!(
        matches!(prefix, Err(EngineError::InvalidRequest(_))),
        "{prefix:?}"
    );
}

#[test]
fn test_distinct_with_fields_treats_1_and_1_0_as_equal_and_keeps_first() {
    let dir = TestDir::new("rel_distinct_fields");
    let db = open(&dir, "db");
    db.define_table(people_table()).expect("define");
    Query::insert_into(
        "people",
        [
            json!({"id": 1, "score": 1}),
            json!({"id": 2, "score": 1.0}),
            json!({"id": 3, "score": -0.0}),
            json!({"id": 4, "score": 0}),
            json!({"id": 5, "score": 2}),
        ],
    )
    .execute(&db)
    .expect("insert");

    let rows: Vec<Value> = Query::table("people")
        .select(["score"])
        .distinct()
        .order(col("id").asc())
        .load(&db)
        .expect("load");

    // 1 == 1.0 (first kept: id 1's score) and -0.0 == 0 (first kept: id 3's score).
    assert_eq!(
        rows,
        vec![
            json!({"score": 1}),
            json!({"score": -0.0}),
            json!({"score": 2})
        ]
    );
}

#[test]
fn test_distinct_without_fields_compares_whole_rows() {
    let dir = TestDir::new("rel_distinct_whole_row");
    let db = open(&dir, "db");
    db.define_table(people_table()).expect("define");
    // Different primary keys make every whole row distinct, even with the
    // same remaining content: distinct is not a no-op filter over content.
    Query::insert_into(
        "people",
        [
            json!({"id": 1, "city": "Bogota"}),
            json!({"id": 2, "city": "Bogota"}),
        ],
    )
    .execute(&db)
    .expect("insert");
    let rows: Vec<Value> = Query::table("people")
        .distinct()
        .order(col("id").asc())
        .load(&db)
        .expect("load");
    assert_eq!(rows.len(), 2, "rows differ by id, so both survive distinct");
}

#[test]
fn test_projection_and_distinct_evaluate_after_filter_and_order() {
    let dir = TestDir::new("rel_projection_order");
    let db = open(&dir, "db");
    db.define_table(people_table()).expect("define");
    Query::insert_into(
        "people",
        [
            json!({"id": 1, "city": "Lima", "age": 30}),
            json!({"id": 2, "city": "Bogota", "age": 40}),
            json!({"id": 3, "city": "Quito", "age": 20}),
        ],
    )
    .execute(&db)
    .expect("insert");

    let rows: Vec<Value> = Query::table("people")
        .filter(col("age").ge(25))
        .select(["city"])
        .order(col("age").asc())
        .load(&db)
        .expect("load");
    // Order applies to the stored rows (by `age`), before projecting `city`.
    assert_eq!(
        rows,
        vec![json!({"city": "Lima"}), json!({"city": "Bogota"})]
    );
}

// ---------------------------------------------------------------------------
// `group`
// ---------------------------------------------------------------------------

fn scores_table() -> TableDef {
    TableDef::new("scores", "id")
}

fn seed_scores(db: &Db) {
    db.define_table(scores_table()).expect("define");
    Query::insert_into(
        "scores",
        [
            json!({"id": 1, "city": "Bogota", "age": 30, "score": 1.5, "email": "a@x"}),
            json!({"id": 2, "city": "Bogota", "age": 40, "score": 2.5}),
            json!({"id": 3, "city": "Lima", "age": 20, "score": 3.0, "email": "c@x"}),
            json!({"id": 4, "age": 50, "score": 4.0, "email": "d@x"}), // no `city`: null group
            json!({"id": 5, "age": 10, "score": 5.0}),                 // no `city`: null group
        ],
    )
    .execute(db)
    .expect("insert");
}

#[test]
fn test_group_by_city_with_count_sum_avg_min_max() {
    let dir = TestDir::new("rel_group_basic");
    let db = open(&dir, "db");
    seed_scores(&db);

    let rows: Vec<Value> = Query::group("scores")
        .by("city")
        .count("people")
        .count_of("email", "with_email")
        .sum("age", "total_age")
        .avg("age", "avg_age")
        .min("age", "youngest")
        .max("age", "oldest")
        .order(col("city").asc())
        .load(&db)
        .expect("load");

    // Oracle computed independently over the seeded rows.
    assert_eq!(
        rows,
        vec![
            json!({
                "city": Value::Null, "people": 2, "with_email": 1,
                "total_age": 60, "avg_age": 30.0, "youngest": 10, "oldest": 50
            }),
            json!({
                "city": "Bogota", "people": 2, "with_email": 1,
                "total_age": 70, "avg_age": 35.0, "youngest": 30, "oldest": 40
            }),
            json!({
                "city": "Lima", "people": 1, "with_email": 1,
                "total_age": 20, "avg_age": 20.0, "youngest": 20, "oldest": 20
            }),
        ],
        "null forms its own group, as in SQL"
    );
}

#[test]
fn test_group_empty_by_is_one_group_even_with_no_matching_rows() {
    let dir = TestDir::new("rel_group_empty_by");
    let db = open(&dir, "db");
    seed_scores(&db);

    let all: Vec<Value> = Query::group("scores")
        .count("total")
        .sum("age", "total_age")
        .load(&db)
        .expect("load");
    assert_eq!(all, vec![json!({"total": 5, "total_age": 150})]);

    let none: Vec<Value> = Query::group("scores")
        .filter(col("age").gt(1000))
        .count("total")
        .sum("age", "total_age")
        .min("age", "youngest")
        .load(&db)
        .expect("load");
    assert_eq!(
        none,
        vec![json!({"total": 0, "total_age": Value::Null, "youngest": Value::Null})],
        "the group exists even when nothing matches"
    );
}

#[test]
fn test_group_non_empty_by_with_no_matches_has_no_groups() {
    let dir = TestDir::new("rel_group_no_groups");
    let db = open(&dir, "db");
    seed_scores(&db);
    let rows: Vec<Value> = Query::group("scores")
        .filter(col("age").gt(1000))
        .by("city")
        .count("total")
        .load(&db)
        .expect("load");
    assert!(rows.is_empty(), "{rows:?}");
}

#[test]
fn test_group_having_filters_output_rows() {
    let dir = TestDir::new("rel_group_having");
    let db = open(&dir, "db");
    seed_scores(&db);
    let rows: Vec<Value> = Query::group("scores")
        .by("city")
        .count("people")
        .having(col("people").ge(2))
        .order(col("city").asc())
        .load(&db)
        .expect("load");
    let cities: Vec<Value> = rows.iter().map(|r| r["city"].clone()).collect();
    assert_eq!(cities, vec![Value::Null, json!("Bogota")]);
}

#[test]
fn test_group_sum_overflow_switches_to_f64() {
    let dir = TestDir::new("rel_group_overflow");
    let db = open(&dir, "db");
    db.define_table(TableDef::new("big", "id")).expect("define");
    Query::insert_into(
        "big",
        [
            json!({"id": 1, "g": "a", "n": i64::MAX}),
            json!({"id": 2, "g": "a", "n": 1}),
        ],
    )
    .execute(&db)
    .expect("insert");
    let rows: Vec<Value> = Query::group("big")
        .by("g")
        .sum("n", "total")
        .load(&db)
        .expect("load");
    let total = rows[0]["total"].as_f64().expect("f64 sum after overflow");
    assert!(
        (total - (i64::MAX as f64 + 1.0)).abs() < 1.0,
        "overflowed sum falls back to f64: {total}"
    );
}

#[test]
fn test_group_alias_validation() {
    let dir = TestDir::new("rel_group_alias");
    let db = open(&dir, "db");
    seed_scores(&db);

    let empty_alias = Query::group("scores").count("").load::<Value>(&db);
    assert!(
        matches!(empty_alias, Err(EngineError::InvalidRequest(_))),
        "{empty_alias:?}"
    );

    let dotted_alias = Query::group("scores").count("a.b").load::<Value>(&db);
    assert!(
        matches!(dotted_alias, Err(EngineError::InvalidRequest(_))),
        "{dotted_alias:?}"
    );

    let duplicate_alias = Query::group("scores")
        .count("total")
        .sum("age", "total")
        .load::<Value>(&db);
    assert!(
        matches!(duplicate_alias, Err(EngineError::InvalidRequest(_))),
        "{duplicate_alias:?}"
    );

    let collides_with_by = Query::group("scores")
        .by("city")
        .count("city")
        .load::<Value>(&db);
    assert!(
        matches!(collides_with_by, Err(EngineError::InvalidRequest(_))),
        "{collides_with_by:?}"
    );
}

// ---------------------------------------------------------------------------
// `join`
// ---------------------------------------------------------------------------

fn users_posts(db: &Db) {
    db.define_table(TableDef::new("users", "id"))
        .expect("define users");
    db.define_table(TableDef::new("posts", "id").index("by_author", &["author_id"]))
        .expect("define posts");
    Query::insert_into(
        "users",
        [
            json!({"id": 1, "name": "ana"}),
            json!({"id": 2, "name": "bob"}),
            json!({"id": 3, "name": "cleo"}), // no posts
        ],
    )
    .execute(db)
    .expect("insert users");
    Query::insert_into(
        "posts",
        [
            json!({"id": 10, "author_id": 1, "title": "p1"}),
            json!({"id": 11, "author_id": 1, "title": "p2"}), // multiplicity for ana
            json!({"id": 12, "author_id": 2, "title": "p3"}),
            json!({"id": 13, "author_id": null, "title": "orphan"}), // never matches
        ],
    )
    .execute(db)
    .expect("insert posts");
}

#[test]
fn test_inner_join_excludes_unmatched_and_keeps_multiplicity() {
    let dir = TestDir::new("rel_join_inner");
    let db = open(&dir, "db");
    users_posts(&db);

    let rows: Vec<Value> = Query::join("users")
        .alias("u")
        .inner_join_as("posts", "p", "u.id", "author_id")
        .order(col("u.id").asc())
        .then_order_by(col("p.id").asc())
        .load(&db)
        .expect("load");

    let pairs: Vec<(Value, Value)> = rows
        .iter()
        .map(|r| (r["u"]["name"].clone(), r["p"]["title"].clone()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            (json!("ana"), json!("p1")),
            (json!("ana"), json!("p2")),
            (json!("bob"), json!("p3")),
        ],
        "cleo has no posts (excluded), ana has two (multiplicity)"
    );
}

#[test]
fn test_left_join_keeps_unmatched_rows_with_null() {
    let dir = TestDir::new("rel_join_left");
    let db = open(&dir, "db");
    users_posts(&db);

    let rows: Vec<Value> = Query::join("users")
        .alias("u")
        .left_join_as("posts", "p", "u.id", "author_id")
        .order(col("u.id").asc())
        .then_order_by(col("p.id").asc())
        .load(&db)
        .expect("load");

    let pairs: Vec<(Value, Value)> = rows
        .iter()
        .map(|r| (r["u"]["name"].clone(), r["p"].clone()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            (
                json!("ana"),
                json!({"id": 10, "author_id": 1, "title": "p1"})
            ),
            (
                json!("ana"),
                json!({"id": 11, "author_id": 1, "title": "p2"})
            ),
            (
                json!("bob"),
                json!({"id": 12, "author_id": 2, "title": "p3"})
            ),
            (json!("cleo"), Value::Null),
        ]
    );
}

#[test]
fn test_join_never_matches_on_null_keys_on_either_side() {
    let dir = TestDir::new("rel_join_null_keys");
    let db = open(&dir, "db");
    users_posts(&db);
    // No user has `ref_id`, so the left value is always missing/null: never
    // matches the orphan post's `author_id: null` either.
    let rows: Vec<Value> = Query::join("users")
        .alias("u")
        .left_join_as("posts", "p", "u.ref_id", "author_id")
        .load(&db)
        .expect("load");
    assert!(
        rows.iter().all(|r| r["p"] == Value::Null),
        "null/missing never matches: {rows:?}"
    );
    assert_eq!(rows.len(), 3, "every user still appears once (left join)");
}

#[test]
fn test_join_matches_the_same_with_or_without_an_index_on_the_right_side() {
    let dir = TestDir::new("rel_join_index");
    let db = open(&dir, "db");
    db.define_table(TableDef::new("users", "id"))
        .expect("define users");
    // No index on `posts.author_id` this time.
    db.define_table(TableDef::new("posts", "id"))
        .expect("define posts");
    Query::insert_into(
        "users",
        [
            json!({"id": 1, "name": "ana"}),
            json!({"id": 2, "name": "bob"}),
        ],
    )
    .execute(&db)
    .expect("insert users");
    Query::insert_into(
        "posts",
        [
            json!({"id": 10, "author_id": 1, "title": "p1"}),
            json!({"id": 11, "author_id": 2, "title": "p2"}),
        ],
    )
    .execute(&db)
    .expect("insert posts");

    let rows: Vec<Value> = Query::join("users")
        .alias("u")
        .inner_join_as("posts", "p", "u.id", "author_id")
        .order(col("u.id").asc())
        .load(&db)
        .expect("load");
    let titles: Vec<Value> = rows.iter().map(|r| r["p"]["title"].clone()).collect();
    assert_eq!(titles, vec![json!("p1"), json!("p2")]);
}

#[test]
fn test_join_empty_joins_wraps_rows_under_the_alias() {
    let dir = TestDir::new("rel_join_empty");
    let db = open(&dir, "db");
    users_posts(&db);
    let rows: Vec<Value> = Query::join("users").alias("u").load(&db).expect("load");
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert!(row.get("u").is_some(), "{row:?}");
    }
}

#[test]
fn test_join_unknown_table_and_duplicate_alias() {
    let dir = TestDir::new("rel_join_errors");
    let db = open(&dir, "db");
    users_posts(&db);

    let unknown = Query::join("nope").load::<Value>(&db);
    assert!(
        matches!(unknown, Err(EngineError::TableNotFound(_))),
        "{unknown:?}"
    );

    let duplicate = Query::join("users")
        .alias("u")
        .inner_join_as("posts", "u", "u.id", "author_id")
        .load::<Value>(&db);
    assert!(
        matches!(duplicate, Err(EngineError::InvalidRequest(_))),
        "{duplicate:?}"
    );

    let empty_alias = Query::join("users").alias("").load::<Value>(&db);
    assert!(
        matches!(empty_alias, Err(EngineError::InvalidRequest(_))),
        "{empty_alias:?}"
    );
}

#[test]
fn test_join_filter_and_order_use_combined_row_paths() {
    let dir = TestDir::new("rel_join_filter");
    let db = open(&dir, "db");
    users_posts(&db);
    let rows: Vec<Value> = Query::join("users")
        .alias("u")
        .inner_join_as("posts", "p", "u.id", "author_id")
        .filter(col("p.title").eq("p2"))
        .load(&db)
        .expect("load");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["u"]["name"], json!("ana"));
}

// ---------------------------------------------------------------------------
// `update` with `increment`
// ---------------------------------------------------------------------------

fn pages_table() -> TableDef {
    TableDef::new("pages", "id")
}

#[test]
fn test_increment_missing_field_defaults_to_zero() {
    let dir = TestDir::new("rel_increment_missing");
    let db = open(&dir, "db");
    db.define_table(pages_table()).expect("define");
    Query::insert_into("pages", [json!({"id": 1})])
        .execute(&db)
        .expect("insert");
    Query::update("pages")
        .filter(col("id").eq(1))
        .increment("views", 5)
        .execute(&db)
        .expect("increment");
    let row: Value = db
        .execute(&Query::find("pages", 1))
        .expect("find")
        .rows()
        .remove(0);
    assert_eq!(row["views"], json!(5));
}

#[test]
fn test_increment_two_integers_stay_integer_others_become_float() {
    let dir = TestDir::new("rel_increment_kinds");
    let db = open(&dir, "db");
    db.define_table(pages_table()).expect("define");
    Query::insert_into("pages", [json!({"id": 1, "views": 10, "score": 1})])
        .execute(&db)
        .expect("insert");
    Query::update("pages")
        .filter(col("id").eq(1))
        .increment("views", 5)
        .increment("score", 0.5)
        .execute(&db)
        .expect("increment");
    let row: Value = db
        .execute(&Query::find("pages", 1))
        .expect("find")
        .rows()
        .remove(0);
    assert_eq!(row["views"], json!(15));
    assert_eq!(row["score"], json!(1.5));
}

#[test]
fn test_increment_overflow_is_invalid_request_and_writes_nothing() {
    let dir = TestDir::new("rel_increment_overflow");
    let db = open(&dir, "db");
    db.define_table(pages_table()).expect("define");
    Query::insert_into("pages", [json!({"id": 1, "views": i64::MAX})])
        .execute(&db)
        .expect("insert");
    let result = Query::update("pages")
        .filter(col("id").eq(1))
        .increment("views", 1)
        .execute(&db);
    assert!(
        matches!(result, Err(EngineError::InvalidRequest(_))),
        "{result:?}"
    );
    let row: Value = db
        .execute(&Query::find("pages", 1))
        .expect("find")
        .rows()
        .remove(0);
    assert_eq!(row["views"], json!(i64::MAX), "nothing was written");
}

#[test]
fn test_increment_to_a_non_finite_number_is_invalid_request_and_writes_nothing() {
    let dir = TestDir::new("rel_increment_non_finite");
    let db = open(&dir, "db");
    db.define_table(pages_table()).expect("define");
    Query::insert_into("pages", [json!({"id": 1, "score": 1e308})])
        .execute(&db)
        .expect("insert");
    let result = Query::update("pages")
        .filter(col("id").eq(1))
        .increment("score", 1e308)
        .execute(&db);
    assert!(
        matches!(result, Err(EngineError::InvalidRequest(_))),
        "{result:?}"
    );
    let row: Value = db
        .execute(&Query::find("pages", 1))
        .expect("find")
        .rows()
        .remove(0);
    assert_eq!(row["score"], json!(1e308), "nothing was written");
}

#[test]
fn test_increment_non_number_current_value_is_invalid_request_and_writes_nothing() {
    let dir = TestDir::new("rel_increment_non_number");
    let db = open(&dir, "db");
    db.define_table(pages_table()).expect("define");
    Query::insert_into("pages", [json!({"id": 1, "views": "not a number"})])
        .execute(&db)
        .expect("insert");
    let result = Query::update("pages")
        .filter(col("id").eq(1))
        .increment("views", 1)
        .execute(&db);
    assert!(
        matches!(result, Err(EngineError::InvalidRequest(_))),
        "{result:?}"
    );
    let row: Value = db
        .execute(&Query::find("pages", 1))
        .expect("find")
        .rows()
        .remove(0);
    assert_eq!(row["views"], json!("not a number"), "nothing was written");
}

#[test]
fn test_increment_and_set_on_the_same_path_is_invalid_request() {
    let dir = TestDir::new("rel_increment_overlap");
    let db = open(&dir, "db");
    db.define_table(pages_table()).expect("define");
    Query::insert_into("pages", [json!({"id": 1, "views": 1})])
        .execute(&db)
        .expect("insert");
    let result = Query::update("pages")
        .filter(col("id").eq(1))
        .set("views", 100)
        .increment("views", 1)
        .execute(&db);
    assert!(
        matches!(result, Err(EngineError::InvalidRequest(_))),
        "{result:?}"
    );
}

#[test]
fn test_increment_on_primary_key_is_invalid_request() {
    let dir = TestDir::new("rel_increment_pk");
    let db = open(&dir, "db");
    db.define_table(pages_table()).expect("define");
    Query::insert_into("pages", [json!({"id": 1})])
        .execute(&db)
        .expect("insert");
    let result = Query::update("pages")
        .filter(col("id").eq(1))
        .increment("id", 1)
        .execute(&db);
    assert!(
        matches!(result, Err(EngineError::InvalidRequest(_))),
        "{result:?}"
    );
}

#[test]
fn test_statements_new_ops_round_trip_as_json() {
    let group: Statement = Query::group("scores")
        .by("city")
        .count("people")
        .having(col("people").ge(1))
        .into();
    let text = serde_json::to_string(&group).expect("serialize");
    assert!(text.contains(r#""op":"group""#), "{text}");
    let back: Statement = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(back, group);

    let join: Statement = Query::join("users")
        .alias("u")
        .inner_join_as("posts", "p", "u.id", "author_id")
        .into();
    let text = serde_json::to_string(&join).expect("serialize");
    assert!(text.contains(r#""op":"join""#), "{text}");
    let back: Statement = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(back, join);
}

#[test]
fn a_join_explains_its_strategy_without_running() {
    let dir = TestDir::new("join_explain");
    let db = open(&dir, "db");
    db.define_table(TableDef::new("users", "id"))
        .expect("users");
    db.define_table(TableDef::new("posts", "id"))
        .expect("posts");

    let plan = Query::join("users")
        .alias("u")
        .inner_join_as("posts", "p", "u.id", "author_id")
        .explain(&db)
        .expect("explain");

    assert_eq!(
        plan,
        json!({
            "strategy": "hash_join",
            "tables": [
                {"table": "users", "as": "u", "access": "full_scan"},
                {"table": "posts", "as": "p", "access": "full_scan"}
            ],
            "filter": "none"
        })
    );

    // A repeated alias is refused here too, as when the join runs.
    let repeated = Query::join("users")
        .alias("u")
        .inner_join_as("posts", "u", "u.id", "author_id")
        .explain(&db);
    assert!(
        matches!(repeated, Err(EngineError::InvalidRequest(_))),
        "{repeated:?}"
    );
}

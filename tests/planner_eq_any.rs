//! `eq_any` reads only the rows it names: point lookups on the primary key,
//! and one index range per value on the leading field of an index, instead
//! of a full scan. The rows are the same as any plan gives.

mod common;

use common::TestDir;
use offline_first_core::engine::{col, Db, Query, TableDef};
use serde_json::{json, Value};

fn db(dir: &TestDir) -> Db {
    let db = Db::open(dir.db_dir("db")).expect("open");
    db.define_table(TableDef::new("posts", "id").index("by_author", &["author_id"]))
        .expect("define");
    Query::insert_into(
        "posts",
        (1..=200i64).map(|id| json!({"id": id, "author_id": id % 20, "views": id * 3})),
    )
    .execute(&db)
    .expect("insert");
    db
}

fn ids(rows: &[Value]) -> Vec<i64> {
    rows.iter()
        .map(|row| row["id"].as_i64().expect("id"))
        .collect()
}

#[test]
fn eq_any_on_the_primary_key_looks_the_rows_up() {
    let dir = TestDir::new("eq_any_pk");
    let db = db(&dir);
    // Unordered, repeated, and one key that does not exist.
    let query = Query::table("posts").filter(col("id").eq_any([150, 3, 999, 3, 42]));

    let plan = db.explain(&query).expect("explain");
    assert_eq!(plan["access"], "primary_key_lookup", "{plan}");
    assert_eq!(plan["index"], Value::Null, "{plan}");

    let rows: Vec<Value> = query.load(&db).expect("load");
    assert_eq!(ids(&rows), [3, 42, 150], "in primary key order, each once");
    assert_eq!(query.count(&db).expect("count"), 3);
}

#[test]
fn eq_any_on_the_leading_field_of_an_index_scans_one_range_per_value() {
    let dir = TestDir::new("eq_any_index");
    let db = db(&dir);
    let query = Query::table("posts")
        .filter(col("author_id").eq_any([7, 2, 7]))
        .order(col("id").asc());

    let plan = db.explain(&query).expect("explain");
    assert_eq!(plan["access"], "index_scan", "{plan}");
    assert_eq!(plan["index"], "by_author", "{plan}");

    let rows: Vec<Value> = query.load(&db).expect("load");
    let expected: Vec<i64> = (1..=200).filter(|id| [2, 7].contains(&(id % 20))).collect();
    assert_eq!(ids(&rows), expected);
}

#[test]
fn eq_any_with_more_conditions_checks_every_row() {
    let dir = TestDir::new("eq_any_more");
    let db = db(&dir);
    let query = Query::table("posts")
        .filter(col("author_id").eq_any([1, 5]).and(col("views").gt(300)))
        .order(col("id").desc())
        .limit(3);

    let rows: Vec<Value> = query.load(&db).expect("load");
    let mut expected: Vec<i64> = (1..=200)
        .filter(|id| [1, 5].contains(&(id % 20)) && id * 3 > 300)
        .collect();
    expected.reverse();
    expected.truncate(3);
    assert_eq!(ids(&rows), expected);
}

#[test]
fn eq_any_with_null_or_composite_values_still_answers_correctly() {
    let dir = TestDir::new("eq_any_null");
    let db = db(&dir);
    // NULL matches no row, as in SQL; an object never equals a number.
    let query =
        Query::table("posts").filter(col("id").eq_any([json!(null), json!({"a": 1}), json!(9)]));

    let rows: Vec<Value> = query.load(&db).expect("load");
    assert_eq!(ids(&rows), [9]);
}

//! Edge cases the engine must handle exactly: keys with NUL and Unicode, and
//! running out of LMDB databases (tables plus indexes).

mod common;

use common::TestDir;
use offline_first_core::engine::{col, Db, EngineError, OpenOptions, Output, Query, TableDef};
use serde_json::{json, Value};

/// Keys a byte-wise store gets wrong when it cuts at a NUL or compares
/// characters instead of bytes. `e\u{301}` (an `e` and a combining accent)
/// and `é` (precomposed) look the same and are different keys.
const KEYS: [&str; 9] = [
    "a", "a\u{0}", "a\u{0}b", "\u{0}", "ñ", "e\u{301}", "é", "😀", "日本",
];

fn open(dir: &TestDir, max_dbs: u32) -> Db {
    Db::open_with(
        dir.db_dir("db"),
        OpenOptions {
            max_dbs,
            ..OpenOptions::default()
        },
    )
    .expect("open must succeed")
}

fn find(db: &Db, table: &str, key: &str) -> Option<Value> {
    match db
        .execute(&Query::find(table, key))
        .expect("find must succeed")
    {
        Output::Row(row) => row.map(|bytes| serde_json::from_slice(&bytes).expect("stored JSON")),
        other => panic!("find answered {other:?}"),
    }
}

#[test]
fn keys_with_nul_and_unicode_are_found_exactly_and_ordered_by_bytes() {
    let dir = TestDir::new("unicode_keys");
    let db = open(&dir, 16);
    db.define_table(TableDef::new("t", "id"))
        .expect("define must succeed");
    Query::insert_into("t", KEYS.map(|key| json!({"id": key, "key": key})))
        .execute(&db)
        .expect("insert must succeed");

    // Each key finds its own row, never one that shares a prefix.
    for key in KEYS {
        let row = find(&db, "t", key).unwrap_or_else(|| panic!("{key:?} must be found"));
        assert_eq!(row["key"], json!(key), "{key:?}");
    }
    assert_eq!(find(&db, "t", "a\u{0}c"), None);

    // Ascending order is the byte order of the UTF-8, as Rust sorts strings.
    let ordered: Vec<Value> = Query::table("t")
        .order(col("id").asc())
        .load(&db)
        .expect("load must succeed");
    let mut expected = KEYS.to_vec();
    expected.sort_unstable();
    let ids: Vec<&str> = ordered
        .iter()
        .map(|row| row["id"].as_str().expect("string id"))
        .collect();
    assert_eq!(ids, expected);
}

#[test]
fn an_index_on_text_with_nul_matches_whole_values_only() {
    let dir = TestDir::new("unicode_index");
    let db = open(&dir, 16);
    db.define_table(TableDef::new("t", "id").index("by_name", &["name"]))
        .expect("define must succeed");
    Query::insert_into(
        "t",
        [
            json!({"id": 1, "name": "x"}),
            json!({"id": 2, "name": "x\u{0}y"}),
            json!({"id": 3, "name": "x\u{0}"}),
            json!({"id": 4, "name": "xy"}),
        ],
    )
    .execute(&db)
    .expect("insert must succeed");

    for (name, id) in [("x", 1), ("x\u{0}y", 2), ("x\u{0}", 3), ("xy", 4)] {
        let rows: Vec<Value> = Query::table("t")
            .filter(col("name").eq(name))
            .load(&db)
            .expect("load must succeed");
        let ids: Vec<i64> = rows
            .iter()
            .map(|row| row["id"].as_i64().expect("id"))
            .collect();
        assert_eq!(ids, [id], "{name:?}");
    }
}

#[test]
fn running_out_of_databases_is_an_error_that_defines_nothing() {
    let dir = TestDir::new("max_dbs");
    // 8 LMDB databases: the key-value one and the catalog take 2, so two
    // tables with two indexes each (3 databases per table) fit.
    let db = open(&dir, 8);
    let table = |name: &str| {
        TableDef::new(name, "id")
            .index("by_a", &["a"])
            .index("by_b", &["b"])
    };
    db.define_table(table("one")).expect("first table fits");
    db.define_table(table("two")).expect("second table fits");

    let full = db.define_table(table("three"));
    assert!(
        matches!(full, Err(EngineError::Storage(natdb::Error::DbsFull))),
        "{full:?}"
    );

    // Nothing of the third table exists, and the others keep working.
    let names: Vec<String> = db
        .tables()
        .expect("tables must succeed")
        .into_iter()
        .map(|def| def.name)
        .collect();
    assert_eq!(names, ["one", "two"]);
    Query::insert_into("one", [json!({"id": 1, "a": 1, "b": 2})])
        .execute(&db)
        .expect("an existing table still accepts writes");

    // With room for more, the same definition succeeds.
    drop(db);
    let db = open(&dir, 16);
    db.define_table(table("three"))
        .expect("defines with more databases");
    assert_eq!(db.tables().expect("tables must succeed").len(), 3);
}

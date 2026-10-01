//! Joins and aggregates compared with SQLite on the same rows: the results
//! must be the same multisets (the same lists, where an order is asked).
//!
//! The data has the cases SQL semantics decide: NULL group keys, NULL and
//! missing values inside aggregates, a group whose values are all NULL,
//! posts whose author is NULL or does not exist, and several matches per
//! row.

mod common;

use common::TestDir;
use offline_first_core::engine::{col, Db, Query, TableDef};
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Value};

/// A cell as both engines answer it; reals are rounded so that the two
/// floating point summations compare equal.
#[derive(Debug, Clone, PartialEq, PartialOrd)]
enum Cell {
    Null,
    Int(i64),
    Real(i64),
    Text(String),
}

impl Cell {
    fn real(value: f64) -> Self {
        Self::Real((value * 1e9).round() as i64)
    }

    fn of_json(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Number(n) => match n.as_i64() {
                Some(int) => Self::Int(int),
                None => Self::real(n.as_f64().expect("a finite number")),
            },
            Value::String(text) => Self::Text(text.clone()),
            other => panic!("unexpected cell {other}"),
        }
    }

    fn of_sqlite(value: ValueRef<'_>) -> Self {
        match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(int) => Self::Int(int),
            ValueRef::Real(real) => Self::real(real),
            ValueRef::Text(text) => Self::Text(String::from_utf8_lossy(text).into_owned()),
            ValueRef::Blob(_) => panic!("unexpected blob"),
        }
    }
}

type Rows = Vec<Vec<Cell>>;

/// A small deterministic generator (an LCG), so failures reproduce.
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

struct Fixture {
    _dir: TestDir,
    db: Db,
    sqlite: Connection,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = TestDir::new(label);
        let db = Db::open(dir.db_dir("db")).expect("open the engine");
        let sqlite = Connection::open_in_memory().expect("open SQLite");
        sqlite
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, city TEXT, age INTEGER);
                 CREATE TABLE posts (id INTEGER PRIMARY KEY, author_id INTEGER, views INTEGER);
                 CREATE TABLE likes (id INTEGER PRIMARY KEY, post_id INTEGER);",
            )
            .expect("create the SQLite tables");
        for table in ["users", "posts", "likes"] {
            db.define_table(TableDef::new(table, "id"))
                .expect("define a table");
        }

        let mut rng = Lcg(7);
        let cities = ["Lima", "Bogota", "Quito"];
        let mut users = Vec::new();
        for id in 1..=300i64 {
            let city = match rng.next(10) {
                0 => Value::Null,
                n => json!(cities[(n % 3) as usize]),
            };
            let age = match rng.next(10) {
                0 => Value::Null,
                _ => json!(18 + rng.next(60) as i64),
            };
            users.push(json!({"id": id, "city": city, "age": age}));
        }
        // A group whose ages are all NULL.
        for id in 301..=303i64 {
            users.push(json!({"id": id, "city": "Cusco", "age": null}));
        }
        let posts: Vec<Value> = (1..=600i64)
            .map(|id| {
                let author = match rng.next(20) {
                    0 => Value::Null,
                    _ => json!(1 + rng.next(330) as i64), // some do not exist
                };
                json!({"id": id, "author_id": author, "views": rng.next(1000) as i64})
            })
            .collect();
        let likes: Vec<Value> = (1..=400i64)
            .map(|id| json!({"id": id, "post_id": 1 + rng.next(700) as i64}))
            .collect();

        for (table, rows) in [("users", &users), ("posts", &posts), ("likes", &likes)] {
            Query::insert_into(table, rows.iter().cloned())
                .execute(&db)
                .expect("insert into the engine");
            for row in rows.iter() {
                let object = row.as_object().expect("an object row");
                let columns: Vec<&String> = object.keys().collect();
                let sql = format!(
                    "INSERT INTO {table} ({}) VALUES ({})",
                    columns
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    vec!["?"; columns.len()].join(", ")
                );
                let params: Vec<rusqlite::types::Value> = columns
                    .iter()
                    .map(|column| match &object[column.as_str()] {
                        Value::Null => rusqlite::types::Value::Null,
                        Value::Number(n) => {
                            rusqlite::types::Value::Integer(n.as_i64().expect("integer"))
                        }
                        Value::String(text) => rusqlite::types::Value::Text(text.clone()),
                        other => panic!("unexpected value {other}"),
                    })
                    .collect();
                sqlite
                    .execute(&sql, rusqlite::params_from_iter(params))
                    .expect("insert into SQLite");
            }
        }

        Self {
            _dir: dir,
            db,
            sqlite,
        }
    }

    fn sqlite_rows(&self, sql: &str) -> Rows {
        let mut statement = self.sqlite.prepare(sql).expect("prepare");
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                Ok((0..columns)
                    .map(|i| Cell::of_sqlite(row.get_ref(i).expect("a column")))
                    .collect())
            })
            .expect("query")
            .map(|row| row.expect("a row"))
            .collect()
    }
}

/// The cells of `rows` at `paths` (`"a.b"` reaches into nested objects).
fn engine_rows(rows: &[Value], paths: &[&str]) -> Rows {
    rows.iter()
        .map(|row| {
            paths
                .iter()
                .map(|path| {
                    let value = path
                        .split('.')
                        .try_fold(row, |value, key| value.get(key))
                        .unwrap_or(&Value::Null);
                    Cell::of_json(value)
                })
                .collect()
        })
        .collect()
}

fn sorted(mut rows: Rows) -> Rows {
    rows.sort_by(|a, b| a.partial_cmp(b).expect("comparable cells"));
    rows
}

#[test]
fn group_by_with_every_aggregate_matches_sqlite() {
    let fixture = Fixture::new("sqlite_group");
    let groups: Vec<Value> = Query::group("users")
        .by("city")
        .count("people")
        .count_of("age", "aged")
        .sum("age", "total")
        .avg("age", "mean")
        .min("age", "youngest")
        .max("age", "oldest")
        .having(col("people").ge(2))
        .load(&fixture.db)
        .expect("group");

    assert_eq!(
        sorted(engine_rows(
            &groups,
            &["city", "people", "aged", "total", "mean", "youngest", "oldest"]
        )),
        sorted(fixture.sqlite_rows(
            "SELECT city, COUNT(*), COUNT(age), SUM(age), AVG(age), MIN(age), MAX(age)
             FROM users GROUP BY city HAVING COUNT(*) >= 2"
        )),
    );
}

#[test]
fn group_by_after_a_filter_with_an_order_and_a_limit_matches_sqlite() {
    let fixture = Fixture::new("sqlite_group_filter");
    let groups: Vec<Value> = Query::group("users")
        .filter(col("age").ge(30))
        .by("city")
        .count("people")
        .sum("age", "total")
        .having(col("total").gt(100))
        .order(col("city").asc())
        .limit(3)
        .load(&fixture.db)
        .expect("group");

    assert_eq!(
        engine_rows(&groups, &["city", "people", "total"]),
        fixture.sqlite_rows(
            "SELECT city, COUNT(*), SUM(age) FROM users WHERE age >= 30
             GROUP BY city HAVING SUM(age) > 100 ORDER BY city ASC LIMIT 3"
        ),
    );
}

#[test]
fn inner_join_matches_sqlite() {
    let fixture = Fixture::new("sqlite_inner");
    let rows: Vec<Value> = Query::join("users")
        .alias("u")
        .inner_join_as("posts", "p", "u.id", "author_id")
        .load(&fixture.db)
        .expect("join");

    assert_eq!(
        sorted(engine_rows(&rows, &["u.id", "p.id"])),
        sorted(
            fixture
                .sqlite_rows("SELECT u.id, p.id FROM users u JOIN posts p ON u.id = p.author_id")
        ),
    );
}

#[test]
fn left_join_matches_sqlite() {
    let fixture = Fixture::new("sqlite_left");
    let rows: Vec<Value> = Query::join("users")
        .alias("u")
        .left_join_as("posts", "p", "u.id", "author_id")
        .load(&fixture.db)
        .expect("join");

    assert_eq!(
        sorted(engine_rows(&rows, &["u.id", "p.id"])),
        sorted(
            fixture.sqlite_rows(
                "SELECT u.id, p.id FROM users u LEFT JOIN posts p ON u.id = p.author_id"
            )
        ),
    );
}

#[test]
fn a_filter_on_the_optional_side_of_a_left_join_matches_sqlite() {
    let fixture = Fixture::new("sqlite_left_filter");
    let rows: Vec<Value> = Query::join("users")
        .alias("u")
        .left_join_as("posts", "p", "u.id", "author_id")
        .filter(col("p.views").gt(500))
        .order(col("u.id").asc())
        .then_order_by(col("p.id").asc())
        .load(&fixture.db)
        .expect("join");

    assert_eq!(
        engine_rows(&rows, &["u.id", "p.id", "p.views"]),
        fixture.sqlite_rows(
            "SELECT u.id, p.id, p.views FROM users u LEFT JOIN posts p ON u.id = p.author_id
             WHERE p.views > 500 ORDER BY u.id ASC, p.id ASC"
        ),
    );
}

#[test]
fn an_inner_then_a_left_join_of_three_tables_matches_sqlite() {
    let fixture = Fixture::new("sqlite_three");
    let rows: Vec<Value> = Query::join("users")
        .alias("u")
        .inner_join_as("posts", "p", "u.id", "author_id")
        .left_join_as("likes", "l", "p.id", "post_id")
        .load(&fixture.db)
        .expect("join");

    assert_eq!(
        sorted(engine_rows(&rows, &["u.id", "p.id", "l.id"])),
        sorted(fixture.sqlite_rows(
            "SELECT u.id, p.id, l.id FROM users u JOIN posts p ON u.id = p.author_id
             LEFT JOIN likes l ON p.id = l.post_id"
        )),
    );
}

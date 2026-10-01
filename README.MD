# offline_first_core

A Diesel-style embedded database for Rust and Flutter, built on
**LMDB 1.0.2** through [natdb](https://github.com/JhonaCodes/natdb).

- **Diesel-style queries** — `filter`, `or_filter`, `order`, `then_order_by`,
  `limit`, `offset`, `find`, `first`, `count`, aggregates, `insert_into`,
  `update(..).set(..)`, `update(..).increment(..)`, `delete`, `on_conflict`,
  with Diesel's operator names (`eq`, `ne`, `gt`, `ge`, `lt`, `le`, `eq_any`,
  `ne_all`, `is_null`, `between`, `like`, …).
- **Projection, distinct, `group` and `join`** — `select(fields)` projects
  nested output paths, `distinct()` dedups by key encoding; `Query::group`
  aggregates (`count`, `sum`, `avg`, `min`, `max`) with `having`; `Query::join`
  combines tables by equality (`inner`/`left`), addressed by alias in
  `filter`/`order`.
- **One executor for Rust and Dart** — the Rust builders and the Dart DSL
  ([db_dsl](https://pub.dev/packages/db_dsl), used by
  [flutter_local_db](https://pub.dev/packages/flutter_local_db) in apps and
  [dart_db](https://pub.dev/packages/dart_db) on servers) produce the same
  requests of one documented protocol; filtering, sorting and index lookups
  always run in Rust.
- **Secondary indexes** (single, composite, unique) maintained in the same
  transaction as their rows, and a rule-based planner with `explain`.
- **Transactions** — Diesel-style `transaction` (commit on `Ok`, rollback on
  `Err`), savepoints, read snapshots, atomic batches; a failed write makes its
  transaction rollback-only.
- **Durable by default**, crash-safe (copy-on-write B+tree, no recovery step),
  with a memory map that grows on demand.
- Runs on Android, iOS (device and simulator), macOS, Linux and Windows.

## Rust

```toml
[dependencies]
offline_first_core = "0.7"
serde_json = "1"
```

```rust
use offline_first_core::engine::{col, Db, Query, TableDef};
use serde_json::{json, Value};

let db = Db::open("/path/to/app.lmdb")?;
db.define_table(
    TableDef::new("skills", "id")
        .index("by_language_priority", &["language", "priority"])
        .unique_index("by_slug", &["slug"]),
)?;

Query::insert_into("skills", [json!({"id": "rust-review", "slug": "rr", "language": "rust", "priority": 3})])
    .execute(&db)?;

let top: Vec<Value> = Query::table("skills")
    .filter(col("language").eq("rust"))
    .filter(col("priority").ge(2))
    .order(col("priority").desc())
    .limit(20)
    .load(&db)?;

db.transaction(|tx| {
    Query::update("skills").filter(col("id").eq("rust-review")).set("priority", 5).execute_in(tx)?;
    // A savepoint: its failure only rolls back its own writes.
    let _ = tx.savepoint(|sp| Query::insert_into("skills", [json!({"id": "rust-review"})]).execute_in(sp));
    Ok(())
})?;
```

Rows are JSON objects (`serde_json::Value`, or any `Deserialize` type with
`load::<T>`). A table declares its primary key field and its indexes; `find`
looks rows up by primary key, and `explain` shows which index a query uses and
whether the index alone satisfies the filter (`"exact"`: rows are not checked
again, and `count` reads no row).
Without `order`, the order of the rows is unspecified, as in SQL.

## Flutter / C ABI

The Dart SDK [flutter_local_db](https://pub.dev/packages/flutter_local_db)
bundles this library for every platform. Its C ABI:

| Symbol | Purpose |
|---|---|
| `ofc_open(path, options, *out)` | Opens `<path>.lmdb`; reports errors such as `LegacyFormat` |
| `ofc_execute(handle, request)` | Runs one request of the JSON [wire protocol](src/wire.rs) (statements, batches, interactive transactions, savepoints, `explain`) |
| `ofc_free_string(ptr)` | Releases every string returned by the library |
| `close_database(handle)` | Releases a handle; the database closes with its last handle |
| `create_db`, `push_data`, `get_by_id`, `get_all`, `update_data`, `delete_by_id`, `clear_all_records`, `reset_database` | The 0.5 key-value API, unchanged |

Every entry point catches panics (`panic = "unwind"`), handles opened on the
same path share one environment (Flutter hot restart safe), and interactive
transactions run on an owner thread and roll back after an idle timeout.

## Options

`OpenOptions` (Rust) or the `options` JSON of `ofc_open`:

| Field | Default | Meaning |
|---|---|---|
| `max_dbs` | 1024 | Tables + indexes |
| `initial_map_size` | 64 MiB | Reserved address space, not disk space |
| `max_map_size` | 16 GiB (1 GiB on 32-bit) | The map doubles when full, up to this size |
| `durability` | `full` | `full`: every commit is flushed (`F_FULLFSYNC` on Apple). `no_meta_sync` / `no_sync`: faster, a *system* crash may undo the last transactions |

## Upgrading from 0.5

0.5 stored data with LMDB 0.9, whose file format LMDB 1.0 cannot read.
Opening such a database fails with `LegacyFormat` and leaves the data file
untouched. Migrate through the Dart SDK: `flutter_local_db` 1.6 exports the
records (`LocalDB.exportAll`), and 2.0 imports them. The key-value API and its
JSON contract are otherwise unchanged; `create_db` now uses the path exactly
as given (previously `./` was prepended, which misplaced absolute paths on
Linux and Windows).

## Benchmarks

`cargo run --release --example bench -- 10000` — median of 5 runs, durable
commits, Apple M1 Max, macOS 26.7, 10 000 rows with two indexes (0.6.2):

| Operation | Per op |
|---|---|
| Insert, batch of 10 000 in one transaction | 6.5 µs |
| Insert, one transaction per row | 9.1–10.2 ms (one `F_FULLFSYNC` per commit) |
| Find by primary key | 1.5 µs |
| Indexed query (`city = x AND age > y`, limit 50) | 38 µs |
| Top 20 through an index (`order(age.desc())`) | 15 µs |
| Indexed count (`city = x`, 2 500 matches) | 31 µs |
| Full scan with an unindexed filter (10 000 rows) | 5.4 ms |
| Update one row by key (own transaction) | 9.9–10.4 ms |

Durable single-row commits cost one full flush on Apple hardware; group writes
in a transaction or a batch to amortize it, or open with a relaxed
`Durability`.

## Building the native libraries

`scripts/build-binaries.sh` builds the Apple (iOS xcframework, universal macOS
dylib) and Android (`jniLibs`) libraries; the
[release workflow](.github/workflows/release.yml) builds every platform,
including Linux and Windows, and attaches them with checksums to the GitHub
release of each tag.

## License

MIT. LMDB is distributed under the OpenLDAP Public License (see natdb).

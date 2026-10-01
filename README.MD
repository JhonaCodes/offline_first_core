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
  `Err`), savepoints, read snapshots (`read_transaction`), atomic batches; a
  failed write makes its transaction rollback-only.
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
use offline_first_core::engine::{col, Db, Output, Query, Statement, TableDef};
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

### Projection and `distinct`

`select` keeps only the given field paths in each output row (nested paths
reach into the row, e.g. `"address.city"`; a missing field comes out as
`null`); `distinct` drops a row equal to an earlier one, compared by the same
key encoding the storage layer uses, keeping the first:

```rust
let slugs: Vec<Value> = Query::table("skills").select(["slug"]).distinct().load(&db)?;
```

### `update().increment()`

Adds a numeric delta to the current value of a field in one atomic read-modify-write;
a missing or `null` field counts as `0`, two integers stay an integer, and a
current value that is not a number fails the statement without writing
anything:

```rust
Query::update("skills").filter(col("id").eq("rust-review")).increment("priority", 1).execute(&db)?;
```

### `explain`

Returns the plan the planner would run, without running it: which access path
it chose (`primary_key_lookup`, `primary_key_range`, `index_scan`,
`full_scan`), the index used, whether the scan runs descending, whether the
visited keys already come out in the requested order (`presorted`), and
whether they alone satisfy the whole filter (`exact`, see "How it works"
below):

```rust
let plan = db.explain(&Query::table("skills").filter(col("language").eq("rust")))?;
assert_eq!(plan["access"], "index_scan");
assert_eq!(plan["index"], "by_language_priority");
```

A join explains itself too (`Join::explain`, or `explain` over the C ABI
with a join query): its strategy, each table with its access path, and
where its filter runs:

```rust
let plan = Query::join("users")
    .alias("u")
    .inner_join_as("posts", "p", "u.id", "author_id")
    .explain(&db)?;
// {"strategy": "hash_join",
//  "tables": [{"table": "users", "as": "u", "access": "full_scan"},
//             {"table": "posts", "as": "p", "access": "full_scan"}],
//  "filter": "none"}
```

### `group`

`by` adds a grouping field, the aggregates (`count`, `count_of`, `sum`, `avg`,
`min`, `max`) are each named with an alias, `having` filters groups by alias
or grouping field, and `order`/`limit`/`offset` apply to the resulting groups.
A `sum` of integers stays an integer unless it overflows `i64`; rows with a
`null` grouping field form their own group, as in SQL:

```rust
db.define_table(TableDef::new("scores", "id"))?;
Query::insert_into(
    "scores",
    [
        json!({"id": 1, "city": "Bogota", "age": 30}),
        json!({"id": 2, "city": "Bogota", "age": 40}),
        json!({"id": 3, "city": "Lima", "age": 20}),
    ],
)
.execute(&db)?;

let by_city: Vec<Value> = Query::group("scores")
    .by("city")
    .count("people")
    .avg("age", "avg_age")
    .having(col("people").ge(1))
    .order(col("city").asc())
    .load(&db)?;
```

### `join`

`Query::join(table)` is the `from` side; `inner_join`/`left_join` (or their
`_as` variants, which give the joined table an explicit alias) add a step with
an equality condition (`left` addresses an already-joined alias, `right` is a
plain field of the table being joined). Combined rows are objects keyed by
alias (`{"u": {...}, "p": {...}}`), and `filter`/`order` address a field
through its alias (`"u.id"`):

```rust
db.define_table(TableDef::new("users", "id"))?;
db.define_table(TableDef::new("posts", "id"))?;
Query::insert_into("users", [json!({"id": 1, "name": "ana"})]).execute(&db)?;
Query::insert_into("posts", [json!({"id": 10, "author_id": 1, "title": "p1"})]).execute(&db)?;

let joined: Vec<Value> = Query::join("users")
    .alias("u")
    .inner_join_as("posts", "p", "u.id", "author_id")
    .order(col("u.id").asc())
    .load(&db)?;
```

### `transaction` and `read_transaction`

`Db::transaction` is the Diesel pattern already shown above: the closure gets
a `&mut WriteTx`, `Ok` commits, `Err` rolls back, and `WriteTx::savepoint`
nests a recoverable unit inside it. `Db::read_transaction` instead runs a
closure against one consistent read-only snapshot (`&ReadTx`): writes
committed by other threads after the snapshot started are not visible to it.
A `ReadTx` only reads, so it is driven with `Statement` directly rather than
the `Select`/`Group`/`Join` builders' `load`:

```rust
let count = db.read_transaction(|snapshot| {
    match snapshot.execute(&Statement::Count { table: "skills".into(), filter: None })? {
        Output::Count(count) => Ok(count),
        _ => Ok(0),
    }
})?;
```

## How it works

**Storage.** One LMDB environment per opened directory. Every table and index
is its own named LMDB database: a table named `skills` is stored as `t:skills`,
and its index `by_language_priority` as `i:skills:by_language_priority`. Table
and index definitions live in a `__catalog` database; the key-value API of 0.5
keeps using its own `main` database, in the same environment.

**Keys.** Primary keys and index keys are LMDB keys, which LMDB sorts
byte-wise. Each JSON value is encoded so that byte order follows value order
(`null` < `false` < `true` < numbers < strings < arrays/objects, numbers
ordered as `f64`, strings and composite values escaped so their bytes still
compare correctly); a composite key is the plain concatenation of its values'
encodings. This is what makes an index usable for both equality and range
scans, and for presorted output. Every encoded key is capped at **511 bytes**
on every platform — the limit of LMDB 0.9 (and of `flutter_local_db` 1.x) kept
on purpose, so the same row fits whether it lands on a device with 4&nbsp;KB
pages or on Apple Silicon's 16&nbsp;KB pages.

**The planner.** `explain` reports what `plan.rs` chooses: an `eq` on the
primary key is one lookup; otherwise the index (or the primary key) with the
most leading `eq` fields plus a range on the next field wins, falling back to
an index that already matches `order` for a top-k scan, and to a full table
scan otherwise. A plan is **exact** when the keys it visits already satisfy
the whole filter: rows are not re-checked, and a `count` does not read a
single row. A non-exact plan still returns the right rows — every visited row
is checked against the full filter — it just reads more of them than
necessary.

**Joins.** A join reads each of its tables in full; every step builds a
hash of the joined table by its `on.right` field and probes it with the
rows combined so far, so it costs one pass over each table and memory for
the joined ones. Its filter, order and limit run on the combined rows: no
index narrows a join yet. `explain` of a join says so (`"strategy":
"hash_join"`, `"access": "full_scan"`, `"filter": "after_join"`).

**Transactions.** LMDB allows one write transaction at a time per
environment; readers never block writers nor each other. `Db::transaction`
holds that single writer slot for its duration; a write that fails inside it
marks the transaction rollback-only — catching the error does not make it
committable again, which is why recoverable work goes in a `savepoint`
(a nested LMDB transaction that only fails its own writes).
`Db::read_transaction` opens a read-only snapshot instead, and fails
immediately if the calling thread already holds a write transaction on the
same database (reentrancy). Over the C ABI, an interactive transaction
(`begin`/`tx_execute`/`commit`) runs on its own **owner thread**, because an
LMDB transaction belongs to the thread that began it while Dart calls can
arrive from any thread; a transaction left idle past its timeout (30 seconds
by default) is rolled back, so an abandoned one — a killed isolate, a dropped
connection — cannot hold the writer lock forever.

**Growth.** The memory map starts at `initial_map_size` (64 MiB by default)
and is reserved address space, not disk space. When a write hits the current
map size, the transaction fails, the map doubles (up to `max_map_size`) under
an exclusive lock that waits for every other transaction of the process to
finish, and the write is retried.

**Panics.** Every C ABI entry point runs its body through
`std::panic::catch_unwind`: an internal panic is logged and turned into an
error response instead of unwinding across the `extern "C"` boundary, which
(since the release profile keeps `panic = "unwind"`) would otherwise abort the
host process.

**Handles.** Opening the same directory twice in one process is only safe
through this crate: a `Db` or a C ABI handle opened on a path that is already
open in the process shares that one LMDB environment (and sees the same
data) instead of trying to open it again, which LMDB forbids. The database
closes when its last handle is released.

```
 Rust API (Db, Query)          C ABI (ofc_open / ofc_execute)
          \                          /
           \                        /
            v                      v
                  SharedDb (per path, registry.rs)
                          |
                    AppDbState
                          |
                       Store  ──────────────  LMDB environment
                     /   |   \                 t:<table>
               legacy  catalog tables          i:<table>:<index>
                                                __catalog, main
```

## C ABI / Dart

The Dart SDKs — [flutter_local_db](https://pub.dev/packages/flutter_local_db)
for apps and [dart_db](https://pub.dev/packages/dart_db) for servers, both
built on [db_dsl](https://pub.dev/packages/db_dsl) — call this library over
`dart:ffi` through the symbols below. Each bundles the prebuilt library of
the release it pins through a build hook: `flutter_local_db` for Android,
iOS, macOS, Linux and Windows, `dart_db` for Linux, macOS and Windows.

| Symbol | Purpose |
|---|---|
| `ofc_open(path, options, *out)` | Opens `<path>.lmdb`; reports errors such as `LegacyFormat` |
| `ofc_execute(handle, request)` | Runs one request of the JSON wire protocol (statements, batches, interactive transactions, savepoints, `explain`) |
| `ofc_free_string(ptr)` | Releases every string returned by the library |
| `close_database(handle)` | Releases a handle; the database closes with its last handle |
| `create_db`, `push_data`, `get_by_id`, `get_all`, `update_data`, `delete_by_id`, `clear_all_records`, `reset_database` | The 0.5 key-value API and its JSON contract. Since 0.7.2, `get_all` answers a `DatabaseError` naming a stored record it cannot decode instead of leaving it out; delete that id to recover |

Every entry point catches panics (`panic = "unwind"`), handles opened on the
same path share one environment (Flutter hot restart safe), and interactive
transactions run on an owner thread and roll back after an idle timeout.

The wire protocol spoken by `ofc_execute` is specified in db_dsl's
[PROTOCOL.md](https://github.com/JhonaCodes/db_dsl/blob/main/PROTOCOL.md);
`src/wire.rs` is this crate's implementation of it.

## How it should be used

- **Group writes.** A durable commit flushes (`F_FULLFSYNC` on Apple
  hardware); inserting rows one transaction at a time pays that flush per row.
  Use `Db::transaction` or `Db::batch` to commit many writes together, or
  relax `durability` (see Options below) when the flush itself is the
  bottleneck.
- **Index what you filter or order by, and check `explain`.** A query without
  a matching index runs a full scan; `explain` says which access path and
  index were chosen and whether the plan is `exact`, so a slow query shows
  the cause directly instead of being guessed at.
- **Do not rely on row order without `order`.** Like SQL, an unordered query
  may return rows in any order the chosen access path happens to produce.
- **Keep encoded keys at or under 511 bytes.** A field that is part of a
  primary key or an index and can hold long values (free text, for example)
  should not be indexed, or should be truncated before indexing.
- **Only one write transaction runs at a time** per database; a thread that
  starts one while already holding one on the same database fails immediately
  (reentrancy) instead of deadlocking.
- **Keep interactive transactions over the C ABI short.** They occupy an
  owner thread and, for writes, the single writer slot, until committed,
  rolled back, or idle past their timeout; a long-lived interactive
  transaction blocks every other write meanwhile.
- **Choose durability deliberately.** `full` (the default) survives a power
  loss; `no_meta_sync`/`no_sync` trade that guarantee for throughput and
  should be a conscious choice, not an accident.
- **Open each path once per process** and share the handle (or rely on this
  crate's automatic sharing, see "Handles" above) — do not design around
  reopening a path to get a fresh connection.

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
records (`LocalDB.exportAll`), and either 2.0 or 3.0 imports them. The
key-value API and its JSON contract are otherwise unchanged; `create_db` now
uses the path exactly as given (previously `./` was prepended, which
misplaced absolute paths on Linux and Windows).

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

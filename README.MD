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

### Offline-first sync

A table declared with `sync_with(remote)` records every effective write as
an immutable change **in the same transaction** as the row, so a crash, a
rolled back savepoint or a full disk can never keep the row and lose its
pending change, or the other way around (RFC-001 §13). The network stays in
your code; the engine only knows what is pending, what is being sent and
what the server confirmed:

```rust
use offline_first_core::engine::sync::{
    Acknowledgement, ClaimLimits, PushResult, RemotePage, Resolution,
};

db.define_table(TableDef::new("notes", "id").sync_with("primary"))?;
Query::insert_into("notes", [json!({"id": "a", "title": "draft"})]).execute(&db)?;

// Push: lease a batch in a short transaction, send it, record the answer.
let batch = db.sync().claim("primary", &ClaimLimits::default())?;
let versions = my_server.push(&batch.envelopes);          // your HTTP code
db.sync().apply_push_result("primary", &PushResult {
    lease_id: batch.lease_id,
    acknowledged: batch.envelopes.iter().zip(versions)
        .map(|(envelope, version)| Acknowledgement::of(envelope, version))
        .collect(),
    rejected: Vec::new(),
})?;

// Pull: a page and its checkpoint are stored together, or not at all.
let status = db.sync().status("primary")?;
let page = my_server.pull(&status.checkpoint);            // your HTTP code
db.sync().apply_remote("primary", &RemotePage {
    expected_checkpoint: status.checkpoint,
    next_checkpoint: page.cursor,
    changes: page.changes,
})?;

// A server change over a pending local change is kept as a conflict.
for conflict in db.sync().conflicts("primary")? {
    db.sync().resolve_conflict(&conflict.id, conflict.local_row_version, &Resolution::AcceptRemote)?;
}
```

What it guarantees, each with its test in `tests/sync.rs`, `tests/sync_model.rs`
and `tests/processes.rs`:

- An acknowledgement settles exactly one mutation and revision: the ACK of
  revision 7 never confirms revision 8 written while 7 was in flight.
- One change per row is in flight, in revision order; an envelope keeps its
  bytes, its `mutation_id` and its `base_version` (the server version it
  builds on, fixed at the first claim) on every retry, so the server can
  deduplicate.
- A late release of an old lease, a duplicate ACK and a rejection for a
  lease that moved on change nothing; an unknown or mismatched ACK is an
  error that changes nothing.
- A deletion leaves a tombstone until the server acknowledged it, and the
  key cannot be created again meanwhile (`TombstonePending`); afterwards it
  comes back as a new `generation`.
- Remote changes never come back as local changes. Over a pending local
  change they become a conflict, with both variants kept;
  `resolve_conflict` takes the row version as a precondition, refuses while a
  change is being sent, and accepting the remote variant settles the local
  changes as resolved, never as acknowledged.
- Rows written before a table was synchronized are `unknown`, never
  `synced`; a synchronized table stays synchronized.

A reference model runs 150 random sequences of 80 steps (writes, deletes,
claims, lost and partial answers, duplicate ACKs, another device's writes,
pulls, resolutions, restarts) against the engine and a simulated server,
and checks that everything converges to the server's rows. The wire
operations are `sync_claim`, `sync_push_result`, `sync_release`,
`sync_retry`, `sync_apply_remote`, `sync_resolve`, `sync_state`,
`sync_pending`, `sync_conflicts` and `sync_status` (see `src/wire.rs`).

## Diesel conformance

[docs/diesel-conformance.md](https://github.com/JhonaCodes/offline_first_core/blob/main/docs/diesel-conformance.md)
maps every family of the Diesel 2.3 API to this engine and to db_dsl:
implemented (with the test that proves it), partial, planned or not
applicable.

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
necessary. An `eq_any` on the primary key, or on the leading field of an index,
looks up only the keys it names (one point or one index range per value)
when nothing better applies, instead of a full scan; those rows are checked
against the whole filter.

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

**Sync.** The records of synchronized tables live in one more internal
database, `__sync`, created with the first synchronized table: per-row
metadata (revisions, the last known server version, the open changes), the
open changes themselves, an index of open changes per remote in commit
order, receipts of settled mutations (so a duplicate ACK is recognised),
conflicts, checkpoints and counters. The insert, update and delete paths of
the executor record a change right after writing the row, inside the same
nested LMDB transaction, so the change commits or rolls back with the row.
The changes of one root transaction carry its LMDB transaction id (nested
transactions inherit it) as their `local_transaction_id`. `claim`, the push
results, `apply_remote` and `resolve_conflict` are write transactions of
their own, short, and never span the network.

**Growth.** The memory map starts at `initial_map_size` (64 MiB by default)
and is reserved address space, not disk space. Before an operation begins,
the map doubles (up to `max_map_size`) when the pages in use passed half of
it, under an exclusive lock that waits for every other transaction of the
process to finish; so the next transactions find room. A write that still
hits the end of the map fails with `MapFull` and the map doubles before the
next operation: a single statement or a batch is then run again on its own,
while a transaction (whose closure cannot be replayed) answers `MapFull`
for the caller to run again. That only happens when one transaction alone
writes more than half of the map, or several large ones race on a small
map.

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

### ABI v2 (`include/localdb.h`)

Since 0.7.6 the same protocol is also served through handles that cannot
dangle (RFC-001 §12.2). The Dart SDKs still use the v1 symbols above; both
ABIs work side by side and share the databases of a path.

| Symbol | Purpose |
|---|---|
| `ldb_abi_version()` | `2` |
| `ldb_open(path, len, options, len, *out, *response)` | Opens `<path>.lmdb`; writes a `u64` handle and a response buffer |
| `ldb_execute(db, request, len, *response)` | Runs one request (bytes with a length, so any byte may appear in it) |
| `ldb_buffer_view(buffer, *data, *len)` | Where a response is, until released |
| `ldb_buffer_release(buffer)` | Releases a response |
| `ldb_close(db)` | Releases a handle; the database closes with its last handle |

Each call answers a status code: `LDB_OK`, or `LDB_INVALID_HANDLE` for a
handle that is closed, released, of the other kind (database or buffer) or
never issued (so a second close or release is harmless),
`LDB_NULL_POINTER`, `LDB_INVALID_UTF8`, `LDB_PANIC` (contained) or
`LDB_REQUEST_TOO_LARGE` (over 256 MiB). Handles are ids validated against a
registry and never reused; the details of a failed operation travel in its
response, as wire JSON. `tests/abi_v2.rs` covers double close and release,
use after close, foreign and made-up handles, hot restart and concurrent
requests.

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
- **Treat `MapFull` from a transaction as retryable.** Run the unit of work
  again (the map grew meanwhile); size `initial_map_size` above what one
  transaction writes to make it rare.
- **Choose durability deliberately.** `full` (the default) survives a power
  loss; `no_meta_sync`/`no_sync` trade that guarantee for throughput and
  should be a conscious choice, not an accident.
- **Sync: settle by mutation, never by row.** Build acknowledgements from
  the envelopes the server confirmed (`Acknowledgement::of`), release a
  lease when a push fails, and let conflicts reach `resolve_conflict`; do
  not delete rows of a synchronized table to make the debt go away (a
  deletion is a change too). Servers must deduplicate by `mutation_id`:
  retries resend the same envelope.
- **Open each path once per process** and share the handle (or rely on this
  crate's automatic sharing, see "Handles" above) — do not design around
  reopening a path to get a fresh connection.

## Compatibility

Within 0.7, the C ABI and the wire protocol (version 1) only grow: new
operations and new optional fields, never a changed meaning. In the Rust
API, `EngineError`, `SyncError` and `TableDef` are `#[non_exhaustive]` since
0.7.5, so new error variants and table options can arrive in a patch
release: match errors with a wildcard arm (or use `code()`), and build
tables with `TableDef::new` and its builders.

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

[BENCHMARKS.md](BENCHMARKS.md) has the whole matrix at 10 000, 100 000 and
1 000 000 rows, reproduced with one command, `tool/bench.sh` (it records
the hardware and the versions). Every write is durable. On an Apple M1 Max
(0.7.6), at 1 000 000 rows:

| Operation | Per op |
|---|---|
| Insert, batch in one transaction | 6.0 µs per row (9.4 µs with the sync outbox) |
| Insert or update, one transaction per row | 9.6–9.9 ms (one `F_FULLFSYNC` per commit) |
| Find by primary key | 2.2 µs (3.1 µs as JSON through `ofc_execute`) |
| Composite range (`city = x AND age > y`, limit 50) | 46 µs |
| Top 20 through an index | 18 µs |
| Posts of 50 users (`eq_any` on an index) | 68 µs |
| Sync: claim and acknowledge 100 changes | 24 ms (two durable commits) |
| Hash join over two full tables (1 000 000 ⋈ 250 000) | 1.45 s |

Durable single-row commits cost one full flush on Apple hardware; group
writes in a transaction or a batch to amortize it, or open with a relaxed
`Durability`. A join reads both tables in full until the join planner uses
indexes (on the roadmap).

## Building the native libraries

`scripts/build-binaries.sh` builds the Apple (iOS xcframework, universal macOS
dylib) and Android (`jniLibs`) libraries; the
[release workflow](.github/workflows/release.yml) builds every platform,
including Linux and Windows, and attaches them with checksums to the GitHub
release of each tag.

## License

MIT. LMDB is distributed under the OpenLDAP Public License (see natdb).

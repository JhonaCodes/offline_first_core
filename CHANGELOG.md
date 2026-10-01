# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.6] - 2026-10-01

### Fixed
- Sync: claiming changes, releasing a lease and listing pending changes
  read only what they answer, instead of every open change of the remote.
  Claiming and acknowledging 100 changes took 780 ms with 100 000 pending
  and takes 24 ms at any backlog (an index of the changes of each lease,
  and the open changes read a step at a time; RFC §13.17). Leases taken
  before 0.7.6 are released when they expire.
- The memory map grows ahead: before an operation begins, it doubles when
  the pages in use passed half of it. Before, it grew only after a write hit
  its end, so every growth failed the transaction in flight with `MapFull`
  (a transaction cannot be replayed); one after another, transactions now
  never meet a full map. The README said a failed write was always retried:
  that holds for statements and batches, not for transactions, which
  answer `MapFull` for the caller to run again.

### Added
- The C ABI v2 (`include/localdb.h`): `ldb_open`, `ldb_execute`,
  `ldb_buffer_view`, `ldb_buffer_release`, `ldb_close` and
  `ldb_abi_version`, with `u64` handles validated against a registry
  (never reused, the kind is part of the id) and response buffers of a
  pointer and a length. A second close or release, a use after close, a
  foreign or made-up handle answer `LDB_INVALID_HANDLE` instead of
  undefined behaviour; panics are contained (`LDB_PANIC`); requests over
  256 MiB answer `LDB_REQUEST_TOO_LARGE`. The v1 symbols are unchanged and
  share the databases of a path with v2 handles.
- Tests: malformed wire requests (truncated JSON, removed fields, extreme
  numbers, unknown operations, nesting past the parser's limit) answer an
  error, never a crash or a panic (`tests/fuzz_wire.rs`); the map grows
  under concurrent writers to a synchronized table and readers without
  losing a row or a pending change (`tests/stress.rs`); wire responses
  freed with `ofc_free_string` keep the heap stable (`tests/memory.rs`).
- `tool/bench.sh` runs the benchmarks at 10 000, 100 000 and 1 000 000 rows
  into `BENCHMARKS.md`, with the hardware and versions: composite ranges,
  top-k, joins, `eq_any`, durable batches with and without the sync outbox,
  sync claim and acknowledgement, and the cost of the JSON wire next to the
  Rust API.

## [0.7.5] - 2026-10-01

### Added
- Offline-first sync inside the transaction (RFC-001 §13):
  `TableDef::sync_with(remote)` records every effective write of the table
  as an immutable change in the same LMDB transaction as the row, and
  `Db::sync()` exposes `claim`, `apply_push_result`, `release`, `retry`,
  `apply_remote`, `resolve_conflict`, `state_of`, `pending`, `conflicts`
  and `status`. Acknowledgements settle one mutation and revision; one
  change per row is in flight, in revision order, with its bytes, identity
  and base version fixed; deletions keep a tombstone until acknowledged;
  remote changes never echo back and become conflicts over pending local
  changes; a page of remote changes and its checkpoint commit together.
- Wire operations `sync_claim`, `sync_push_result`, `sync_release`,
  `sync_retry`, `sync_apply_remote`, `sync_resolve`, `sync_state`,
  `sync_pending`, `sync_conflicts` and `sync_status`, and the `sync` field
  of a table definition (absent for local tables, so existing requests and
  catalogs are unchanged).
- `SyncError` (inside `EngineError::Sync`) with the stable codes
  `SyncNotTracked`, `UnknownMutation`, `AcknowledgementMismatch`,
  `StaleCheckpoint`, `ConflictNotFound`, `RowVersionMismatch`,
  `MutationInFlight` and `TombstonePending`.
- Tests: the invariants I1–I12 (`tests/sync.rs`), a reference model over 150
  random sequences of 80 steps against a simulated server
  (`tests/sync_model.rs`), and a killed writer and a full file system on a
  synchronized table (`tests/processes.rs`).

### Changed
- `EngineError` and `TableDef` are `#[non_exhaustive]`: match errors with a
  wildcard arm and build tables with `TableDef::new` and its builders (see
  "Compatibility" in the README). The C ABI and the wire protocol are
  unchanged for existing requests.
- Dropping a synchronized table discards its sync records with it.

## [0.7.4] - 2026-10-01

### Changed
- `eq_any` on the primary key looks up only the keys it names, and on the
  leading field of an index scans one index range per value, when no `eq`
  or range plan applies; it was a full scan. Repeated values are visited
  once, `null` and composite values keep the full scan, and every row is
  checked against the whole filter, so results do not change.
  `explain` answers `primary_key_lookup` or `index_scan` for them. This is
  what makes db_dsl's `belongingTo` read only the children it asks for.

## [0.7.3] - 2026-10-01

### Added
- `explain` of a join: `Join::explain` / `Db::explain_join`, and the wire
  `explain` with a join query (it has `from`, a select has `table`). It
  answers the strategy (`hash_join`), every table with its alias and access
  path (`full_scan`), and where the filter runs (`after_join` or `none`).
- Tests against SQLite on the same rows: group by with every aggregate,
  having, filters, order and limit; inner, left and three-table joins, and
  a filter on the optional side of a left join.
- Tests with real processes: a writer killed mid-write (three times) leaves
  only whole transactions and the next writer resumes; two processes
  writing at once both commit and see each other's rows; on Unix, a full
  file system is a Storage error that keeps every commit.
- Tests of keys with NUL, combining characters and emoji, of an index on
  text with NUL, and of running out of LMDB databases (`max_dbs`).

## [0.7.2] - 2026-10-01

### Fixed
- `get_all` (and `AppDbState::get`) no longer skips a stored record it cannot
  decode: the listing fails with the record's id (`DbError::Utf8` or
  `DbError::Deserialization`; over the C ABI a `DatabaseError` naming it),
  instead of answering an incomplete list as if it were complete. Deleting
  that id makes the listing complete again.

### Added
- Tests that seed undecodable records into a real database (JSON and UTF-8,
  through the Rust API and the C ABI, and the recovery by deleting the id).
- With the test-only `fault-injection` feature,
  `ofc_fault_injection_arm_in_registry_lock` and
  `ofc_fault_injection_arm_in_database_lock` make the next acquisition of
  that lock panic while held; tests show that a poisoned lock is recovered
  and the database keeps working.

### Changed
- Tests that only checked for the absence of a panic now assert results:
  JSON edge cases round-trip, invalid database names are passed to `init`
  for real, and the bulk and repeated-cycle tests check the exact records
  left.

## [0.7.1] - 2026-10-01

### Changed
- Rewrote the documentation (`README.MD` and the crate-level rustdoc of
  `src/lib.rs`) against the current code: what the engine does, how storage,
  key encoding, the planner, transactions and the C ABI panic boundary work,
  and how the database should be used (grouping writes, indexing what is
  filtered or ordered, key size, durability, interactive transaction
  lifetime). No behavior change.

## [0.7.0] - 2026-10-01

### Added
- `select`: `fields` (projection into nested output paths, `null` when
  missing) and `distinct` (dedup by key encoding, keeping the first row).
- `group` (new statement): `by`, aggregates (`count`, `count_of`, `sum`,
  `avg`, `min`, `max`, each named by `as`), `having`, `order`, `limit`,
  `offset`. Semantics match the `aggregate` statement (sum of integers stays
  an integer unless it overflows `i64`, then `f64`; `null` groups form their
  own group, as in SQL).
- `join` (new statement): `from`/`joins` with aliases, `inner`/`left` kinds,
  an equality condition (`on.left`/`on.right`), and combined rows addressed
  by alias (`"u.name"`) in `filter` and `order`.
- `update`: `increment`, adding a numeric delta to the current value of a
  field (missing or `null` counts as `0`; two integers stay an integer;
  otherwise a finite `f64`). A current value that is not a number, an `i64`
  overflow or a sum that is not finite fails with `InvalidRequest` and
  writes nothing.
- Rust DSL: `Select::select`/`distinct`, `Query::group`/`Group`,
  `Query::join`/`Join`, `Update::increment`.

### Changed
- Keys are limited to 511 bytes on every platform (`PROTOCOL_MAX_KEY_SIZE`) —
  previously LMDB 1.0's page-size-dependent limit (about 2 KB with 4 KB
  pages, 8 KB with the 16 KB pages of Apple Silicon), which made the same row
  fit on some devices and not others.

## [0.6.2] - 2026-10-01

### Changed
- Range scans visit only the keys inside their bounds: `gt` and `lt` skip the
  keys of the bound itself, and a one-sided range stops at the end of the kind
  of value it compares with (a numeric range no longer walks the strings).
- A plan whose keys satisfy the whole filter is *exact*: its rows are not
  checked again, and `count` reads no row at all. `explain` reports it as
  `"exact"`. With 10 000 rows, an indexed `count` went from 2.9 ms to 32 µs and
  the indexed query of the benchmark from 221 µs to 39 µs.

### Fixed
- `-0.0` equals `0` in filters, as it already did in index keys.

## [0.6.1] - 2026-10-01

### Fixed
- Opening a database failed with `Operation not permitted` inside the iOS and
  macOS App Sandbox: natdb 0.1.1 locks with process-shared pthread mutexes
  instead of named POSIX semaphores, which the sandbox rejects.

## [0.6.0] - 2026-10-01

A Diesel-style query engine over LMDB 1.0.2, and the fixes needed to ship it.

### Breaking
- Storage moved to **LMDB 1.0.2** through [natdb](https://crates.io/crates/natdb).
  LMDB 1.0 cannot read files written by LMDB 0.9 (0.5.x): opening one fails
  with `LegacyFormat` (`ofc_open`, `AppDbState::init`) or returns null
  (`create_db`), and the data file is left untouched. Migrate with
  flutter_local_db 1.6 (`exportAll`) and 2.0 (import).
- All nine legacy entry points are `unsafe extern "C" fn`: they dereference raw
  pointers. C and Dart callers are unaffected (`unsafe` is not part of the
  symbol); Rust callers now need an `unsafe` block.
- `create_db` uses the path exactly as given (`./` was prepended, which placed
  absolute paths under the working directory on Linux desktop and Windows).

### Added
- `engine`: tables of JSON rows with a primary key and secondary indexes
  (single, composite, unique, maintained in the same transaction), a
  rule-based planner (`explain`), and Diesel-style Rust builders (`Query`,
  `col`, `filter`, `order`, `limit`, `insert_into`, `update().set()`,
  `delete`, `on_conflict`, aggregates).
- Transactions: `Db::transaction` (commit on `Ok`, rollback on `Err`),
  savepoints, read snapshots, atomic batches; a failed write makes its
  transaction rollback-only; statement writes are atomic.
- C ABI `ofc_open` (reports open errors) and `ofc_execute`, a versioned JSON
  wire protocol (statements, batches, interactive transactions on an owner
  thread with an idle timeout, savepoints, `explain`, `info`).
- The memory map starts at 64 MiB and doubles when full, up to `max_map_size`.
- `OpenOptions::durability`: `full` (default), `no_meta_sync`, `no_sync`.
- `examples/bench.rs`, `scripts/build-binaries.sh` and a release workflow that
  attaches the libraries of every platform (including Windows x64/arm64 and
  Linux x64/arm64) to each GitHub release.

### Changed
- Release profile: `opt-level = 3` (reads 35–70% faster than `z` in
  `examples/bench.rs`, for a 20% larger library).
- Opening one directory twice in a process with the Rust API fails with
  `AlreadyOpen` (LMDB forbids it); FFI handles share one environment.

### Removed
- The committed `jniLibs/` binaries of 0.5.0, `ndk_guide.md` and the workflow
  that pushed binaries into the Flutter repository.

### Also in this release (C ABI hardening)
- `ofc_free_string(ptr)`: releases the strings returned by the library. Every
  response string must be released with it, exactly once (not with the C
  `free`). Before, no function could release them, so every call leaked its
  response (`get_all` leaked a copy of the whole database).
- New exported symbols use the `ofc_` prefix: on iOS the library is linked
  statically into the app, where exported names share the process namespace.

#### Changed
- Release builds use `panic = "unwind"`, and every entry point contains
  panics: an internal panic is logged and answered with
  `{"DatabaseError": "internal panic in <function>: <message>"}` (`create_db`
  returns null) instead of aborting the app. The release library grows by
  16,800 bytes (aarch64-apple-darwin dylib, 470,912 → 487,712 bytes).
- `create_db` on a path that is already open in the process returns a new
  handle to the same database, as in 0.5.0 (with heed it returned null). This
  covers several Dart isolates and a Flutter hot restart that loses its
  pointer without closing it. The database closes with its last handle.
- `reset_database` resets the database shared by every handle opened on the
  same path; all of them keep working on the new database. If the target path
  is open by another database of the process, it fails with a `DatabaseError`
  ("already open") and changes nothing.
- The `BadRequest` messages for null pointers passed to `push_data` now name
  the function, like the other entry points (`Null state pointer passed to
  push_data`).

#### Fixed
- `close_database` did not release anything: the handle and the LMDB
  environment stayed alive, so the same path could not be reopened in the
  process. It now releases the handle; **the pointer is invalid after the
  call**, and closing it twice is undefined behavior, like a double `free`.
- **Breaking on Linux desktop**: `create_db` turned the name into `./<name>`,
  so an absolute path resolved against the working directory
  (`$CWD/<absolute path>.lmdb`). The path is now used exactly as given, as
  `reset_database` already did. iOS, Android and macOS apps run with `/` as
  working directory, so their location does not change. On Linux desktop the
  effective location changes: databases created by earlier versions stay under
  `$CWD/<absolute path>.lmdb` and must be moved to `<absolute path>.lmdb` to be
  found. On Windows (`./C:\...`) absolute paths did not work before.
- `delete_by_id`, `reset_database` and `close_database` created a mutable
  reference to state that other calls (other isolates) could be using at the
  same time, which is undefined behavior. Entry points now only share the
  handle, and a reset excludes other operations through a lock.

#### Testing
- Cargo feature `fault-injection` (test only, never enable it in a shipped
  build) exports `ofc_fault_injection_arm` to test the panic containment:
  `cargo test --features fault-injection --test panic_boundary`.

### v0.5.0 - 2025-01-14
- Update documentation

### v0.4.0 - 2025-01-14
- Improve test cases

## [0.3.0] - 2025-01-13

### 🔄 **BREAKING CHANGES**
- **Migrated from redb to LMDB** as the underlying storage engine
- Database files now use `.lmdb` directory format instead of single files

### ✨ **Added**
- **New FFI function**: `close_database()` for explicit connection management
- Improved hot restart support for Flutter applications
- Better memory management and resource cleanup
- Enhanced error handling for LMDB-specific cases

### 🛡️ **Security & Stability**
- **Eliminated all `unwrap()` calls** in production code for safer error handling
- **Robust error propagation** with comprehensive LMDB error mapping
- **Null pointer safety** improvements in FFI layer
- **ACID compliance** maintained with LMDB transactions

### 🐛 **Fixed**
- **Hot restart issues** in Flutter FFI integration (primary motivation for migration)
- **Null pointer exceptions** during database reconnections
- **Memory leaks** in long-running applications
- **Connection stability** in development environments

### 🎯 **Why LMDB?**

The migration from redb to LMDB was primarily driven by **Flutter FFI stability issues**:

- **Hot Restart Problems**: redb was causing null pointer exceptions during Flutter hot restart cycles
- **Connection Management**: LMDB provides more robust connection handling for FFI scenarios
- **Production Stability**: LMDB is battle-tested in production environments (used by OpenLDAP, Bitcoin Core)
- **Better FFI Support**: LMDB's C-compatible design works better with Flutter's FFI bridge
- **Memory Efficiency**: Superior memory mapping and resource management

### 🔧 **Technical Improvements**
- Database initialization now uses directory-based storage
- Improved cursor iteration for batch operations
- Better transaction lifecycle management
- Enhanced error messages with specific LMDB error codes
- Comprehensive test coverage (20/20 tests passing)

### 📊 **Performance**
- Maintained zero-copy reads
- Optimized transaction batching
- Efficient memory mapping (1GB default map size)
- Improved concurrent access patterns

### 🔄 **Migration Guide**

**For existing Flutter projects:**
- No FFI interface changes required
- Database files will be automatically converted on first run
- Existing data remains compatible through JSON serialization
- Consider calling `close_database()` before hot restart for optimal performance

**For direct Rust usage:**
- Replace `redb` imports with `lmdb` equivalents
- Database paths now create directories instead of files
- Error types have changed to LMDB-specific variants

### 📋 **API Compatibility**
All existing FFI functions remain unchanged:
- ✅ `create_db()`
- ✅ `push_data()`
- ✅ `get_by_id()`
- ✅ `get_all()`
- ✅ `update_data()`
- ✅ `delete_by_id()`
- ✅ `clear_all_records()`
- ✅ `reset_database()`
- 🆕 `close_database()` - NEW

## [0.2.0] - 2024-XX-XX

### Added
- Enhanced redb-based implementation
- Improved CRUD operations
- Better FFI interface for cross-language integration
- Enhanced JSON serialization support

### Fixed
- Memory safety improvements
- Error handling enhancements
- Performance optimizations

## [0.1.1] - 2024-XX-XX

### Added
- Initial redb-based implementation
- Basic CRUD operations
- FFI interface for cross-language integration
- JSON serialization support

### Fixed
- Memory safety improvements
- Error handling enhancements

## [0.1.0] - 2024-XX-XX

### Added
- Initial release
- Core database functionality
- FFI bindings
- Basic documentation
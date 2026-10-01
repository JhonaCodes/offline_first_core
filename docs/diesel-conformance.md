# Diesel 2.3 conformance

How the API of [Diesel 2.3](https://docs.diesel.rs/2.3.x/diesel/) maps to
offline_first_core (Rust) and to [db_dsl](https://pub.dev/packages/db_dsl)
(Dart, on this engine and on `MemoryEngine`), family by family. Every row
marked **implemented** names a test that exists and passes; nothing is
listed as implemented that answers `Ok` without running.

Status:

- **implemented**: same meaning as Diesel; the test proves it.
- **partial**: implemented with a stated limit.
- **planned**: not implemented yet; it is in the roadmap.
- **not-applicable**: Diesel needs it because of SQL or code generation;
  this engine stores JSON rows declared at run time.

Rust tests are under `tests/` of this crate; Dart tests are db_dsl's
conformance cases (`lib/src/conformance/cases/`), which run on
`MemoryEngine` and on the native engine of flutter_local_db and dart_db.

## Queries (`QueryDsl`)

| Diesel | Here | Status | Test |
|---|---|---|---|
| `filter` | `Query::table(t).filter(e)` / `t.filter(e)` | implemented | `engine.rs::test_insert_find_and_select_like_diesel` |
| `or_filter` | `or_filter` | implemented | `engine.rs::test_or_filter_and_offset_like_diesel` |
| `order`, `then_order_by` | `order` (replaces), `then_order_by` (adds) | implemented | `sqlite_reference.rs::a_filter_on_the_optional_side_of_a_left_join_matches_sqlite` |
| `limit`, `offset` | `limit`, `offset` | implemented | `engine.rs::test_or_filter_and_offset_like_diesel` |
| `select` | `select(fields)` (Dart `pluck`, `project`) | implemented | `relational.rs::test_projection_places_nested_values_and_defaults_missing_to_null` |
| `distinct` | `distinct` | implemented | `relational.rs::test_distinct_without_fields_compares_whole_rows` |
| `group_by`, `having` | `Query::group(t).by(f)…having(e)` | implemented | `sqlite_reference.rs::group_by_with_every_aggregate_matches_sqlite` |
| `inner_join` | `Query::join(t).inner_join(…)` | partial: equality on one field per step | `sqlite_reference.rs::inner_join_matches_sqlite` |
| `left_join` / `left_outer_join` | `left_join` | partial: equality on one field per step | `sqlite_reference.rs::left_join_matches_sqlite` |
| `find` | `Query::find(t, key)` | implemented | `engine.rs::test_insert_find_and_select_like_diesel` |
| `load`, `first`, `get_result(s)` | `load`, `first`, `get_results` | implemented | `engine.rs::test_insert_find_and_select_like_diesel` (`load`), `engine.rs::test_batch_is_atomic` (`first`), `engine.rs::test_auto_increment_and_aggregates` (`get_results`) |
| `count` | `count` | implemented | `planner_eq_any.rs::eq_any_on_the_primary_key_looks_the_rows_up` |
| `exists` (subquery) | — | planned | — |
| unions, subqueries | — | planned | — |
| `into_boxed` | builders are values already | not-applicable | — |
| `for_update` and locks | one writer at a time | not-applicable | — |
| `debug_query` | statements are JSON (`serde_json::to_string`) | implemented | `engine.rs::test_statements_round_trip_as_json` |

## Expressions (`ExpressionMethods`, `TextExpressionMethods`, `BoolExpressionMethods`)

| Diesel | Here | Status | Test |
|---|---|---|---|
| `eq`, `ne`, `gt`, `ge`, `lt`, `le` | same names | implemented | `engine.rs::test_every_operator_matches_the_in_memory_oracle` |
| `eq_any`, `ne_all` | same names | implemented | `engine.rs::test_every_operator_matches_the_in_memory_oracle` |
| `is_null`, `is_not_null` | same names | implemented | `engine.rs::test_every_operator_matches_the_in_memory_oracle` |
| `between`, `not_between` | same names | implemented | `engine.rs::test_every_operator_matches_the_in_memory_oracle` |
| `like`, `ilike` | same names | implemented | `engine.rs::test_every_operator_matches_the_in_memory_oracle` |
| `not_like`, `not_ilike` | `!col(f).like(p)` | implemented through `not` | `engine.rs::test_every_operator_matches_the_in_memory_oracle` (`!` and `like` each) |
| `and`, `or` | `.and(e)`, `.or(e)` | implemented | `engine.rs::test_every_operator_matches_the_in_memory_oracle` |
| `not` | `!e` (Rust), `.not()` (Dart) | implemented | `engine.rs::test_every_operator_matches_the_in_memory_oracle` |
| `asc`, `desc` | same names | implemented | `engine.rs::test_or_filter_and_offset_like_diesel` (both) |
| arithmetic, `concat`, SQL functions | — | not-applicable | — |

## Aggregates (`dsl::count`, `sum`, `avg`, `min`, `max`)

| Diesel | Here | Status | Test |
|---|---|---|---|
| `count_star`, `count` | `count`, `count_of(f)` | implemented | `sqlite_reference.rs::group_by_with_every_aggregate_matches_sqlite` |
| `sum`, `avg`, `min`, `max` | same names | implemented | `sqlite_reference.rs::group_by_with_every_aggregate_matches_sqlite` |
| `count_distinct` | — | planned | — |

## Writes

| Diesel | Here | Status | Test |
|---|---|---|---|
| `insert_into(t).values(rows)` | `Query::insert_into(t, rows)` | implemented | `engine.rs::test_insert_find_and_select_like_diesel` |
| `returning`, `get_results` | `get_results` | implemented | `engine.rs::test_auto_increment_and_aggregates` |
| `on_conflict().do_nothing()` | `on_conflict_do_nothing` (primary key) | partial: the primary key only | `engine.rs::test_constraints_and_conflicts` |
| `on_conflict().do_update().set(…)` | `on_conflict_replace` (the whole row) | partial: replaces the row | `engine.rs::test_constraints_and_conflicts` |
| `update(t).filter(…).set(…)` | `Query::update(t).filter(…).set(…)` | implemented | `engine.rs::test_update_delete_and_expectations` |
| `column + n` in `set` | `increment(f, n)` | implemented | `relational.rs::test_increment_two_integers_stay_integer_others_become_float` |
| `delete(t.filter(…))` | `Query::delete(t).filter(…)` | implemented | `engine.rs::test_update_delete_and_expectations` |
| — | `expect_affected_rows(n)` (extra) | implemented | `engine.rs::test_update_delete_and_expectations` |

## Transactions (`Connection`)

| Diesel | Here | Status | Test |
|---|---|---|---|
| `transaction` (commit on `Ok`, roll back on `Err`) | `Db::transaction` | implemented | `engine.rs::test_transaction_commit_rollback_and_rollback_only` |
| nested `transaction` (savepoints) | `WriteTx::savepoint` | implemented | `engine.rs::test_savepoints` |
| `build_transaction().read_only()` | `Db::read_transaction` | implemented | `engine.rs::test_read_transaction_is_a_snapshot_and_reentrancy_is_rejected` |
| one transaction for many statements | `Db::batch` | implemented | `engine.rs::test_batch_is_atomic` |

## Associations

| Diesel | Here | Status | Test |
|---|---|---|---|
| `belonging_to`, `grouped_by` | db_dsl `belongingTo`, `Associations.groupedBy` | implemented (Dart) | conformance: `belongingTo loads the children of some parents; groupedBy attaches them` |
| many-to-many through a join table | db_dsl `Relation` (`attach`, `detach`, `targetsOf`, `sourcesOf`) | implemented (Dart) | conformance: `a relation links both ways, and detach keeps the rows` |
| `Identifiable`, `Associations` derives | — | not-applicable | — |

## Schema and code generation

| Diesel | Here | Status | Test |
|---|---|---|---|
| `table!`, `Queryable`, `Insertable`, `Selectable` | tables declared at run time (`TableDef`, db_dsl `DbTable`); rows are JSON (`serde`) | not-applicable | — |
| migrations | `define_table` creates tables and builds or drops indexes | not-applicable | `engine.rs::test_schema_persists_and_indexes_are_built_on_existing_rows` |
| `sql_query` | — | not-applicable | — |
| `explain` | `Db::explain`, `Join::explain` (extra) | implemented | `engine.rs::test_explain_uses_indexes`, `relational.rs::a_join_explains_its_strategy_without_running` |

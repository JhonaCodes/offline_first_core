//! Statement executor.
//!
//! Reads work on any transaction; writes need the write transaction they run
//! in. No function here opens or commits a transaction: callers decide the
//! atomic unit (one statement, a batch, or an interactive transaction).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::iter;

use natdb::{Cursor, RwTransaction, Transaction, WriteFlags};
use natdb_sys::{MDB_FIRST, MDB_LAST, MDB_NEXT, MDB_PREV, MDB_SET_RANGE};
use serde_json::{json, Map, Number, Value};

use super::error::{EngineError, EngineResult};
use super::keys::{encode_key, encode_values, prefix_successor};
use super::plan::{self, Access, Plan};
use super::stmt::{
    Aggregate, AggregateSpec, Expr, GroupFunction, GroupQuery, JoinKind, JoinQuery, OnConflict,
    OrderBy, Output, Select, Statement,
};
use super::store::{Index, Store, Table};
use super::value::{field, sql_eq, total_cmp};

const NULL: Value = Value::Null;

/// Decodes a stored row of `table`.
pub(crate) fn decode_row(table: &str, bytes: &[u8]) -> EngineResult<Value> {
    serde_json::from_slice(bytes).map_err(|e| EngineError::CorruptRecord {
        table: table.to_string(),
        detail: e.to_string(),
    })
}

/// Visits the `(primary key, row bytes)` pairs selected by `plan`, until
/// `visit` returns `false`. Without `fetch`, an index scan passes empty row
/// bytes instead of reading each row.
fn scan<T: Transaction>(
    txn: &T,
    table: &Table,
    plan: &Plan,
    fetch: bool,
    mut visit: impl FnMut(&[u8], &[u8]) -> EngineResult<bool>,
) -> EngineResult<()> {
    match &plan.access {
        Access::PkPoint(key) => match txn.get(table.db, key) {
            Ok(row) => visit(key, row).map(|_| ()),
            Err(natdb::Error::NotFound) => Ok(()),
            Err(e) => Err(e.into()),
        },
        Access::Table { lower, upper } => walk(
            txn,
            table.db,
            &[],
            lower.as_deref(),
            upper.as_deref(),
            plan.desc,
            |key, row| visit(key, row),
        ),
        Access::Index {
            index,
            prefix,
            lower,
            upper,
        } => {
            let index = table
                .indexes
                .get(*index)
                .ok_or_else(|| EngineError::InvalidRequest("unknown index".to_string()))?;
            walk(
                txn,
                index.db,
                prefix,
                lower.as_deref(),
                upper.as_deref(),
                plan.desc,
                |_, pk| {
                    if !fetch {
                        return visit(pk, &[]);
                    }
                    match txn.get(table.db, &pk) {
                        Ok(row) => visit(pk, row),
                        // An index entry always has its row; tolerate nothing else.
                        Err(natdb::Error::NotFound) => Err(EngineError::CorruptRecord {
                            table: table.def.name.clone(),
                            detail: format!("index `{}` points to a missing row", index.def.name),
                        }),
                        Err(e) => Err(e.into()),
                    }
                },
            )
        }
    }
}

/// Walks the keys of `db` that start with `prefix`, from `lower` (inclusive)
/// to `upper` (exclusive), ascending or descending.
fn walk<T: Transaction>(
    txn: &T,
    db: natdb::Database,
    prefix: &[u8],
    lower: Option<&[u8]>,
    upper: Option<&[u8]>,
    desc: bool,
    mut visit: impl FnMut(&[u8], &[u8]) -> EngineResult<bool>,
) -> EngineResult<()> {
    let cursor = txn.open_ro_cursor(db)?;
    let step = |op| -> EngineResult<Option<(&[u8], &[u8])>> {
        match cursor.get(None, None, op) {
            Ok((Some(key), value)) => Ok(Some((key, value))),
            Ok((None, _)) | Err(natdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    };
    let seek = |key: &[u8]| -> EngineResult<Option<(&[u8], &[u8])>> {
        match cursor.get(Some(key), None, MDB_SET_RANGE) {
            Ok((Some(key), value)) => Ok(Some((key, value))),
            Ok((None, _)) | Err(natdb::Error::NotFound) => Ok(None),
            Err(e) => Err(e.into()),
        }
    };

    if !desc {
        let start = lower.unwrap_or(prefix);
        // LMDB rejects an empty key: an unbounded scan starts at the first key.
        let mut entry = if start.is_empty() {
            step(MDB_FIRST)?
        } else {
            seek(start)?
        };
        while let Some((key, value)) = entry {
            if !key.starts_with(prefix) || upper.is_some_and(|upper| key >= upper) {
                break;
            }
            if !visit(key, value)? {
                break;
            }
            entry = step(MDB_NEXT)?;
        }
        return Ok(());
    }

    // Descending: position on the last key before the end, then step back.
    let end = upper
        .map(<[u8]>::to_vec)
        .or_else(|| prefix_successor(prefix));
    let mut entry = match end {
        Some(end) => match seek(&end)? {
            Some(_) => step(MDB_PREV)?,
            None => step(MDB_LAST)?,
        },
        None => step(MDB_LAST)?,
    };
    while let Some((key, value)) = entry {
        if !key.starts_with(prefix) || lower.is_some_and(|lower| key < lower) {
            break;
        }
        if !visit(key, value)? {
            break;
        }
        entry = step(MDB_PREV)?;
    }
    Ok(())
}

/// Rows matching `filter`, following `plan`: `(primary key, bytes, row)`.
type Matched = Vec<(Vec<u8>, Vec<u8>, Value)>;

fn collect<T: Transaction>(
    txn: &T,
    table: &Table,
    plan: &Plan,
    filter: Option<&Expr>,
    limit: Option<usize>,
) -> EngineResult<Matched> {
    let mut rows = Vec::new();
    scan(txn, table, plan, true, |key, bytes| {
        let row = decode_row(&table.def.name, bytes)?;
        if plan.exact || filter.is_none_or(|f| f.matches(&row)) {
            rows.push((key.to_vec(), bytes.to_vec(), row));
        }
        Ok(limit.is_none_or(|limit| rows.len() < limit))
    })?;
    Ok(rows)
}

fn sort_rows(rows: &mut Matched, order: &[OrderBy]) {
    rows.sort_by(|(_, _, a), (_, _, b)| {
        for key in order {
            let x = field(a, &key.field).unwrap_or(&NULL);
            let y = field(b, &key.field).unwrap_or(&NULL);
            let ordering = total_cmp(x, y);
            let ordering = if key.desc {
                ordering.reverse()
            } else {
                ordering
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    });
}

fn to_usize(value: Option<u64>) -> Option<usize> {
    value.map(|v| usize::try_from(v).unwrap_or(usize::MAX))
}

fn select<T: Transaction>(store: &Store, txn: &T, query: &Select) -> EngineResult<Vec<Vec<u8>>> {
    if query.fields.is_empty() && !query.distinct {
        return select_plain(store, txn, query);
    }
    select_projected(store, txn, query)
}

/// `select` without projection or distinct: the fast path that can hand back
/// stored bytes untouched and cut a sorted scan short at `offset + limit`.
fn select_plain<T: Transaction>(
    store: &Store,
    txn: &T,
    query: &Select,
) -> EngineResult<Vec<Vec<u8>>> {
    let table = store.table(&query.table)?;
    let plan = plan::choose(&table, query.filter.as_ref(), &query.order);
    let offset = to_usize(query.offset).unwrap_or(0);
    let limit = to_usize(query.limit);
    let early = if plan.ordered {
        limit.map(|limit| limit.saturating_add(offset))
    } else {
        None
    };
    if plan.exact && plan.ordered {
        // Neither the filter nor the order needs the decoded rows.
        let mut rows = Vec::new();
        scan(txn, &table, &plan, true, |_, bytes| {
            rows.push(bytes.to_vec());
            Ok(early.is_none_or(|early| rows.len() < early))
        })?;
        return Ok(rows
            .into_iter()
            .skip(offset)
            .take(limit.unwrap_or(usize::MAX))
            .collect());
    }
    let mut rows = collect(txn, &table, &plan, query.filter.as_ref(), early)?;
    if !plan.ordered {
        sort_rows(&mut rows, &query.order);
    }
    Ok(rows
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|(_, bytes, _)| bytes)
        .collect())
}

/// `select` with `fields` and/or `distinct`: filter and order need the whole
/// matching set before projecting, since distinct can drop rows that would
/// otherwise survive `offset`/`limit`.
fn select_projected<T: Transaction>(
    store: &Store,
    txn: &T,
    query: &Select,
) -> EngineResult<Vec<Vec<u8>>> {
    validate_fields(&query.fields)?;
    let table = store.table(&query.table)?;
    let plan = plan::choose(&table, query.filter.as_ref(), &query.order);
    let mut rows = collect(txn, &table, &plan, query.filter.as_ref(), None)?;
    if !plan.ordered {
        sort_rows(&mut rows, &query.order);
    }
    let mut seen = query.distinct.then(HashSet::new);
    let mut projected = Vec::new();
    for (_, _, row) in &rows {
        if let Some(seen) = &mut seen {
            if !seen.insert(distinct_key(row, &query.fields)) {
                continue;
            }
        }
        projected.push(project(row, &query.fields)?);
    }
    let offset = to_usize(query.offset).unwrap_or(0);
    let limit = to_usize(query.limit);
    projected
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|row| encode_row(&row))
        .collect()
}

/// Rejects an empty path, a path repeated in `fields`, or a path that is a
/// prefix of another one (`"a"` and `"a.b"`).
fn validate_fields(fields: &[String]) -> EngineResult<()> {
    let mut seen = HashSet::new();
    for path in fields {
        if path.is_empty() {
            return Err(EngineError::InvalidRequest(
                "a projected field path must not be empty".to_string(),
            ));
        }
        if !seen.insert(path.as_str()) {
            return Err(EngineError::InvalidRequest(format!(
                "projected field path `{path}` is repeated"
            )));
        }
    }
    for a in fields {
        for b in fields {
            if a != b && is_prefix_path(a, b) {
                return Err(EngineError::InvalidRequest(format!(
                    "projected field path `{a}` is a prefix of `{b}`"
                )));
            }
        }
    }
    Ok(())
}

/// Whether `a.b` is a (dot-boundary) prefix path of `b`.
fn is_prefix_path(a: &str, b: &str) -> bool {
    b.strip_prefix(a).is_some_and(|rest| rest.starts_with('.'))
}

/// The output row of `select`'s projection: `row`'s value at every path of
/// `fields`, `null` when missing, placed at the same nested path. Without
/// `fields`, the whole row.
fn project(row: &Value, fields: &[String]) -> EngineResult<Value> {
    if fields.is_empty() {
        return Ok(row.clone());
    }
    let mut out = Value::Object(Map::new());
    for path in fields {
        let value = field(row, path).cloned().unwrap_or(NULL);
        set_path(&mut out, path, value)?;
    }
    Ok(out)
}

/// The key `distinct` compares: the encoding of `row`'s values at `fields`, in
/// order, or of the whole row without `fields`.
fn distinct_key(row: &Value, fields: &[String]) -> Vec<u8> {
    if fields.is_empty() {
        return encode_key(row);
    }
    let values: Vec<Value> = fields
        .iter()
        .map(|path| field(row, path).cloned().unwrap_or(NULL))
        .collect();
    encode_values(values.iter())
}

/// The plan `explain` reports for `query`.
pub fn explain(store: &Store, query: &Select) -> EngineResult<Value> {
    let table = store.table(&query.table)?;
    Ok(plan::choose(&table, query.filter.as_ref(), &query.order).explain(&table))
}

/// The plan of a join (`explain`), as [`join`] runs it: every table is read
/// in full, each step builds a hash of the joined table by `on.right` and
/// probes it with the combined rows, and the filter, if any, runs on the
/// combined rows.
pub fn explain_join(store: &Store, query: &JoinQuery) -> EngineResult<Value> {
    let from_alias = join_alias(&query.from.alias, &query.from.table);
    let sources = iter::once((query.from.table.as_str(), from_alias)).chain(
        query
            .joins
            .iter()
            .map(|step| (step.table.as_str(), join_alias(&step.alias, &step.table))),
    );
    let mut aliases = Vec::new();
    let mut tables = Vec::new();
    for (table, alias) in sources {
        store.table(table)?;
        aliases.push(alias);
        tables.push(json!({"table": table, "as": alias, "access": "full_scan"}));
    }
    validate_join_aliases(&aliases)?;

    Ok(json!({
        "strategy": "hash_join",
        "tables": tables,
        "filter": if query.filter.is_some() { "after_join" } else { "none" },
    }))
}

fn count<T: Transaction>(
    store: &Store,
    txn: &T,
    table: &str,
    filter: Option<&Expr>,
) -> EngineResult<u64> {
    let table = store.table(table)?;
    if filter.is_none() {
        return Ok(txn.stat(table.db)?.entries() as u64);
    }
    let plan = plan::choose(&table, filter, &[]);
    let mut total = 0u64;
    // An exact plan counts keys without reading rows.
    scan(txn, &table, &plan, !plan.exact, |_, bytes| {
        let matched = match filter {
            Some(filter) if !plan.exact => filter.matches(&decode_row(&table.def.name, bytes)?),
            _ => true,
        };
        total += u64::from(matched);
        Ok(true)
    })?;
    Ok(total)
}

fn aggregate<T: Transaction>(
    store: &Store,
    txn: &T,
    table: &str,
    filter: Option<&Expr>,
    function: Aggregate,
    target: &str,
) -> EngineResult<Value> {
    let table = store.table(table)?;
    let plan = plan::choose(&table, filter, &[]);
    let rows = collect(txn, &table, &plan, filter, None)?;
    let values = rows
        .iter()
        .filter_map(|(_, _, row)| field(row, target).filter(|v| !v.is_null()));
    Ok(aggregate_value(values, function))
}

/// Computes `function` over `values` (already filtered to non-`null`): the
/// semantics shared by the `aggregate` statement and `group`'s aggregates.
/// Sum of integers stays an integer unless it overflows `i64`, then falls
/// back to `f64`; average is always `f64`; min/max compare in total order.
fn aggregate_value<'a>(values: impl Iterator<Item = &'a Value>, function: Aggregate) -> Value {
    match function {
        Aggregate::Min => values
            .min_by(|a, b| total_cmp(a, b))
            .cloned()
            .unwrap_or(NULL),
        Aggregate::Max => values
            .max_by(|a, b| total_cmp(a, b))
            .cloned()
            .unwrap_or(NULL),
        Aggregate::Sum | Aggregate::Avg => {
            let numbers: Vec<&Number> = values.filter_map(Value::as_number).collect();
            if numbers.is_empty() {
                NULL
            } else if function == Aggregate::Avg {
                let sum: f64 = numbers.iter().filter_map(|n| n.as_f64()).sum();
                Number::from_f64(sum / numbers.len() as f64).map_or(NULL, Value::Number)
            } else if numbers.iter().all(|n| n.is_i64()) {
                let sum = numbers
                    .iter()
                    .filter_map(|n| n.as_i64())
                    .try_fold(0i64, i64::checked_add);
                match sum {
                    Some(sum) => Value::from(sum),
                    None => Number::from_f64(numbers.iter().filter_map(|n| n.as_f64()).sum())
                        .map_or(NULL, Value::Number),
                }
            } else {
                Number::from_f64(numbers.iter().filter_map(|n| n.as_f64()).sum())
                    .map_or(NULL, Value::Number)
            }
        }
    }
}

/// `GroupFunction`'s numeric functions (everything but `count`), as an
/// [`Aggregate`]: the two enums share the same semantics.
fn as_aggregate(function: GroupFunction) -> Option<Aggregate> {
    match function {
        GroupFunction::Count => None,
        GroupFunction::Sum => Some(Aggregate::Sum),
        GroupFunction::Avg => Some(Aggregate::Avg),
        GroupFunction::Min => Some(Aggregate::Min),
        GroupFunction::Max => Some(Aggregate::Max),
    }
}

/// Rejects an empty `by` or aggregate `field` path, a `sum`/`avg`/`min`/`max`
/// aggregate without `field`, and an `as` alias that is empty, holds `.`, is
/// repeated, or collides with the first segment of a `by` path.
fn validate_group_spec(by: &[String], aggregates: &[AggregateSpec]) -> EngineResult<()> {
    for path in by {
        if path.is_empty() {
            return Err(EngineError::InvalidRequest(
                "a `by` path must not be empty".to_string(),
            ));
        }
    }
    let by_heads: HashSet<&str> = by
        .iter()
        .map(|path| path.split('.').next().unwrap_or(path.as_str()))
        .collect();
    let mut aliases = HashSet::new();
    for spec in aggregates {
        match &spec.field {
            Some(field) if field.is_empty() => {
                return Err(EngineError::InvalidRequest(
                    "an aggregate field path must not be empty".to_string(),
                ))
            }
            None if spec.function != GroupFunction::Count => {
                return Err(EngineError::InvalidRequest(format!(
                    "aggregate `{}` needs a field",
                    spec.alias
                )));
            }
            _ => {}
        }
        if spec.alias.is_empty() || spec.alias.contains('.') {
            return Err(EngineError::InvalidRequest(format!(
                "aggregate alias `{}` must be non-empty and without `.`",
                spec.alias
            )));
        }
        if !aliases.insert(spec.alias.as_str()) {
            return Err(EngineError::InvalidRequest(format!(
                "aggregate alias `{}` is repeated",
                spec.alias
            )));
        }
        if by_heads.contains(spec.alias.as_str()) {
            return Err(EngineError::InvalidRequest(format!(
                "aggregate alias `{}` collides with a `by` path",
                spec.alias
            )));
        }
    }
    Ok(())
}

/// One aggregate of a group's member rows.
fn group_aggregate(members: &[Value], spec: &AggregateSpec) -> Value {
    if spec.function == GroupFunction::Count {
        let count = match &spec.field {
            Some(path) => members
                .iter()
                .filter(|row| field(row, path).is_some_and(|v| !v.is_null()))
                .count(),
            None => members.len(),
        };
        return Value::from(count as u64);
    }
    // `validate_group_spec` guarantees `field` is set for every other
    // function, and `as_aggregate` maps every non-`count` function.
    let (Some(path), Some(function)) = (&spec.field, as_aggregate(spec.function)) else {
        return NULL;
    };
    let values = members
        .iter()
        .filter_map(|row| field(row, path).filter(|v| !v.is_null()));
    aggregate_value(values, function)
}

fn group<T: Transaction>(store: &Store, txn: &T, query: &GroupQuery) -> EngineResult<Vec<Vec<u8>>> {
    validate_group_spec(&query.by, &query.aggregates)?;
    let table = store.table(&query.table)?;
    let plan = plan::choose(&table, query.filter.as_ref(), &[]);
    let matched = collect(txn, &table, &plan, query.filter.as_ref(), None)?;

    // Groups, in first-seen order: (key encoding, `by` values, member rows).
    let mut groups: Vec<(Vec<u8>, Vec<Value>, Vec<Value>)> = Vec::new();
    if query.by.is_empty() {
        // One group over everything, even with no matching row.
        let members: Vec<Value> = matched.iter().map(|(_, _, row)| row.clone()).collect();
        groups.push((Vec::new(), Vec::new(), members));
    } else {
        let mut index: HashMap<Vec<u8>, usize> = HashMap::new();
        for (_, _, row) in &matched {
            let values: Vec<Value> = query
                .by
                .iter()
                .map(|path| field(row, path).cloned().unwrap_or(NULL))
                .collect();
            let key = encode_values(values.iter());
            match index.get(&key) {
                Some(&position) => groups[position].2.push(row.clone()),
                None => {
                    index.insert(key.clone(), groups.len());
                    groups.push((key, values, vec![row.clone()]));
                }
            }
        }
    }
    // Deterministic default order: ascending by the `by` key encoding.
    groups.sort_by(|a, b| a.0.cmp(&b.0));

    let mut outputs = Vec::with_capacity(groups.len());
    for (_, values, members) in &groups {
        let mut out = Value::Object(Map::new());
        for (path, value) in query.by.iter().zip(values) {
            set_path(&mut out, path, value.clone())?;
        }
        if let Value::Object(map) = &mut out {
            for spec in &query.aggregates {
                map.insert(spec.alias.clone(), group_aggregate(members, spec));
            }
        }
        outputs.push(out);
    }
    if let Some(having) = &query.having {
        outputs.retain(|row| having.matches(row));
    }
    if !query.order.is_empty() {
        sort_by_order(&mut outputs, &query.order);
    }
    let offset = to_usize(query.offset).unwrap_or(0);
    let limit = to_usize(query.limit);
    outputs
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|row| encode_row(&row))
        .collect()
}

/// Stable sort of plain JSON rows (not tied to a table's storage) by `order`,
/// in total order. Shared by `group` and `join`, whose output rows are built
/// in memory rather than read from a table scan.
fn sort_by_order(rows: &mut [Value], order: &[OrderBy]) {
    rows.sort_by(|a, b| {
        for key in order {
            let x = field(a, &key.field).unwrap_or(&NULL);
            let y = field(b, &key.field).unwrap_or(&NULL);
            let ordering = total_cmp(x, y);
            let ordering = if key.desc {
                ordering.reverse()
            } else {
                ordering
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    });
}

/// `alias`, or `table` when there is none (`from.as` / `joins[].as`).
fn join_alias<'a>(alias: &'a Option<String>, table: &'a str) -> &'a str {
    alias.as_deref().unwrap_or(table)
}

/// Rejects an empty or `.`-holding alias, and a repeated one.
fn validate_join_aliases(aliases: &[&str]) -> EngineResult<()> {
    let mut seen = HashSet::new();
    for alias in aliases {
        if alias.is_empty() || alias.contains('.') {
            return Err(EngineError::InvalidRequest(format!(
                "join alias `{alias}` must be non-empty and without `.`"
            )));
        }
        if !seen.insert(*alias) {
            return Err(EngineError::InvalidRequest(format!(
                "join alias `{alias}` is repeated"
            )));
        }
    }
    Ok(())
}

fn join<T: Transaction>(store: &Store, txn: &T, query: &JoinQuery) -> EngineResult<Vec<Vec<u8>>> {
    let from_alias = join_alias(&query.from.alias, &query.from.table).to_string();
    let mut aliases = vec![from_alias.as_str()];
    let step_aliases: Vec<&str> = query
        .joins
        .iter()
        .map(|step| join_alias(&step.alias, &step.table))
        .collect();
    aliases.extend(step_aliases.iter().copied());
    validate_join_aliases(&aliases)?;

    let from_table = store.table(&query.from.table)?;
    let from_plan = plan::choose(&from_table, None, &[]);
    let from_rows = collect(txn, &from_table, &from_plan, None, None)?;
    let mut combined: Vec<Value> = from_rows
        .into_iter()
        .map(|(_, _, row)| {
            let mut object = Map::new();
            object.insert(from_alias.clone(), row);
            Value::Object(object)
        })
        .collect();

    for (step, step_alias) in query.joins.iter().zip(step_aliases) {
        let joined_table = store.table(&step.table)?;
        let joined_plan = plan::choose(&joined_table, None, &[]);
        let joined_rows = collect(txn, &joined_table, &joined_plan, None, None)?;
        // Buckets by the encoded key of `on.right`, in primary key order
        // within each bucket, so matches come out in that order.
        let mut buckets: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        for (position, (_, _, candidate)) in joined_rows.iter().enumerate() {
            if let Some(value) = field(candidate, &step.on.right).filter(|v| !v.is_null()) {
                buckets.entry(encode_key(value)).or_default().push(position);
            }
        }
        let mut next = Vec::new();
        for row in &combined {
            let left = field(row, &step.on.left).filter(|v| !v.is_null());
            let matches: Vec<usize> = left
                .and_then(|v| buckets.get(&encode_key(v)))
                .into_iter()
                .flatten()
                .copied()
                .filter(|&position| {
                    let (_, _, candidate) = &joined_rows[position];
                    // The encoded key can collide for very large integers
                    // sharing an `f64`; `eq` is the source of truth.
                    left.zip(field(candidate, &step.on.right))
                        .is_some_and(|(v, c)| sql_eq(v, c))
                })
                .collect();
            if matches.is_empty() {
                match step.kind {
                    JoinKind::Inner => {}
                    JoinKind::Left => {
                        let mut extended = row.clone();
                        if let Value::Object(map) = &mut extended {
                            map.insert(step_alias.to_string(), NULL);
                        }
                        next.push(extended);
                    }
                }
                continue;
            }
            for position in matches {
                let (_, _, candidate) = &joined_rows[position];
                let mut extended = row.clone();
                if let Value::Object(map) = &mut extended {
                    map.insert(step_alias.to_string(), candidate.clone());
                }
                next.push(extended);
            }
        }
        combined = next;
    }

    let mut combined: Vec<Value> = combined
        .into_iter()
        .filter(|row| query.filter.as_ref().is_none_or(|f| f.matches(row)))
        .collect();
    if !query.order.is_empty() {
        sort_by_order(&mut combined, &query.order);
    }
    let offset = to_usize(query.offset).unwrap_or(0);
    let limit = to_usize(query.limit);
    combined
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|row| encode_row(&row))
        .collect()
}

/// Executes a read-only statement.
pub fn execute_read<T: Transaction>(
    store: &Store,
    txn: &T,
    statement: &Statement,
) -> EngineResult<Output> {
    match statement {
        Statement::Select(query) => select(store, txn, query).map(Output::Rows),
        Statement::Count { table, filter } => {
            count(store, txn, table, filter.as_ref()).map(Output::Count)
        }
        Statement::Aggregate {
            table,
            filter,
            function,
            field,
        } => aggregate(store, txn, table, filter.as_ref(), *function, field).map(Output::Value),
        Statement::Find { table, key } => {
            let table = store.table(table)?;
            match txn.get(table.db, &encode_key(key)) {
                Ok(row) => Ok(Output::Row(Some(row.to_vec()))),
                Err(natdb::Error::NotFound) => Ok(Output::Row(None)),
                Err(e) => Err(e.into()),
            }
        }
        Statement::Group(query) => group(store, txn, query).map(Output::Rows),
        Statement::Join(query) => join(store, txn, query).map(Output::Rows),
        Statement::Insert { .. } | Statement::Update { .. } | Statement::Delete { .. } => {
            Err(EngineError::ReadOnlyTransaction)
        }
    }
}

/// Executes any statement inside the write transaction `txn`.
///
/// On error the transaction may hold part of the statement's writes: callers
/// must abort it (or the enclosing savepoint), never commit it.
pub fn execute_write(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    statement: &Statement,
) -> EngineResult<Output> {
    match statement {
        Statement::Insert {
            table,
            rows,
            on_conflict,
        } => insert(store, txn, table, rows, *on_conflict),
        Statement::Update {
            table,
            filter,
            set,
            increment,
            expect,
        } => update(store, txn, table, filter.as_ref(), set, increment, *expect),
        Statement::Delete {
            table,
            filter,
            expect,
        } => delete(store, txn, table, filter.as_ref(), *expect),
        read => execute_read(store, &*txn, read),
    }
}

fn check_key(store: &Store, key: &[u8]) -> EngineResult<()> {
    if key.is_empty() || key.len() > store.max_key_size() {
        return Err(EngineError::KeyTooLarge {
            size: key.len(),
            max: store.max_key_size(),
        });
    }
    Ok(())
}

fn index_values<'a>(index: &Index, row: &'a Value) -> Vec<&'a Value> {
    index
        .def
        .fields
        .iter()
        .map(|f| field(row, f).unwrap_or(&NULL))
        .collect()
}

/// Key of the entry of `row` in `index`. A unique index stores the values
/// alone, unless one of them is `NULL`: like SQL, several rows may hold `NULL`
/// in a unique index, so those entries carry the primary key too.
fn index_key(index: &Index, row: &Value, pk: &[u8]) -> (Vec<u8>, bool) {
    let values = index_values(index, row);
    let enforce = index.def.unique && values.iter().all(|v| !v.is_null());
    let mut key = encode_values(values);
    if !enforce {
        key.extend_from_slice(pk);
    }
    (key, enforce)
}

/// Adds the entry of `row` (primary key `pk`) to `index`.
pub(crate) fn add_index_entry(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    table: &Table,
    index: &Index,
    row: &Value,
    pk: &[u8],
) -> EngineResult<()> {
    let (key, enforce) = index_key(index, row, pk);
    check_key(store, &key)?;
    if enforce {
        match txn.get(index.db, &key) {
            Ok(owner) if owner != pk => {
                let values: Vec<&Value> = index_values(index, row);
                return Err(EngineError::UniqueViolation {
                    table: table.def.name.clone(),
                    index: index.def.name.clone(),
                    value: serde_json::to_string(&values).unwrap_or_default(),
                });
            }
            Ok(_) | Err(natdb::Error::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
    }
    txn.put(index.db, &key, &pk, WriteFlags::empty())?;
    Ok(())
}

fn remove_index_entries(
    txn: &mut RwTransaction<'_>,
    table: &Table,
    row: &Value,
    pk: &[u8],
) -> EngineResult<()> {
    for index in &table.indexes {
        let (key, _) = index_key(index, row, pk);
        match txn.del(index.db, &key, None) {
            Ok(()) | Err(natdb::Error::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn add_index_entries(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    table: &Table,
    row: &Value,
    pk: &[u8],
) -> EngineResult<()> {
    for index in &table.indexes {
        add_index_entry(store, txn, table, index, row, pk)?;
    }
    Ok(())
}

fn encode_row(row: &Value) -> EngineResult<Vec<u8>> {
    serde_json::to_vec(row).map_err(|e| EngineError::InvalidRequest(e.to_string()))
}

fn insert(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    table_name: &str,
    rows: &[Value],
    on_conflict: OnConflict,
) -> EngineResult<Output> {
    let table = store.table(table_name)?;
    let pk_field = table.def.primary_key.clone();
    let mut written = Vec::with_capacity(rows.len());
    for row in rows {
        let Value::Object(object) = row else {
            return Err(EngineError::InvalidRequest(format!(
                "rows of `{table_name}` must be JSON objects"
            )));
        };
        let mut row = Value::Object(object.clone());
        let pk = match table.primary_key(&row) {
            Some(pk) => {
                if table.def.auto_increment {
                    if let Some(value) = field(&row, &pk_field).and_then(Value::as_i64) {
                        store.bump_sequence(txn, table_name, value)?;
                    }
                }
                pk
            }
            None if table.def.auto_increment && !pk_field.contains('.') => {
                let id = store.next_sequence(txn, table_name)?;
                if let Value::Object(object) = &mut row {
                    object.insert(pk_field.clone(), Value::from(id));
                }
                encode_key(&Value::from(id))
            }
            None => {
                return Err(EngineError::MissingPrimaryKey {
                    table: table_name.to_string(),
                    field: pk_field,
                })
            }
        };
        check_key(store, &pk)?;
        let previous = match txn.get(table.db, &pk) {
            Ok(bytes) => Some(decode_row(table_name, bytes)?),
            Err(natdb::Error::NotFound) => None,
            Err(e) => return Err(e.into()),
        };
        if let Some(previous) = previous {
            match on_conflict {
                OnConflict::Error => {
                    return Err(EngineError::DuplicateKey {
                        table: table_name.to_string(),
                        key: field(&row, &pk_field)
                            .map(Value::to_string)
                            .unwrap_or_default(),
                    })
                }
                OnConflict::Ignore => continue,
                OnConflict::Replace => remove_index_entries(txn, &table, &previous, &pk)?,
            }
        }
        add_index_entries(store, txn, &table, &row, &pk)?;
        let bytes = encode_row(&row)?;
        txn.put(table.db, &pk, &bytes, WriteFlags::empty())?;
        written.push(bytes);
    }
    Ok(Output::Affected {
        count: written.len() as u64,
        rows: written,
    })
}

/// Sets the value at the dot-separated `path` of `row`, creating objects on
/// the way.
fn set_path(row: &mut Value, path: &str, value: Value) -> EngineResult<()> {
    let mut target = row;
    let mut segments = path.split('.').peekable();
    while let Some(segment) = segments.next() {
        let Value::Object(object) = target else {
            return Err(EngineError::InvalidRequest(format!(
                "cannot set `{path}`: `{segment}` is inside a non-object value"
            )));
        };
        if segments.peek().is_none() {
            object.insert(segment.to_string(), value);
            return Ok(());
        }
        target = object
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    Ok(())
}

fn matching(
    txn: &RwTransaction<'_>,
    table: &Table,
    filter: Option<&Expr>,
) -> EngineResult<Matched> {
    let plan = plan::choose(table, filter, &[]);
    collect(txn, table, &plan, filter, None)
}

fn check_expected(expect: Option<u64>, actual: u64) -> EngineResult<()> {
    match expect {
        Some(expected) if expected != actual => {
            Err(EngineError::AffectedRowsMismatch { expected, actual })
        }
        _ => Ok(()),
    }
}

/// Whether `path` is the primary key `pk_field`, or nested under it.
fn under_field(path: &str, pk_field: &str) -> bool {
    path == pk_field || path.starts_with(&format!("{pk_field}."))
}

/// `current + delta`, where a missing or `null` `current` counts as `0`.
/// Two integers give an `i64` (checked; overflow is an error); otherwise the
/// result is a finite `f64`. `current` must be `null` or a number.
fn increment_value(current: &Value, delta: &Number, path: &str) -> EngineResult<Value> {
    let current_number = match current {
        Value::Null => None,
        Value::Number(number) => Some(number),
        _ => {
            return Err(EngineError::InvalidRequest(format!(
                "cannot increment `{path}`: the current value is not a number"
            )))
        }
    };
    if delta.is_i64() && current_number.is_none_or(Number::is_i64) {
        let current = current_number.and_then(Number::as_i64).unwrap_or(0);
        let delta = delta.as_i64().unwrap_or(0);
        return current.checked_add(delta).map(Value::from).ok_or_else(|| {
            EngineError::InvalidRequest(format!("increment of `{path}` overflows i64"))
        });
    }
    let current = current_number.and_then(Number::as_f64).unwrap_or(0.0);
    let delta = delta.as_f64().unwrap_or(0.0);
    // JSON has no infinity: a sum that is not finite cannot be stored.
    Number::from_f64(current + delta)
        .map(Value::Number)
        .ok_or_else(|| {
            EngineError::InvalidRequest(format!("increment of `{path}` is not a finite number"))
        })
}

fn update(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    table_name: &str,
    filter: Option<&Expr>,
    set: &Map<String, Value>,
    increment: &Map<String, Value>,
    expect: Option<u64>,
) -> EngineResult<Output> {
    let table = store.table(table_name)?;
    let pk_field = &table.def.primary_key;
    if set.keys().any(|path| under_field(path, pk_field)) {
        return Err(EngineError::InvalidRequest(format!(
            "the primary key `{pk_field}` of `{table_name}` cannot be updated"
        )));
    }
    if increment.keys().any(|path| under_field(path, pk_field)) {
        return Err(EngineError::InvalidRequest(format!(
            "the primary key `{pk_field}` of `{table_name}` cannot be incremented"
        )));
    }
    for (path, delta) in increment {
        if !delta.is_number() {
            return Err(EngineError::InvalidRequest(format!(
                "increment of `{path}` must be a number"
            )));
        }
        if set.contains_key(path) {
            return Err(EngineError::InvalidRequest(format!(
                "`{path}` cannot be both set and incremented"
            )));
        }
    }
    let rows = matching(txn, &table, filter)?;
    check_expected(expect, rows.len() as u64)?;
    for (pk, _, old) in &rows {
        let mut new = old.clone();
        for (path, value) in set {
            set_path(&mut new, path, value.clone())?;
        }
        for (path, delta) in increment {
            let Value::Number(delta) = delta else {
                return Err(EngineError::InvalidRequest(format!(
                    "increment of `{path}` must be a number"
                )));
            };
            let current = field(&new, path).cloned().unwrap_or(NULL);
            let sum = increment_value(&current, delta, path)?;
            set_path(&mut new, path, sum)?;
        }
        remove_index_entries(txn, &table, old, pk)?;
        add_index_entries(store, txn, &table, &new, pk)?;
        txn.put(table.db, pk, &encode_row(&new)?, WriteFlags::empty())?;
    }
    Ok(Output::Affected {
        count: rows.len() as u64,
        rows: Vec::new(),
    })
}

fn delete(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    table_name: &str,
    filter: Option<&Expr>,
    expect: Option<u64>,
) -> EngineResult<Output> {
    let table = store.table(table_name)?;
    let rows = matching(txn, &table, filter)?;
    check_expected(expect, rows.len() as u64)?;
    for (pk, _, old) in &rows {
        remove_index_entries(txn, &table, old, pk)?;
        txn.del(table.db, pk, None)?;
    }
    Ok(Output::Affected {
        count: rows.len() as u64,
        rows: Vec::new(),
    })
}

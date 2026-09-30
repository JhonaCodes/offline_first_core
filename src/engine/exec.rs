//! Statement executor.
//!
//! Reads work on any transaction; writes need the write transaction they run
//! in. No function here opens or commits a transaction: callers decide the
//! atomic unit (one statement, a batch, or an interactive transaction).

use std::cmp::Ordering;

use natdb::{Cursor, RwTransaction, Transaction, WriteFlags};
use natdb_sys::{MDB_FIRST, MDB_LAST, MDB_NEXT, MDB_PREV, MDB_SET_RANGE};
use serde_json::{Map, Number, Value};

use super::error::{EngineError, EngineResult};
use super::keys::{encode_key, encode_values, prefix_successor};
use super::plan::{self, Access, Plan};
use super::stmt::{Aggregate, Expr, OnConflict, OrderBy, Output, Select, Statement};
use super::store::{Index, Store, Table};
use super::value::{field, total_cmp};

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

/// The plan `explain` reports for `query`.
pub fn explain(store: &Store, query: &Select) -> EngineResult<Value> {
    let table = store.table(&query.table)?;
    Ok(plan::choose(&table, query.filter.as_ref(), &query.order).explain(&table))
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
    Ok(match function {
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
    })
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
            expect,
        } => update(store, txn, table, filter.as_ref(), set, *expect),
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

fn update(
    store: &Store,
    txn: &mut RwTransaction<'_>,
    table_name: &str,
    filter: Option<&Expr>,
    set: &Map<String, Value>,
    expect: Option<u64>,
) -> EngineResult<Output> {
    let table = store.table(table_name)?;
    let pk_field = &table.def.primary_key;
    if set
        .keys()
        .any(|path| path == pk_field || path.starts_with(&format!("{pk_field}.")))
    {
        return Err(EngineError::InvalidRequest(format!(
            "the primary key `{pk_field}` of `{table_name}` cannot be updated"
        )));
    }
    let rows = matching(txn, &table, filter)?;
    check_expected(expect, rows.len() as u64)?;
    for (pk, _, old) in &rows {
        let mut new = old.clone();
        for (path, value) in set {
            set_path(&mut new, path, value.clone())?;
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

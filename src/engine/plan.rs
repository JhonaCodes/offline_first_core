//! Rule-based query planner.
//!
//! The planner chooses *which keys to visit*. Rules, in order:
//!
//! 1. `eq` on the primary key: one lookup.
//! 2. The index (or primary key) with the most leading `eq` fields, plus a
//!    range (`gt`, `ge`, `lt`, `le`, `between`) on the next field: one range
//!    scan.
//! 3. Without a usable filter, an index whose fields match `order`: an ordered
//!    scan that stops after `offset + limit` rows (top-k).
//! 4. Otherwise a full scan.
//!
//! A range visits only the keys inside its bounds (`gt` and `lt` exclude the
//! bound itself) and of the kind of value it compares with, since a number
//! never compares with a string in a filter. When the visited keys satisfy the
//! whole filter, the plan is *exact*: rows are not checked again, and a count
//! reads no row at all. Otherwise every visited row is checked against the
//! whole filter, so a plan never changes a result, only how many rows are read.
//!
//! Rows come out already sorted when the chosen keys follow `order`; otherwise
//! the executor sorts them.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::keys::{encode_key, encode_values, prefix_successor};
use super::stmt::{Expr, OrderBy};
use super::store::Table;
use super::value::kind_rank;

/// Numbers up to this magnitude are exact in their `f64` key encoding.
const EXACT_NUMBER_LIMIT: f64 = 9_007_199_254_740_992.0;

/// Which keys a scan visits.
#[derive(Debug, Clone, PartialEq)]
pub enum Access {
    /// One primary key.
    PkPoint(Vec<u8>),
    /// The table, in primary key order, within optional bounds.
    Table {
        /// First key to visit (inclusive).
        lower: Option<Vec<u8>>,
        /// First key past the end (exclusive).
        upper: Option<Vec<u8>>,
    },
    /// Several primary keys, ascending and without repeats (`eq_any`).
    PkPoints(Vec<Vec<u8>>),
    /// An index: keys starting with each of `prefixes` (one leading value
    /// each, ascending and without repeats; `eq_any`).
    IndexPoints {
        /// Position of the index in the table definition.
        index: usize,
        /// Encoded values of the leading field.
        prefixes: Vec<Vec<u8>>,
    },
    /// An index: keys starting with `prefix`, within optional bounds.
    Index {
        /// Position of the index in the table definition.
        index: usize,
        /// Encoded values of the leading `eq` fields.
        prefix: Vec<u8>,
        /// First key to visit (inclusive).
        lower: Option<Vec<u8>>,
        /// First key past the end (exclusive).
        upper: Option<Vec<u8>>,
    },
}

/// A chosen plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Keys to visit.
    pub access: Access,
    /// Visit keys in descending order.
    pub desc: bool,
    /// Visited rows already follow the requested order.
    pub ordered: bool,
    /// Every visited row matches the filter: rows are not checked again.
    pub exact: bool,
}

impl Plan {
    /// Description of the plan (`explain`).
    pub fn explain(&self, table: &Table) -> Value {
        let (access, index) = match &self.access {
            Access::PkPoint(_) | Access::PkPoints(_) => ("primary_key_lookup", None),
            Access::IndexPoints { index, .. } => (
                "index_scan",
                table.indexes.get(*index).map(|i| i.def.name.clone()),
            ),
            Access::Table { lower, upper } if lower.is_some() || upper.is_some() => {
                ("primary_key_range", None)
            }
            Access::Table { .. } => ("full_scan", None),
            Access::Index { index, .. } => (
                "index_scan",
                table.indexes.get(*index).map(|i| i.def.name.clone()),
            ),
        };
        json!({
            "table": table.def.name,
            "access": access,
            "index": index,
            "descending": self.desc,
            "presorted": self.ordered,
            "exact": self.exact,
        })
    }
}

/// One side of a range: the value and whether its keys are excluded (`gt`,
/// `lt`). Only exact values exclude their keys: an integer beyond 2^53 shares
/// its key with neighbours that do satisfy the bound.
#[derive(Clone, Copy)]
struct Bound<'a> {
    value: &'a Value,
    strict: bool,
}

impl<'a> Bound<'a> {
    fn new(value: &'a Value, strict: bool) -> Self {
        Bound {
            value,
            strict: strict && exact_value(value),
        }
    }
}

/// A range constraint on one field.
#[derive(Default, Clone, Copy)]
struct Range<'a> {
    low: Option<Bound<'a>>,
    high: Option<Bound<'a>>,
}

impl Range<'_> {
    /// Whether the bounds compare with a single kind of value, as a filter
    /// with both bounds requires.
    fn one_kind(&self) -> bool {
        match (self.low, self.high) {
            (Some(low), Some(high)) => kind_span(low.value) == kind_span(high.value),
            _ => true,
        }
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::Bool(_) | Value::Number(_) | Value::String(_))
}

/// Whether a key comparison with `value` gives the same answer as the filter.
/// Integers beyond 2^53 share their `f64` key with their neighbours.
fn exact_value(value: &Value) -> bool {
    match value {
        Value::Number(number) => number
            .as_f64()
            .is_some_and(|float| float.abs() < EXACT_NUMBER_LIMIT),
        Value::Bool(_) | Value::String(_) => true,
        _ => false,
    }
}

/// First and past-the-end key tags of the kind of `value`: filters compare
/// booleans with booleans, numbers with numbers, strings with strings.
fn kind_span(value: &Value) -> (u8, u8) {
    match value {
        Value::Bool(_) => (
            kind_rank(&Value::Bool(false)),
            kind_rank(&Value::Bool(true)) + 1,
        ),
        other => (kind_rank(other), kind_rank(other) + 1),
    }
}

/// Top-level conjuncts of a filter.
fn conjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    match expr {
        Expr::And { args } => args.iter().for_each(|arg| conjuncts(arg, out)),
        other => out.push(other),
    }
}

/// The constraints of a filter that keys can serve.
#[derive(Default)]
struct Constraints<'a> {
    eqs: HashMap<&'a str, &'a Value>,
    ranges: HashMap<&'a str, Range<'a>>,
    /// Every conjunct is an `eq` or a range on a scalar, each field has at
    /// most one `eq` and one bound per side, and every value is exact.
    simple: bool,
}

impl<'a> Constraints<'a> {
    fn of(filter: Option<&'a Expr>) -> Self {
        let mut parts = Vec::new();
        if let Some(filter) = filter {
            conjuncts(filter, &mut parts);
        }
        let mut constraints = Constraints {
            simple: true,
            ..Constraints::default()
        };
        for part in parts {
            let usable = match part {
                Expr::Eq { field, value } if is_scalar(value) => {
                    exact_value(value) & constraints.eqs.insert(field, value).is_none()
                }
                Expr::Gt { field, value } if is_scalar(value) => {
                    constraints.low(field, value, true)
                }
                Expr::Ge { field, value } if is_scalar(value) => {
                    constraints.low(field, value, false)
                }
                Expr::Lt { field, value } if is_scalar(value) => {
                    constraints.high(field, value, true)
                }
                Expr::Le { field, value } if is_scalar(value) => {
                    constraints.high(field, value, false)
                }
                Expr::Between { field, low, high } if is_scalar(low) && is_scalar(high) => {
                    constraints.low(field, low, false) & constraints.high(field, high, false)
                }
                _ => false,
            };
            constraints.simple &= usable;
        }
        constraints
    }

    /// Records a lower bound; `false` when the field already had one.
    fn low(&mut self, field: &'a str, value: &'a Value, strict: bool) -> bool {
        let range = self.ranges.entry(field).or_default();
        let first = range.low.is_none();
        range.low = Some(Bound::new(value, strict));
        first && exact_value(value)
    }

    /// Records an upper bound; `false` when the field already had one.
    fn high(&mut self, field: &'a str, value: &'a Value, strict: bool) -> bool {
        let range = self.ranges.entry(field).or_default();
        let first = range.high.is_none();
        range.high = Some(Bound::new(value, strict));
        first && exact_value(value)
    }

    /// Whether keys fixing `leading` fields with `eq`, plus `range` on the
    /// next one, satisfy the whole filter.
    fn served_by(&self, leading: usize, range: Option<&Range<'_>>) -> bool {
        self.simple
            && self.eqs.len() == leading
            && match range {
                Some(range) => self.ranges.len() == 1 && range.one_kind(),
                None => self.ranges.is_empty(),
            }
    }
}

/// Key bounds of `range` after `prefix`: `(first key, first key past the end)`.
fn bounds(prefix: &[u8], range: &Range<'_>) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let key = |value: &Value| {
        let mut key = prefix.to_vec();
        key.extend(encode_key(value));
        key
    };
    let tag = |tag: u8| {
        let mut key = prefix.to_vec();
        key.push(tag);
        key
    };
    // Keys of one value are the value's encoding followed by the primary key
    // (or nothing), so the successor of the encoding is past all of them.
    let lower = match (range.low, range.high) {
        (
            Some(Bound {
                value,
                strict: true,
            }),
            _,
        ) => prefix_successor(&key(value)),
        (Some(Bound { value, .. }), _) => Some(key(value)),
        (None, Some(high)) => Some(tag(kind_span(high.value).0)),
        (None, None) => None,
    };
    let upper = match (range.low, range.high) {
        (
            _,
            Some(Bound {
                value,
                strict: true,
            }),
        ) => Some(key(value)),
        (_, Some(Bound { value, .. })) => prefix_successor(&key(value)),
        (Some(low), None) => Some(tag(kind_span(low.value).1)),
        (None, None) => None,
    };
    (lower, upper)
}

/// The field and the encoded keys of the first top-level `eq_any` of
/// `filter` whose values are all scalars (not `null`), ascending and without
/// repeats; `None` when there is none. A `null` matches no row, and a
/// composite value no scalar field, so such a list is left to a full scan.
fn eq_any_keys(filter: Option<&Expr>) -> Option<(&str, Vec<Vec<u8>>)> {
    let mut parts = Vec::new();
    if let Some(filter) = filter {
        conjuncts(filter, &mut parts);
    }
    parts.into_iter().find_map(|part| match part {
        Expr::EqAny { field, values }
            if !values.is_empty() && values.iter().all(|v| is_scalar(v) && !v.is_null()) =>
        {
            let mut keys: Vec<Vec<u8>> = values.iter().map(encode_key).collect();
            keys.sort_unstable();
            keys.dedup();
            Some((field.as_str(), keys))
        }
        _ => None,
    })
}

/// Chooses how to visit the rows of `table` for `filter` and `order`.
pub fn choose(table: &Table, filter: Option<&Expr>, order: &[OrderBy]) -> Plan {
    let constraints = Constraints::of(filter);
    let eqs = &constraints.eqs;
    let ranges = &constraints.ranges;

    // Sort keys that are not fixed by an `eq`.
    let free_order: Vec<&OrderBy> = order
        .iter()
        .filter(|key| !eqs.contains_key(key.field.as_str()))
        .collect();
    let uniform_desc = free_order.first().map(|key| key.desc);
    let uniform = free_order.iter().all(|key| Some(key.desc) == uniform_desc);
    let desc = uniform_desc.unwrap_or(false);
    let pk = table.def.primary_key.as_str();
    let follows = |fields: &[String]| {
        uniform
            && free_order.len() <= fields.len()
            && free_order
                .iter()
                .zip(fields)
                .all(|(key, field)| key.field == *field)
    };

    // 1. Primary key lookup.
    if let Some(key) = eqs.get(pk) {
        return Plan {
            access: Access::PkPoint(encode_key(key)),
            desc: false,
            ordered: true,
            exact: constraints.served_by(1, None),
        };
    }

    // 2. Best index or primary key range for the filter.
    let mut best: Option<(u32, Plan)> = None;
    for (position, index) in table.indexes.iter().enumerate() {
        let fields = &index.def.fields;
        let leading = fields
            .iter()
            .take_while(|field| eqs.contains_key(field.as_str()))
            .count();
        let range = fields
            .get(leading)
            .and_then(|f| ranges.get(f.as_str()))
            .copied();
        let score = leading as u32 * 2 + u32::from(range.is_some());
        if score == 0 || best.as_ref().is_some_and(|(s, _)| *s >= score) {
            continue;
        }
        let prefix = encode_values(fields[..leading].iter().map(|f| eqs[f.as_str()]));
        let exact = constraints.served_by(leading, range.as_ref());
        let (lower, upper) = bounds(&prefix, &range.unwrap_or_default());
        let ordered = free_order.is_empty() || follows(&fields[leading..]);
        best = Some((
            score,
            Plan {
                access: Access::Index {
                    index: position,
                    lower,
                    upper,
                    prefix,
                },
                desc: ordered && desc,
                ordered,
                exact,
            },
        ));
    }
    if let Some(range) = ranges.get(pk) {
        if best.is_none() {
            let ordered = free_order.is_empty() || follows(&[pk.to_string()]);
            let (lower, upper) = bounds(&[], range);
            best = Some((
                1,
                Plan {
                    access: Access::Table { lower, upper },
                    desc: ordered && desc,
                    ordered,
                    exact: constraints.served_by(0, Some(range)),
                },
            ));
        }
    }
    if let Some((_, plan)) = best {
        return plan;
    }

    // 3. `eq_any` on the primary key or on the leading field of an index:
    // only the named keys, instead of a full scan. Rows are checked again.
    if let Some((field, keys)) = eq_any_keys(filter) {
        if field == pk {
            return Plan {
                access: Access::PkPoints(keys),
                desc: false,
                ordered: free_order.is_empty() || (follows(&[pk.to_string()]) && !desc),
                exact: false,
            };
        }
        let leading = table
            .indexes
            .iter()
            .position(|index| index.def.fields.first().is_some_and(|f| f == field));
        if let Some(position) = leading {
            return Plan {
                access: Access::IndexPoints {
                    index: position,
                    prefixes: keys,
                },
                desc: false,
                ordered: free_order.is_empty(),
                exact: false,
            };
        }
    }

    // 4. An index that provides the order (top-k).
    if !free_order.is_empty() {
        if follows(&[pk.to_string()]) {
            return Plan {
                access: Access::Table {
                    lower: None,
                    upper: None,
                },
                desc,
                ordered: true,
                exact: filter.is_none(),
            };
        }
        for (position, index) in table.indexes.iter().enumerate() {
            if follows(&index.def.fields) {
                return Plan {
                    access: Access::Index {
                        index: position,
                        prefix: Vec::new(),
                        lower: None,
                        upper: None,
                    },
                    desc,
                    ordered: true,
                    exact: filter.is_none(),
                };
            }
        }
    }

    // 5. Full scan.
    Plan {
        access: Access::Table {
            lower: None,
            upper: None,
        },
        desc: false,
        ordered: free_order.is_empty(),
        exact: filter.is_none(),
    }
}

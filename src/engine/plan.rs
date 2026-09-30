//! Rule-based query planner.
//!
//! The planner only chooses *which keys to visit*; every visited row is still
//! checked against the whole filter, so a plan can never change a result, only
//! how many rows are read. Rules, in order:
//!
//! 1. `eq` on the primary key: one lookup.
//! 2. The index (or primary key) with the most leading `eq` fields, plus a
//!    range (`gt`, `ge`, `lt`, `le`, `between`) on the next field: one range
//!    scan.
//! 3. Without a usable filter, an index whose fields match `order`: an ordered
//!    scan that stops after `offset + limit` rows (top-k).
//! 4. Otherwise a full scan.
//!
//! Rows come out already sorted when the chosen keys follow `order`; otherwise
//! the executor sorts them.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::keys::{encode_key, encode_values};
use super::stmt::{Expr, OrderBy};
use super::store::Table;

/// Which keys a scan visits.
#[derive(Debug, Clone, PartialEq)]
pub enum Access {
    /// One primary key.
    PkPoint(Vec<u8>),
    /// The table, in primary key order, within optional bounds.
    Table {
        /// First key to visit (inclusive).
        lower: Option<Vec<u8>>,
        /// Keys whose leading bytes are greater than this bound are past the end.
        upper: Option<Vec<u8>>,
    },
    /// An index: keys starting with `prefix`, within optional bounds.
    Index {
        /// Position of the index in the table definition.
        index: usize,
        /// Encoded values of the leading `eq` fields.
        prefix: Vec<u8>,
        /// First key to visit (inclusive).
        lower: Option<Vec<u8>>,
        /// Keys whose leading bytes are greater than this bound are past the end.
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
}

impl Plan {
    /// Description of the plan (`explain`).
    pub fn explain(&self, table: &Table) -> Value {
        let (access, index) = match &self.access {
            Access::PkPoint(_) => ("primary_key_lookup", None),
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
        })
    }
}

/// A range constraint on one field.
#[derive(Default, Clone, Copy)]
struct Range<'a> {
    low: Option<&'a Value>,
    high: Option<&'a Value>,
}

fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::Bool(_) | Value::Number(_) | Value::String(_))
}

/// Top-level conjuncts of a filter.
fn conjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    match expr {
        Expr::And { args } => args.iter().for_each(|arg| conjuncts(arg, out)),
        other => out.push(other),
    }
}

/// Chooses how to visit the rows of `table` for `filter` and `order`.
pub fn choose(table: &Table, filter: Option<&Expr>, order: &[OrderBy]) -> Plan {
    let mut parts = Vec::new();
    if let Some(filter) = filter {
        conjuncts(filter, &mut parts);
    }
    let mut eqs: HashMap<&str, &Value> = HashMap::new();
    let mut ranges: HashMap<&str, Range<'_>> = HashMap::new();
    for part in &parts {
        match part {
            Expr::Eq { field, value } if is_scalar(value) => {
                eqs.insert(field, value);
            }
            Expr::Gt { field, value } | Expr::Ge { field, value } if is_scalar(value) => {
                ranges.entry(field).or_default().low = Some(value);
            }
            Expr::Lt { field, value } | Expr::Le { field, value } if is_scalar(value) => {
                ranges.entry(field).or_default().high = Some(value);
            }
            Expr::Between { field, low, high } if is_scalar(low) && is_scalar(high) => {
                let range = ranges.entry(field).or_default();
                range.low = Some(low);
                range.high = Some(high);
            }
            _ => {}
        }
    }

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
        let bound = |value: Option<&Value>| {
            value.map(|value| {
                let mut key = prefix.clone();
                key.extend(encode_key(value));
                key
            })
        };
        let range = range.unwrap_or_default();
        let ordered = free_order.is_empty() || follows(&fields[leading..]);
        best = Some((
            score,
            Plan {
                access: Access::Index {
                    index: position,
                    lower: bound(range.low),
                    upper: bound(range.high),
                    prefix,
                },
                desc: ordered && desc,
                ordered,
            },
        ));
    }
    if let Some(range) = ranges.get(pk) {
        if best.is_none() {
            let ordered = free_order.is_empty() || follows(&[pk.to_string()]);
            best = Some((
                1,
                Plan {
                    access: Access::Table {
                        lower: range.low.map(encode_key),
                        upper: range.high.map(encode_key),
                    },
                    desc: ordered && desc,
                    ordered,
                },
            ));
        }
    }
    if let Some((_, plan)) = best {
        return plan;
    }

    // 3. An index that provides the order (top-k).
    if !free_order.is_empty() {
        if follows(&[pk.to_string()]) {
            return Plan {
                access: Access::Table {
                    lower: None,
                    upper: None,
                },
                desc,
                ordered: true,
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
                };
            }
        }
    }

    // 4. Full scan.
    Plan {
        access: Access::Table {
            lower: None,
            upper: None,
        },
        desc: false,
        ordered: free_order.is_empty(),
    }
}

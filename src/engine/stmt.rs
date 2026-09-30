//! Statements and expressions: the intermediate representation shared by the
//! Rust API and the wire protocol.
//!
//! The JSON form of every type is the wire format sent by the Dart SDK. The
//! operator names follow Diesel (`eq`, `ne`, `gt`, `ge`, `lt`, `le`, `eq_any`,
//! `ne_all`, `is_null`, `is_not_null`, `between`, `not_between`, `like`,
//! `ilike`).

use std::ops::Not;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::value::{compare, field, like, sql_eq};

/// A boolean expression over the fields of a row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Expr {
    /// `field = value`.
    Eq {
        /// Field path.
        field: String,
        /// Compared value.
        value: Value,
    },
    /// `field <> value`.
    Ne {
        /// Field path.
        field: String,
        /// Compared value.
        value: Value,
    },
    /// `field > value`.
    Gt {
        /// Field path.
        field: String,
        /// Compared value.
        value: Value,
    },
    /// `field >= value`.
    Ge {
        /// Field path.
        field: String,
        /// Compared value.
        value: Value,
    },
    /// `field < value`.
    Lt {
        /// Field path.
        field: String,
        /// Compared value.
        value: Value,
    },
    /// `field <= value`.
    Le {
        /// Field path.
        field: String,
        /// Compared value.
        value: Value,
    },
    /// `field IN (values)`; false for an empty list.
    EqAny {
        /// Field path.
        field: String,
        /// Candidate values.
        values: Vec<Value>,
    },
    /// `field NOT IN (values)`; true for an empty list unless the field is
    /// `NULL`.
    NeAll {
        /// Field path.
        field: String,
        /// Excluded values.
        values: Vec<Value>,
    },
    /// The field is missing or `NULL`.
    IsNull {
        /// Field path.
        field: String,
    },
    /// The field is present and not `NULL`.
    IsNotNull {
        /// Field path.
        field: String,
    },
    /// `field BETWEEN low AND high` (inclusive).
    Between {
        /// Field path.
        field: String,
        /// Lower bound.
        low: Value,
        /// Upper bound.
        high: Value,
    },
    /// `field NOT BETWEEN low AND high`.
    NotBetween {
        /// Field path.
        field: String,
        /// Lower bound.
        low: Value,
        /// Upper bound.
        high: Value,
    },
    /// `field LIKE pattern` (`%` and `_`, case-sensitive).
    Like {
        /// Field path.
        field: String,
        /// Pattern.
        pattern: String,
    },
    /// `field ILIKE pattern` (ASCII case-insensitive).
    Ilike {
        /// Field path.
        field: String,
        /// Pattern.
        pattern: String,
    },
    /// All arguments hold; true for an empty list.
    And {
        /// Conjuncts.
        args: Vec<Expr>,
    },
    /// Any argument holds; false for an empty list.
    Or {
        /// Disjuncts.
        args: Vec<Expr>,
    },
    /// The argument does not hold.
    Not {
        /// Negated expression.
        arg: Box<Expr>,
    },
}

impl Expr {
    /// Evaluates the expression on `row`.
    ///
    /// Comparisons with a missing field, `NULL`, or a value of another kind
    /// are false; `Not` of such a comparison is true (two-valued negation of
    /// the three-valued result, like SQLite's `NOT (x = NULL)` filtering).
    pub fn matches(&self, row: &Value) -> bool {
        let get = |path: &str| field(row, path).filter(|value| !value.is_null());
        match self {
            Self::Eq { field, value } => get(field).is_some_and(|v| sql_eq(v, value)),
            Self::Ne { field, value } => {
                get(field).is_some_and(|v| compare(v, value).is_some_and(|o| o.is_ne()))
            }
            Self::Gt { field, value } => {
                get(field).is_some_and(|v| compare(v, value).is_some_and(|o| o.is_gt()))
            }
            Self::Ge { field, value } => {
                get(field).is_some_and(|v| compare(v, value).is_some_and(|o| o.is_ge()))
            }
            Self::Lt { field, value } => {
                get(field).is_some_and(|v| compare(v, value).is_some_and(|o| o.is_lt()))
            }
            Self::Le { field, value } => {
                get(field).is_some_and(|v| compare(v, value).is_some_and(|o| o.is_le()))
            }
            Self::EqAny { field, values } => {
                get(field).is_some_and(|v| values.iter().any(|candidate| sql_eq(v, candidate)))
            }
            Self::NeAll { field, values } => get(field).is_some_and(|v| {
                values
                    .iter()
                    .all(|excluded| compare(v, excluded).is_some_and(|o| o.is_ne()))
            }),
            Self::IsNull { field } => get(field).is_none(),
            Self::IsNotNull { field } => get(field).is_some(),
            Self::Between { field, low, high } => get(field).is_some_and(|v| between(v, low, high)),
            Self::NotBetween { field, low, high } => get(field).is_some_and(|v| {
                compare(v, low).is_some() && compare(v, high).is_some() && !between(v, low, high)
            }),
            Self::Like { field, pattern } => get(field)
                .and_then(Value::as_str)
                .is_some_and(|text| like(text, pattern, false)),
            Self::Ilike { field, pattern } => get(field)
                .and_then(Value::as_str)
                .is_some_and(|text| like(text, pattern, true)),
            Self::And { args } => args.iter().all(|arg| arg.matches(row)),
            Self::Or { args } => args.iter().any(|arg| arg.matches(row)),
            Self::Not { arg } => !arg.matches(row),
        }
    }

    /// `self AND other`.
    pub fn and(self, other: Expr) -> Expr {
        match self {
            Self::And { mut args } => {
                args.push(other);
                Self::And { args }
            }
            first => Self::And {
                args: vec![first, other],
            },
        }
    }

    /// `self OR other`.
    pub fn or(self, other: Expr) -> Expr {
        match self {
            Self::Or { mut args } => {
                args.push(other);
                Self::Or { args }
            }
            first => Self::Or {
                args: vec![first, other],
            },
        }
    }
}

impl Not for Expr {
    type Output = Expr;

    /// `NOT self` (`!expr`).
    fn not(self) -> Expr {
        Expr::Not {
            arg: Box::new(self),
        }
    }
}

fn between(value: &Value, low: &Value, high: &Value) -> bool {
    compare(value, low).is_some_and(|o| o.is_ge())
        && compare(value, high).is_some_and(|o| o.is_le())
}

/// One key of an `ORDER BY` clause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderBy {
    /// Field path.
    pub field: String,
    /// Descending order.
    #[serde(default)]
    pub desc: bool,
}

/// A query over one table.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Select {
    /// Table name.
    pub table: String,
    /// Rows must match this expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Expr>,
    /// Sort keys. Without them the order of the rows is unspecified (it is
    /// the order of the keys the planner visits), as in SQL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<OrderBy>,
    /// Maximum number of rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// Rows skipped before the first returned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
}

/// Aggregate functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregate {
    /// Sum of the numeric values; `null` over no value.
    Sum,
    /// Average of the numeric values; `null` over no value.
    Avg,
    /// Smallest value in total order; `null` over no value.
    Min,
    /// Largest value in total order; `null` over no value.
    Max,
}

/// What an insert does when the primary key already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnConflict {
    /// Fail with `DuplicateKey` (Diesel's default).
    #[default]
    Error,
    /// Replace the existing row (`on_conflict(...).do_update()` of the whole row).
    Replace,
    /// Keep the existing row (`on_conflict_do_nothing`).
    Ignore,
}

/// A statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Statement {
    /// Loads rows (`load`, `get_results`, `first` with `limit(1)`).
    Select(Select),
    /// Counts matching rows (`count().get_result`).
    Count {
        /// Table name.
        table: String,
        /// Rows must match this expression.
        #[serde(default)]
        filter: Option<Expr>,
    },
    /// Aggregates one field over the matching rows.
    Aggregate {
        /// Table name.
        table: String,
        /// Rows must match this expression.
        #[serde(default)]
        filter: Option<Expr>,
        /// Function.
        function: Aggregate,
        /// Aggregated field.
        field: String,
    },
    /// Loads one row by primary key (`find`).
    Find {
        /// Table name.
        table: String,
        /// Primary key.
        key: Value,
    },
    /// Inserts rows (`insert_into(...).values(...)`).
    Insert {
        /// Table name.
        table: String,
        /// Rows (JSON objects).
        rows: Vec<Value>,
        /// Behavior on an existing primary key.
        #[serde(default)]
        on_conflict: OnConflict,
    },
    /// Updates the matching rows (`update(...).set(...)`).
    Update {
        /// Table name.
        table: String,
        /// Rows must match this expression; all rows without it.
        #[serde(default)]
        filter: Option<Expr>,
        /// New values by field path (the primary key cannot change).
        set: Map<String, Value>,
        /// Fail unless exactly this many rows are updated.
        #[serde(default)]
        expect: Option<u64>,
    },
    /// Deletes the matching rows (`delete(...)`).
    Delete {
        /// Table name.
        table: String,
        /// Rows must match this expression; all rows without it.
        #[serde(default)]
        filter: Option<Expr>,
        /// Fail unless exactly this many rows are deleted.
        #[serde(default)]
        expect: Option<u64>,
    },
}

impl Statement {
    /// Whether the statement writes.
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Self::Insert { .. } | Self::Update { .. } | Self::Delete { .. }
        )
    }

    /// The table the statement reads or writes.
    pub fn table(&self) -> &str {
        match self {
            Self::Select(select) => &select.table,
            Self::Count { table, .. }
            | Self::Aggregate { table, .. }
            | Self::Find { table, .. }
            | Self::Insert { table, .. }
            | Self::Update { table, .. }
            | Self::Delete { table, .. } => table,
        }
    }
}

/// The result of a statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// Rows, each one the stored JSON text of the row.
    Rows(Vec<Vec<u8>>),
    /// One row or none (`find`).
    Row(Option<Vec<u8>>),
    /// A number of rows (`count`).
    Count(u64),
    /// The value of an aggregate.
    Value(Value),
    /// Rows affected by a write; `rows` holds the inserted rows, with their
    /// generated primary keys (`returning`).
    Affected {
        /// Number of rows written.
        count: u64,
        /// Inserted rows (empty for updates and deletes).
        rows: Vec<Vec<u8>>,
    },
}

impl Output {
    /// Decodes the rows of the output as JSON values.
    pub fn rows(&self) -> Vec<Value> {
        let decode = |bytes: &Vec<u8>| serde_json::from_slice(bytes).unwrap_or(Value::Null);
        match self {
            Self::Rows(rows) | Self::Affected { rows, .. } => rows.iter().map(decode).collect(),
            Self::Row(Some(row)) => vec![decode(row)],
            Self::Row(None) | Self::Count(_) | Self::Value(_) => Vec::new(),
        }
    }
}

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
    /// Paths projected into each output row (empty: whole rows). Nested paths
    /// (`"meta.stars"`) are placed at the same path in the output, `null`
    /// when missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// Drops output rows equal (by key encoding) to an earlier one, keeping
    /// the first. Compares `fields` values, or whole rows without `fields`.
    #[serde(default)]
    pub distinct: bool,
}

/// Aggregate functions available to [`Statement::Group`] (a superset of
/// [`Aggregate`]: it adds `count`, which has no per-row field of its own).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupFunction {
    /// Rows of the group (no `field`), or rows whose value at `field` is not
    /// `null` (with `field`).
    Count,
    /// Sum of the numeric values; `null` over no value.
    Sum,
    /// Average of the numeric values; `null` over no value.
    Avg,
    /// Smallest value in total order; `null` over no value.
    Min,
    /// Largest value in total order; `null` over no value.
    Max,
}

/// One aggregate of a [`Statement::Group`], named by `as`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateSpec {
    /// Function to apply.
    pub function: GroupFunction,
    /// Aggregated field; required for every function but `count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Name of the output field. Non-empty, without `.`, unique among the
    /// aggregates, and different from the first segment of every `by` path.
    #[serde(rename = "as")]
    pub alias: String,
}

/// The `from` table of a [`Statement::Join`], or one of its `joins`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinSource {
    /// Table name.
    pub table: String,
    /// Alias combined rows use for this table; defaults to `table`. Must be
    /// non-empty, without `.`, and unique among the sources of the join.
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

/// The equality condition of one join step: `<combined row>.left = <joined
/// table>.right`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinOn {
    /// Path into the combined row built so far.
    pub left: String,
    /// Path into the joined table's rows.
    pub right: String,
}

/// How a join step treats a combined row with no match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinKind {
    /// Drop the combined row.
    Inner,
    /// Keep it, with `null` for the joined table.
    Left,
}

/// One step of a [`Statement::Join`]: joins `table` onto the combined rows
/// built so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinStep {
    /// Table to join.
    pub table: String,
    /// Alias combined rows use for this table; defaults to `table`.
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Inner or left.
    pub kind: JoinKind,
    /// Equality condition.
    pub on: JoinOn,
}

/// Fields of [`Statement::Group`]. A dedicated struct (like [`Select`]),
/// since `group` alone would push the executor past the function-argument
/// limit clippy enforces, and it doubles as the wire form of the query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupQuery {
    /// Table name.
    pub table: String,
    /// Rows must match this expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Expr>,
    /// Paths whose values define each group (missing is `null`; empty: one
    /// group over all matching rows).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by: Vec<String>,
    /// Aggregates computed per group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aggregates: Vec<AggregateSpec>,
    /// Drops output rows (their fields are the `by` paths and the aggregate
    /// aliases) that do not match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub having: Option<Expr>,
    /// Sort keys over the output rows; without them, ascending by the key
    /// encoding of the `by` values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<OrderBy>,
    /// Maximum number of groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// Groups skipped before the first returned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
}

/// Fields of [`Statement::Join`]. A dedicated struct for the same reason as
/// [`GroupQuery`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinQuery {
    /// The table every combined row starts from.
    pub from: JoinSource,
    /// Joins applied in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joins: Vec<JoinStep>,
    /// Combined rows must match this expression (paths reach into an alias,
    /// e.g. `"u.name"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Expr>,
    /// Sort keys over the combined rows; without them, the order the joins
    /// were built in.
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
    /// Groups matching rows and aggregates each group.
    Group(GroupQuery),
    /// Joins one or more tables by equality and returns the combined rows.
    Join(JoinQuery),
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
        #[serde(default)]
        set: Map<String, Value>,
        /// Numeric deltas by field path, added to the current value (missing
        /// or `null` counts as `0`). Disjoint from `set` and from the primary
        /// key.
        #[serde(default)]
        increment: Map<String, Value>,
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

    /// The table the statement reads or writes. For `join`, the `from` table.
    pub fn table(&self) -> &str {
        match self {
            Self::Select(select) => &select.table,
            Self::Group(query) => &query.table,
            Self::Count { table, .. }
            | Self::Aggregate { table, .. }
            | Self::Find { table, .. }
            | Self::Insert { table, .. }
            | Self::Update { table, .. }
            | Self::Delete { table, .. } => table,
            Self::Join(query) => &query.from.table,
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

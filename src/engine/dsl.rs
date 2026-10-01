//! Diesel-style query builders for Rust.
//!
//! ```no_run
//! use offline_first_core::engine::{col, Db, Query};
//! # fn main() -> Result<(), offline_first_core::engine::EngineError> {
//! # let db = Db::open("/tmp/app.lmdb")?;
//! let top: Vec<serde_json::Value> = Query::table("skills")
//!     .filter(col("enabled").eq(true))
//!     .filter(col("language").eq("rust"))
//!     .order(col("priority").desc())
//!     .then_order_by(col("id").asc())
//!     .limit(20)
//!     .load(&db)?;
//!
//! let changed = Query::update("skills")
//!     .filter(col("id").eq("rust-review"))
//!     .set("enabled", false)
//!     .execute(&db)?;
//! # Ok(())
//! # }
//! ```

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::db::{decode_rows, Db};
use super::error::EngineResult;
use super::stmt::{
    Aggregate, AggregateSpec, Expr, GroupFunction, GroupQuery, JoinKind, JoinOn, JoinQuery,
    JoinSource, JoinStep, OnConflict, OrderBy, Output, Select, Statement,
};
use super::tx::WriteTx;

/// A column (field path) used to build expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Col(String);

/// The column `field` (dot paths reach nested fields: `"address.city"`).
pub fn col(field: impl Into<String>) -> Col {
    Col(field.into())
}

impl Col {
    /// `self = value`.
    pub fn eq(&self, value: impl Into<Value>) -> Expr {
        Expr::Eq {
            field: self.0.clone(),
            value: value.into(),
        }
    }
    /// `self <> value`.
    pub fn ne(&self, value: impl Into<Value>) -> Expr {
        Expr::Ne {
            field: self.0.clone(),
            value: value.into(),
        }
    }
    /// `self > value`.
    pub fn gt(&self, value: impl Into<Value>) -> Expr {
        Expr::Gt {
            field: self.0.clone(),
            value: value.into(),
        }
    }
    /// `self >= value`.
    pub fn ge(&self, value: impl Into<Value>) -> Expr {
        Expr::Ge {
            field: self.0.clone(),
            value: value.into(),
        }
    }
    /// `self < value`.
    pub fn lt(&self, value: impl Into<Value>) -> Expr {
        Expr::Lt {
            field: self.0.clone(),
            value: value.into(),
        }
    }
    /// `self <= value`.
    pub fn le(&self, value: impl Into<Value>) -> Expr {
        Expr::Le {
            field: self.0.clone(),
            value: value.into(),
        }
    }
    /// `self IN (values)`.
    pub fn eq_any<V: Into<Value>>(&self, values: impl IntoIterator<Item = V>) -> Expr {
        Expr::EqAny {
            field: self.0.clone(),
            values: values.into_iter().map(Into::into).collect(),
        }
    }
    /// `self NOT IN (values)`.
    pub fn ne_all<V: Into<Value>>(&self, values: impl IntoIterator<Item = V>) -> Expr {
        Expr::NeAll {
            field: self.0.clone(),
            values: values.into_iter().map(Into::into).collect(),
        }
    }
    /// `self IS NULL` (missing or `null`).
    pub fn is_null(&self) -> Expr {
        Expr::IsNull {
            field: self.0.clone(),
        }
    }
    /// `self IS NOT NULL`.
    pub fn is_not_null(&self) -> Expr {
        Expr::IsNotNull {
            field: self.0.clone(),
        }
    }
    /// `self BETWEEN low AND high`.
    pub fn between(&self, low: impl Into<Value>, high: impl Into<Value>) -> Expr {
        Expr::Between {
            field: self.0.clone(),
            low: low.into(),
            high: high.into(),
        }
    }
    /// `self NOT BETWEEN low AND high`.
    pub fn not_between(&self, low: impl Into<Value>, high: impl Into<Value>) -> Expr {
        Expr::NotBetween {
            field: self.0.clone(),
            low: low.into(),
            high: high.into(),
        }
    }
    /// `self LIKE pattern`.
    pub fn like(&self, pattern: impl Into<String>) -> Expr {
        Expr::Like {
            field: self.0.clone(),
            pattern: pattern.into(),
        }
    }
    /// `self ILIKE pattern`.
    pub fn ilike(&self, pattern: impl Into<String>) -> Expr {
        Expr::Ilike {
            field: self.0.clone(),
            pattern: pattern.into(),
        }
    }
    /// Ascending sort key.
    pub fn asc(&self) -> OrderBy {
        OrderBy {
            field: self.0.clone(),
            desc: false,
        }
    }
    /// Descending sort key.
    pub fn desc(&self) -> OrderBy {
        OrderBy {
            field: self.0.clone(),
            desc: true,
        }
    }
}

/// Entry points of the query builders.
#[derive(Debug, Clone, Copy)]
pub struct Query;

impl Query {
    /// `SELECT * FROM table`.
    pub fn table(table: impl Into<String>) -> Select {
        Select {
            table: table.into(),
            ..Select::default()
        }
    }

    /// `INSERT INTO table VALUES rows`.
    pub fn insert_into(table: impl Into<String>, rows: impl IntoIterator<Item = Value>) -> Insert {
        Insert {
            table: table.into(),
            rows: rows.into_iter().collect(),
            on_conflict: OnConflict::Error,
        }
    }

    /// `UPDATE table SET ...`.
    pub fn update(table: impl Into<String>) -> Update {
        Update {
            table: table.into(),
            filter: None,
            set: Map::new(),
            increment: Map::new(),
            expect: None,
        }
    }

    /// `DELETE FROM table`.
    pub fn delete(table: impl Into<String>) -> Delete {
        Delete {
            table: table.into(),
            filter: None,
            expect: None,
        }
    }

    /// Loads one row by primary key.
    pub fn find(table: impl Into<String>, key: impl Into<Value>) -> Statement {
        Statement::Find {
            table: table.into(),
            key: key.into(),
        }
    }

    /// `SELECT ... FROM table GROUP BY ...`.
    pub fn group(table: impl Into<String>) -> Group {
        Group {
            table: table.into(),
            filter: None,
            by: Vec::new(),
            aggregates: Vec::new(),
            having: None,
            order: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    /// `SELECT ... FROM table JOIN ...`.
    pub fn join(table: impl Into<String>) -> Join {
        Join {
            from: JoinSource {
                table: table.into(),
                alias: None,
            },
            joins: Vec::new(),
            filter: None,
            order: Vec::new(),
            limit: None,
            offset: None,
        }
    }
}

fn and_filter(current: Option<Expr>, next: Expr) -> Option<Expr> {
    Some(match current {
        Some(current) => current.and(next),
        None => next,
    })
}

impl Select {
    /// Adds a condition (`AND` with the previous ones).
    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = and_filter(self.filter.take(), expr);
        self
    }

    /// Adds an alternative (`OR` with the conditions so far).
    pub fn or_filter(mut self, expr: Expr) -> Self {
        self.filter = Some(match self.filter.take() {
            Some(current) => current.or(expr),
            None => expr,
        });
        self
    }

    /// Replaces the sort keys.
    pub fn order(mut self, key: OrderBy) -> Self {
        self.order = vec![key];
        self
    }

    /// Adds a sort key.
    pub fn then_order_by(mut self, key: OrderBy) -> Self {
        self.order.push(key);
        self
    }

    /// Maximum number of rows.
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Rows to skip.
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Projects only these field paths into each output row (nested paths
    /// reach into the row, e.g. `"address.city"`); without it, whole rows.
    pub fn select(mut self, fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Drops output rows equal to an earlier one (by key encoding), keeping
    /// the first.
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    /// Runs the query and deserializes the rows.
    pub fn load<T: DeserializeOwned>(&self, db: &Db) -> EngineResult<Vec<T>> {
        db.load(self)
    }

    /// Runs the query with `limit(1)`.
    pub fn first<T: DeserializeOwned>(&self, db: &Db) -> EngineResult<Option<T>> {
        db.first(self)
    }

    /// Counts the matching rows (limit and offset are ignored).
    pub fn count(&self, db: &Db) -> EngineResult<u64> {
        match db.execute(&Statement::Count {
            table: self.table.clone(),
            filter: self.filter.clone(),
        })? {
            Output::Count(count) => Ok(count),
            _ => Ok(0),
        }
    }

    /// Aggregates `field` over the matching rows.
    pub fn aggregate(
        &self,
        db: &Db,
        function: Aggregate,
        field: impl Into<String>,
    ) -> EngineResult<Value> {
        let statement = Statement::Aggregate {
            table: self.table.clone(),
            filter: self.filter.clone(),
            function,
            field: field.into(),
        };
        match db.execute(&statement)? {
            Output::Value(value) => Ok(value),
            _ => Ok(Value::Null),
        }
    }

    /// Runs the query inside a transaction and deserializes the rows.
    pub fn load_in<T: DeserializeOwned>(&self, tx: &mut WriteTx<'_>) -> EngineResult<Vec<T>> {
        decode_rows(&tx.execute(&Statement::Select(self.clone()))?)
    }
}

impl From<Select> for Statement {
    fn from(select: Select) -> Self {
        Statement::Select(select)
    }
}

/// A `group` query under construction.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    table: String,
    filter: Option<Expr>,
    by: Vec<String>,
    aggregates: Vec<AggregateSpec>,
    having: Option<Expr>,
    order: Vec<OrderBy>,
    limit: Option<u64>,
    offset: Option<u64>,
}

impl Group {
    /// Adds a condition (`AND` with the previous ones), evaluated before
    /// grouping.
    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = and_filter(self.filter.take(), expr);
        self
    }

    /// Adds an alternative (`OR` with the conditions so far).
    pub fn or_filter(mut self, expr: Expr) -> Self {
        self.filter = Some(match self.filter.take() {
            Some(current) => current.or(expr),
            None => expr,
        });
        self
    }

    /// Adds `field` to the grouping key.
    pub fn by(mut self, field: impl Into<String>) -> Self {
        self.by.push(field.into());
        self
    }

    /// Counts the rows of each group, as `alias`.
    pub fn count(mut self, alias: impl Into<String>) -> Self {
        self.aggregates.push(AggregateSpec {
            function: GroupFunction::Count,
            field: None,
            alias: alias.into(),
        });
        self
    }

    /// Counts the rows of each group whose `field` is not `null`, as `alias`.
    pub fn count_of(mut self, field: impl Into<String>, alias: impl Into<String>) -> Self {
        self.aggregates.push(AggregateSpec {
            function: GroupFunction::Count,
            field: Some(field.into()),
            alias: alias.into(),
        });
        self
    }

    /// Sums `field` over each group, as `alias`.
    pub fn sum(self, field: impl Into<String>, alias: impl Into<String>) -> Self {
        self.aggregate(GroupFunction::Sum, field, alias)
    }

    /// Averages `field` over each group, as `alias`.
    pub fn avg(self, field: impl Into<String>, alias: impl Into<String>) -> Self {
        self.aggregate(GroupFunction::Avg, field, alias)
    }

    /// Smallest `field` of each group (total order), as `alias`.
    pub fn min(self, field: impl Into<String>, alias: impl Into<String>) -> Self {
        self.aggregate(GroupFunction::Min, field, alias)
    }

    /// Largest `field` of each group (total order), as `alias`.
    pub fn max(self, field: impl Into<String>, alias: impl Into<String>) -> Self {
        self.aggregate(GroupFunction::Max, field, alias)
    }

    fn aggregate(
        mut self,
        function: GroupFunction,
        field: impl Into<String>,
        alias: impl Into<String>,
    ) -> Self {
        self.aggregates.push(AggregateSpec {
            function,
            field: Some(field.into()),
            alias: alias.into(),
        });
        self
    }

    /// Drops output groups that do not match (`AND` with the previous ones).
    /// Its fields are the `by` paths and the aggregate aliases.
    pub fn having(mut self, expr: Expr) -> Self {
        self.having = Some(match self.having.take() {
            Some(current) => current.and(expr),
            None => expr,
        });
        self
    }

    /// Replaces the sort keys.
    pub fn order(mut self, key: OrderBy) -> Self {
        self.order = vec![key];
        self
    }

    /// Adds a sort key.
    pub fn then_order_by(mut self, key: OrderBy) -> Self {
        self.order.push(key);
        self
    }

    /// Maximum number of groups.
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Groups to skip.
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Runs the query and deserializes the output rows.
    pub fn load<T: DeserializeOwned>(&self, db: &Db) -> EngineResult<Vec<T>> {
        decode_rows(&db.execute(&Statement::from(self.clone()))?)
    }

    /// Runs the query inside a transaction and deserializes the output rows.
    pub fn load_in<T: DeserializeOwned>(&self, tx: &mut WriteTx<'_>) -> EngineResult<Vec<T>> {
        decode_rows(&tx.execute(&Statement::from(self.clone()))?)
    }
}

impl From<Group> for Statement {
    fn from(group: Group) -> Self {
        Statement::Group(GroupQuery {
            table: group.table,
            filter: group.filter,
            by: group.by,
            aggregates: group.aggregates,
            having: group.having,
            order: group.order,
            limit: group.limit,
            offset: group.offset,
        })
    }
}

/// A `join` query under construction.
#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    from: JoinSource,
    joins: Vec<JoinStep>,
    filter: Option<Expr>,
    order: Vec<OrderBy>,
    limit: Option<u64>,
    offset: Option<u64>,
}

impl Join {
    /// Alias of the `from` table in combined rows (defaults to its name).
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.from.alias = Some(alias.into());
        self
    }

    /// Inner-joins `table` (aliased by its name) on `left = right`.
    pub fn inner_join(
        self,
        table: impl Into<String>,
        left: impl Into<String>,
        right: impl Into<String>,
    ) -> Self {
        self.push_join(
            table.into(),
            None,
            JoinKind::Inner,
            left.into(),
            right.into(),
        )
    }

    /// Left-joins `table` (aliased by its name) on `left = right`.
    pub fn left_join(
        self,
        table: impl Into<String>,
        left: impl Into<String>,
        right: impl Into<String>,
    ) -> Self {
        self.push_join(
            table.into(),
            None,
            JoinKind::Left,
            left.into(),
            right.into(),
        )
    }

    /// Inner-joins `table` (aliased `alias`) on `left = right`.
    pub fn inner_join_as(
        self,
        table: impl Into<String>,
        alias: impl Into<String>,
        left: impl Into<String>,
        right: impl Into<String>,
    ) -> Self {
        self.push_join(
            table.into(),
            Some(alias.into()),
            JoinKind::Inner,
            left.into(),
            right.into(),
        )
    }

    /// Left-joins `table` (aliased `alias`) on `left = right`.
    pub fn left_join_as(
        self,
        table: impl Into<String>,
        alias: impl Into<String>,
        left: impl Into<String>,
        right: impl Into<String>,
    ) -> Self {
        self.push_join(
            table.into(),
            Some(alias.into()),
            JoinKind::Left,
            left.into(),
            right.into(),
        )
    }

    fn push_join(
        mut self,
        table: String,
        alias: Option<String>,
        kind: JoinKind,
        left: String,
        right: String,
    ) -> Self {
        self.joins.push(JoinStep {
            table,
            alias,
            kind,
            on: JoinOn { left, right },
        });
        self
    }

    /// Adds a condition over the combined rows (`AND` with the previous
    /// ones).
    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = and_filter(self.filter.take(), expr);
        self
    }

    /// Adds an alternative (`OR` with the conditions so far).
    pub fn or_filter(mut self, expr: Expr) -> Self {
        self.filter = Some(match self.filter.take() {
            Some(current) => current.or(expr),
            None => expr,
        });
        self
    }

    /// Replaces the sort keys (paths reach into an alias, e.g. `"u.name"`).
    pub fn order(mut self, key: OrderBy) -> Self {
        self.order = vec![key];
        self
    }

    /// Adds a sort key.
    pub fn then_order_by(mut self, key: OrderBy) -> Self {
        self.order.push(key);
        self
    }

    /// Maximum number of combined rows.
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Combined rows to skip.
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Runs the query and deserializes the combined rows.
    pub fn load<T: DeserializeOwned>(&self, db: &Db) -> EngineResult<Vec<T>> {
        decode_rows(&db.execute(&Statement::from(self.clone()))?)
    }

    /// Runs the query inside a transaction and deserializes the combined rows.
    pub fn load_in<T: DeserializeOwned>(&self, tx: &mut WriteTx<'_>) -> EngineResult<Vec<T>> {
        decode_rows(&tx.execute(&Statement::from(self.clone()))?)
    }

    /// How the query would run, without running it (see [`Db::explain_join`]).
    pub fn explain(&self, db: &Db) -> EngineResult<Value> {
        db.explain_join(&JoinQuery::from(self.clone()))
    }
}

impl From<Join> for JoinQuery {
    fn from(join: Join) -> Self {
        JoinQuery {
            from: join.from,
            joins: join.joins,
            filter: join.filter,
            order: join.order,
            limit: join.limit,
            offset: join.offset,
        }
    }
}

impl From<Join> for Statement {
    fn from(join: Join) -> Self {
        Statement::Join(JoinQuery::from(join))
    }
}

fn affected(output: Output) -> u64 {
    match output {
        Output::Affected { count, .. } => count,
        _ => 0,
    }
}

/// An insert under construction.
#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    table: String,
    rows: Vec<Value>,
    on_conflict: OnConflict,
}

impl Insert {
    /// Replace existing rows with the same primary key.
    pub fn on_conflict_replace(mut self) -> Self {
        self.on_conflict = OnConflict::Replace;
        self
    }

    /// Keep existing rows with the same primary key.
    pub fn on_conflict_do_nothing(mut self) -> Self {
        self.on_conflict = OnConflict::Ignore;
        self
    }

    /// Runs the insert in its own transaction; returns the inserted rows count.
    pub fn execute(self, db: &Db) -> EngineResult<u64> {
        db.execute(&self.into()).map(affected)
    }

    /// Runs the insert and returns the inserted rows (with generated keys).
    pub fn get_results<T: DeserializeOwned>(self, db: &Db) -> EngineResult<Vec<T>> {
        decode_rows(&db.execute(&self.into())?)
    }

    /// Runs the insert inside a transaction.
    pub fn execute_in(self, tx: &mut WriteTx<'_>) -> EngineResult<u64> {
        tx.execute(&self.into()).map(affected)
    }
}

impl From<Insert> for Statement {
    fn from(insert: Insert) -> Self {
        Statement::Insert {
            table: insert.table,
            rows: insert.rows,
            on_conflict: insert.on_conflict,
        }
    }
}

/// An update under construction.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    table: String,
    filter: Option<Expr>,
    set: Map<String, Value>,
    increment: Map<String, Value>,
    expect: Option<u64>,
}

impl Update {
    /// Restricts the updated rows.
    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = and_filter(self.filter.take(), expr);
        self
    }

    /// Sets `field` to `value`.
    pub fn set(mut self, field: impl Into<String>, value: impl Into<Value>) -> Self {
        self.set.insert(field.into(), value.into());
        self
    }

    /// Adds `delta` to the current value of `field` (missing or `null` counts
    /// as `0`); disjoint from `set` and from the primary key.
    pub fn increment(mut self, field: impl Into<String>, delta: impl Into<Value>) -> Self {
        self.increment.insert(field.into(), delta.into());
        self
    }

    /// Fails (and writes nothing) unless exactly `count` rows are updated.
    pub fn expect_affected_rows(mut self, count: u64) -> Self {
        self.expect = Some(count);
        self
    }

    /// Runs the update in its own transaction; returns the updated rows count.
    pub fn execute(self, db: &Db) -> EngineResult<u64> {
        db.execute(&self.into()).map(affected)
    }

    /// Runs the update inside a transaction.
    pub fn execute_in(self, tx: &mut WriteTx<'_>) -> EngineResult<u64> {
        tx.execute(&self.into()).map(affected)
    }
}

impl From<Update> for Statement {
    fn from(update: Update) -> Self {
        Statement::Update {
            table: update.table,
            filter: update.filter,
            set: update.set,
            increment: update.increment,
            expect: update.expect,
        }
    }
}

/// A delete under construction.
#[derive(Debug, Clone, PartialEq)]
pub struct Delete {
    table: String,
    filter: Option<Expr>,
    expect: Option<u64>,
}

impl Delete {
    /// Restricts the deleted rows.
    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = and_filter(self.filter.take(), expr);
        self
    }

    /// Fails (and deletes nothing) unless exactly `count` rows are deleted.
    pub fn expect_affected_rows(mut self, count: u64) -> Self {
        self.expect = Some(count);
        self
    }

    /// Runs the delete in its own transaction; returns the deleted rows count.
    pub fn execute(self, db: &Db) -> EngineResult<u64> {
        db.execute(&self.into()).map(affected)
    }

    /// Runs the delete inside a transaction.
    pub fn execute_in(self, tx: &mut WriteTx<'_>) -> EngineResult<u64> {
        tx.execute(&self.into()).map(affected)
    }
}

impl From<Delete> for Statement {
    fn from(delete: Delete) -> Self {
        Statement::Delete {
            table: delete.table,
            filter: delete.filter,
            expect: delete.expect,
        }
    }
}

//! The query engine: a Diesel-style database over LMDB (natdb).
//!
//! Tables hold JSON objects keyed by a primary key field; secondary indexes
//! are maintained in the same transaction as their rows. Queries are
//! expressed with the [`Statement`] intermediate representation, built with
//! the Diesel-style builders of [`dsl`] from Rust, or received as JSON from
//! the Dart SDK through [`ofc_execute`](crate::ofc_execute). One planner and
//! one executor serve both.
//!
//! Transactions follow Diesel: [`Db::transaction`] commits when its closure
//! returns `Ok` and rolls back on `Err`; savepoints nest; a write that fails
//! inside a transaction makes it rollback-only.

pub(crate) mod db;
pub mod dsl;
mod error;
pub(crate) mod exec;
pub mod keys;
pub mod plan;
pub mod schema;
pub(crate) mod session;
pub mod stmt;
pub mod store;
pub(crate) mod tx;
pub mod value;

pub use db::{decode_rows, Db};
pub use dsl::{col, Col, Delete, Group, Insert, Join, Query, Update};
pub use error::{EngineError, EngineResult};
pub use schema::{IndexDef, TableDef};
pub use stmt::{
    Aggregate, AggregateSpec, Expr, GroupFunction, GroupQuery, JoinKind, JoinOn, JoinQuery,
    JoinSource, JoinStep, OnConflict, OrderBy, Output, Select, Statement,
};
pub use store::{Durability, OpenOptions, Store};
pub use tx::{ReadTx, WriteTx};

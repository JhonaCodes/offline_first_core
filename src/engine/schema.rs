//! Table definitions.

use serde::{Deserialize, Serialize};

use super::error::{EngineError, EngineResult};

/// Definition of a table: its primary key and secondary indexes.
///
/// Rows are JSON objects. The primary key is one field of the row; it
/// identifies the row and orders a table scan. Every index is maintained in
/// the same transaction as the row it indexes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDef {
    /// Table name: non-empty, without `:`, not starting with `__`.
    pub name: String,
    /// Field holding the primary key.
    pub primary_key: String,
    /// Assign an increasing integer primary key to rows inserted without one.
    #[serde(default)]
    pub auto_increment: bool,
    /// Secondary indexes.
    #[serde(default)]
    pub indexes: Vec<IndexDef>,
}

/// A secondary index over one or more fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDef {
    /// Index name, unique within its table.
    pub name: String,
    /// Indexed fields, in order (dot paths allowed).
    pub fields: Vec<String>,
    /// Reject two rows with equal values in these fields.
    #[serde(default)]
    pub unique: bool,
}

impl TableDef {
    /// A table with `primary_key` and no index.
    pub fn new(name: impl Into<String>, primary_key: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            primary_key: primary_key.into(),
            auto_increment: false,
            indexes: Vec::new(),
        }
    }

    /// Adds a non-unique index.
    pub fn index(mut self, name: impl Into<String>, fields: &[&str]) -> Self {
        self.indexes.push(IndexDef {
            name: name.into(),
            fields: fields.iter().map(|f| (*f).to_string()).collect(),
            unique: false,
        });
        self
    }

    /// Adds a unique index.
    pub fn unique_index(mut self, name: impl Into<String>, fields: &[&str]) -> Self {
        self.indexes.push(IndexDef {
            name: name.into(),
            fields: fields.iter().map(|f| (*f).to_string()).collect(),
            unique: true,
        });
        self
    }

    /// Generates integer primary keys for rows inserted without one.
    pub fn auto_increment(mut self) -> Self {
        self.auto_increment = true;
        self
    }

    /// Checks names and fields.
    pub fn validate(&self) -> EngineResult<()> {
        validate_name("table", &self.name)?;
        if self.primary_key.is_empty() {
            return Err(EngineError::InvalidSchema(format!(
                "table `{}` has an empty primary key",
                self.name
            )));
        }
        let mut names = std::collections::HashSet::new();
        for index in &self.indexes {
            validate_name("index", &index.name)?;
            if !names.insert(index.name.as_str()) {
                return Err(EngineError::InvalidSchema(format!(
                    "index `{}` is defined twice on `{}`",
                    index.name, self.name
                )));
            }
            if index.fields.is_empty() || index.fields.iter().any(String::is_empty) {
                return Err(EngineError::InvalidSchema(format!(
                    "index `{}` of `{}` needs non-empty fields",
                    index.name, self.name
                )));
            }
        }
        Ok(())
    }

    /// Name of the LMDB database holding the rows.
    pub(crate) fn db_name(&self) -> String {
        format!("t:{}", self.name)
    }

    /// Name of the LMDB database holding `index`.
    pub(crate) fn index_db_name(&self, index: &IndexDef) -> String {
        format!("i:{}:{}", self.name, index.name)
    }
}

fn validate_name(kind: &str, name: &str) -> EngineResult<()> {
    if name.is_empty() || name.contains(':') || name.starts_with("__") {
        return Err(EngineError::InvalidSchema(format!(
            "{kind} name `{name}` must be non-empty, without `:`, and not start with `__`"
        )));
    }
    Ok(())
}

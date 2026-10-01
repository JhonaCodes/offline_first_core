//! Table definitions.

use serde::{Deserialize, Serialize};

use super::error::{EngineError, EngineResult};

/// Definition of a table: its primary key and secondary indexes.
///
/// Rows are JSON objects. The primary key is one field of the row; it
/// identifies the row and orders a table scan. Every index is maintained in
/// the same transaction as the row it indexes.
///
/// Build it with [`TableDef::new`] and the builder methods; fields may be
/// added in any release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
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
    /// The remote this table synchronizes with (RFC §13.2): every effective
    /// write records a change in the same transaction. `None`: local only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<String>,
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
            sync: None,
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

    /// Synchronizes the table with `remote`, a logical name (not a URL): see
    /// [`sync`](super::sync). Once synchronized, a table stays so.
    pub fn sync_with(mut self, remote: impl Into<String>) -> Self {
        self.sync = Some(remote.into());
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
        if self.sync.as_deref() == Some("") {
            return Err(EngineError::InvalidSchema(format!(
                "table `{}` synchronizes with an empty remote",
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

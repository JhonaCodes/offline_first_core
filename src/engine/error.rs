//! Errors of the query engine, with stable codes for the wire protocol.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// An error returned by the query engine.
///
/// Every variant has a stable [`code`](EngineError::code), which the wire
/// protocol sends to Dart; messages may change between versions, codes do not.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The statement names a table that is not defined.
    #[error("table `{0}` is not defined")]
    TableNotFound(String),
    /// A table definition is invalid.
    #[error("invalid table definition: {0}")]
    InvalidSchema(String),
    /// A table is already defined with an incompatible definition.
    #[error("table `{table}` is already defined differently: {detail}")]
    SchemaMismatch {
        /// Table name.
        table: String,
        /// What differs.
        detail: String,
    },
    /// An insert used a primary key that already exists.
    #[error("table `{table}` already has a row with primary key {key}")]
    DuplicateKey {
        /// Table name.
        table: String,
        /// The primary key, as JSON.
        key: String,
    },
    /// A write would store two rows with the same value in a unique index.
    #[error("unique index `{index}` of table `{table}` already contains {value}")]
    UniqueViolation {
        /// Table name.
        table: String,
        /// Index name.
        index: String,
        /// The duplicated value, as JSON.
        value: String,
    },
    /// A row has no primary key and the table does not generate one.
    #[error("row of table `{table}` has no primary key `{field}`")]
    MissingPrimaryKey {
        /// Table name.
        table: String,
        /// Primary key field.
        field: String,
    },
    /// The request is malformed.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// A write did not affect the number of rows its precondition expected.
    #[error("expected {expected} affected rows, the statement affected {actual}")]
    AffectedRowsMismatch {
        /// Rows the statement was expected to affect.
        expected: u64,
        /// Rows it would have affected.
        actual: u64,
    },
    /// A stored row cannot be decoded.
    #[error("stored row of table `{table}` is corrupted: {detail}")]
    CorruptRecord {
        /// Table name.
        table: String,
        /// Decoding error.
        detail: String,
    },
    /// An indexed value does not fit in an LMDB key.
    #[error("key of {size} bytes exceeds the LMDB maximum of {max} bytes")]
    KeyTooLarge {
        /// Encoded size.
        size: usize,
        /// Maximum key size of the environment.
        max: usize,
    },
    /// The memory map is full; the operation can be retried after it grows.
    #[error("the database map is full")]
    MapFull,
    /// The database was written by LMDB 0.9 (offline_first_core 0.5 or older).
    #[error("the database was written by LMDB 0.9 and must be migrated")]
    LegacyFormat,
    /// Any other storage error.
    #[error("storage error: {0}")]
    Storage(natdb::Error),
    /// A file system error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// The transaction was already committed, rolled back or expired.
    #[error("the transaction is closed")]
    TransactionClosed,
    /// A write failed earlier in this transaction scope: it can only be
    /// rolled back.
    #[error("a write failed in this transaction, which can only be rolled back")]
    TransactionAborted,
    /// The transaction was rolled back after staying idle for too long.
    #[error("the transaction was rolled back after {0:?} without activity")]
    TransactionExpired(Duration),
    /// A write statement was sent to a read-only transaction.
    #[error("read-only transactions cannot write")]
    ReadOnlyTransaction,
    /// A savepoint was released or rolled back while none was open.
    #[error("no savepoint is open")]
    NoSavepoint,
    /// The transaction was committed while a savepoint was still open.
    #[error("a savepoint is still open")]
    SavepointOpen,
    /// A second write transaction was started on a thread that already runs
    /// one on the same database (it would wait for itself forever).
    #[error("a write transaction is already running on this thread for this database")]
    Reentrancy,
    /// The session does not exist or belongs to another database.
    #[error("unknown transaction {0}")]
    UnknownTransaction(u64),
    /// The directory is already open in this process by another `Store`
    /// (LMDB forbids opening one environment twice in a process).
    #[error("the database `{}` is already open in this process", .0.display())]
    AlreadyOpen(PathBuf),
    /// The database was closed (for example after a failed reset).
    #[error("the database is closed")]
    Closed,
    /// The request uses a protocol version this library does not speak.
    #[error("unsupported protocol version {0}")]
    UnsupportedProtocol(u64),
}

impl EngineError {
    /// Stable identifier of the error, used by the wire protocol.
    pub fn code(&self) -> &'static str {
        match self {
            Self::TableNotFound(_) => "TableNotFound",
            Self::InvalidSchema(_) => "InvalidSchema",
            Self::SchemaMismatch { .. } => "SchemaMismatch",
            Self::DuplicateKey { .. } => "DuplicateKey",
            Self::UniqueViolation { .. } => "UniqueViolation",
            Self::MissingPrimaryKey { .. } => "MissingPrimaryKey",
            Self::InvalidRequest(_) => "InvalidRequest",
            Self::AffectedRowsMismatch { .. } => "AffectedRowsMismatch",
            Self::CorruptRecord { .. } => "CorruptRecord",
            Self::KeyTooLarge { .. } => "KeyTooLarge",
            Self::MapFull => "MapFull",
            Self::LegacyFormat => "LegacyFormat",
            Self::Storage(_) => "StorageError",
            Self::Io(_) => "IoError",
            Self::TransactionClosed => "TransactionClosed",
            Self::TransactionAborted => "TransactionAborted",
            Self::TransactionExpired(_) => "TransactionExpired",
            Self::ReadOnlyTransaction => "ReadOnlyTransaction",
            Self::NoSavepoint => "NoSavepoint",
            Self::SavepointOpen => "SavepointOpen",
            Self::Reentrancy => "TransactionReentrancy",
            Self::UnknownTransaction(_) => "UnknownTransaction",
            Self::AlreadyOpen(_) => "AlreadyOpen",
            Self::Closed => "Closed",
            Self::UnsupportedProtocol(_) => "UnsupportedProtocol",
        }
    }

    /// Whether the operation failed only because the memory map is full.
    pub fn is_map_full(&self) -> bool {
        matches!(self, Self::MapFull)
    }
}

impl From<natdb::Error> for EngineError {
    fn from(error: natdb::Error) -> Self {
        match error {
            natdb::Error::MapFull => Self::MapFull,
            natdb::Error::LegacyFormat => Self::LegacyFormat,
            natdb::Error::BadValSize => Self::InvalidRequest(
                "a key is empty or exceeds the LMDB maximum key size".to_string(),
            ),
            other => Self::Storage(other),
        }
    }
}

/// Result type of the query engine.
pub type EngineResult<T> = Result<T, EngineError>;

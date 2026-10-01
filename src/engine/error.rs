//! Errors of the query engine, with stable codes for the wire protocol.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// An error returned by the query engine.
///
/// Every variant has a stable [`code`](EngineError::code), which the wire
/// protocol sends to Dart; messages may change between versions, codes do not.
/// New variants may appear in any release: match with a wildcard arm.
#[derive(Debug, Error)]
#[non_exhaustive]
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
    /// A sync operation was refused (see [`SyncError`]).
    #[error(transparent)]
    Sync(#[from] SyncError),
}

/// Why a sync operation was refused. Nothing changed when one is returned.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SyncError {
    /// The table (or the remote) is not synchronized.
    #[error("`{0}` is not synchronized")]
    NotTracked(String),
    /// The mutation is not known to this database.
    #[error("unknown mutation `{0}`")]
    UnknownMutation(String),
    /// An acknowledgement names a mutation, but not its entity, revision or
    /// remote, or the mutation was never claimed.
    #[error("the acknowledgement of `{mutation_id}` does not match it: {detail}")]
    AcknowledgementMismatch {
        /// The mutation.
        mutation_id: String,
        /// What differs.
        detail: String,
    },
    /// The page was read after another checkpoint than the current one.
    #[error("the checkpoint is {actual}, the page expected {expected}")]
    StaleCheckpoint {
        /// The checkpoint the page expected, as JSON.
        expected: String,
        /// The current checkpoint, as JSON.
        actual: String,
    },
    /// The conflict does not exist (or was resolved).
    #[error("unknown conflict `{0}`")]
    ConflictNotFound(String),
    /// The row changed since the conflict was read.
    #[error("the row version is {actual}, the resolution expected {expected}")]
    RowVersionMismatch {
        /// The version the resolution expected.
        expected: u64,
        /// The current version.
        actual: u64,
    },
    /// A change of the row is being sent: acknowledge or release it first.
    #[error("mutation `{0}` is being sent; acknowledge or release it first")]
    MutationInFlight(String),
    /// The key was deleted and the deletion is not settled yet: it cannot be
    /// created again meanwhile.
    #[error("row {key} of `{table}` has a deletion that is not settled yet")]
    TombstonePending {
        /// Table name.
        table: String,
        /// The primary key, as JSON.
        key: String,
    },
}

impl SyncError {
    /// Stable identifier of the error, used by the wire protocol.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotTracked(_) => "SyncNotTracked",
            Self::UnknownMutation(_) => "UnknownMutation",
            Self::AcknowledgementMismatch { .. } => "AcknowledgementMismatch",
            Self::StaleCheckpoint { .. } => "StaleCheckpoint",
            Self::ConflictNotFound(_) => "ConflictNotFound",
            Self::RowVersionMismatch { .. } => "RowVersionMismatch",
            Self::MutationInFlight(_) => "MutationInFlight",
            Self::TombstonePending { .. } => "TombstonePending",
        }
    }
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
            Self::Sync(error) => error.code(),
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

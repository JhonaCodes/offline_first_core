use std::fmt::{Display, Formatter};

use redb::{CommitError, Error as RedbError, StorageError, TableError, TransactionError};
use serde::{Deserialize, Serialize};
use serde_json::Error as SerdeError;

#[derive(Debug, Serialize, Deserialize)]
pub enum AppResponse {
    DatabaseError(String),
    SerializationError(String),
    NotFound(String),
    ValidationError(String),
    BadRequest(String),
    Ok(String),
}

impl Display for AppResponse {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AppResponse::DatabaseError(msg) => write!(f, "Database error: {}", msg),
            AppResponse::SerializationError(msg) => write!(f, "Serialization error: {}", msg),
            AppResponse::NotFound(msg) => write!(f, "Not found: {}", msg),
            AppResponse::ValidationError(msg) => write!(f, "Validation error: {}", msg),
            AppResponse::BadRequest(msg) => write!(f, "Bad Request: {}", msg),
            AppResponse::Ok(msg) => write!(f, "Ok: {}", msg),
        }
    }
}

impl From<RedbError> for AppResponse {
    fn from(err: RedbError) -> Self {
        match err {
            RedbError::TableDoesNotExist(name) =>
                AppResponse::NotFound(format!("Table '{}' not found", name)),
            RedbError::Corrupted(msg) =>
                AppResponse::DatabaseError(format!("Database is corrupted: {}", msg)),
            RedbError::Io(io_err) =>
                AppResponse::DatabaseError(format!("IO error: {}", io_err)),
            _ => AppResponse::DatabaseError(format!("Database error: {:?}", err)),
        }
    }
}

impl From<SerdeError> for AppResponse {
    fn from(err: SerdeError) -> Self {
        AppResponse::SerializationError(format!("JSON serialization error: {}", err))
    }
}

impl From<TransactionError> for AppResponse {
    fn from(err: TransactionError) -> Self {
        AppResponse::DatabaseError(format!("Transaction error: {:?}", err))
    }
}

impl From<TableError> for AppResponse {
    fn from(err: TableError) -> Self {
        AppResponse::DatabaseError(format!("Table operation error: {:?}", err))
    }
}

impl From<StorageError> for AppResponse {
    fn from(err: StorageError) -> Self {
        AppResponse::DatabaseError(format!("Error de almacenamiento en la base de datos: {:?}", err))
    }
}

impl From<CommitError> for AppResponse {
    fn from(err: CommitError) -> Self {
        AppResponse::DatabaseError(format!("Error al confirmar la transacción: {:?}", err))
    }
}

impl AppResponse {
    pub fn success(msg: impl Into<String>) -> Self {
        AppResponse::Ok(msg.into())
    }
}
use std::fmt::{Display, Formatter};

use lmdb::Error as LmdbError;
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

impl From<LmdbError> for AppResponse {
    fn from(err: LmdbError) -> Self {
        match err {
            LmdbError::KeyExist =>
                AppResponse::BadRequest("Key already exists".to_string()),
            LmdbError::NotFound =>
                AppResponse::NotFound("Record not found".to_string()),
            LmdbError::Corrupted =>
                AppResponse::DatabaseError("Database is corrupted".to_string()),
            LmdbError::Panic =>
                AppResponse::DatabaseError("Database panic occurred".to_string()),
            LmdbError::MapFull =>
                AppResponse::DatabaseError("Database map is full".to_string()),
            LmdbError::DbsFull =>
                AppResponse::DatabaseError("Maximum databases reached".to_string()),
            LmdbError::ReadersFull =>
                AppResponse::DatabaseError("Maximum readers reached".to_string()),
            LmdbError::TxnFull =>
                AppResponse::DatabaseError("Transaction is full".to_string()),
            LmdbError::CursorFull =>
                AppResponse::DatabaseError("Cursor stack is full".to_string()),
            LmdbError::PageFull =>
                AppResponse::DatabaseError("Page is full".to_string()),
            LmdbError::MapResized =>
                AppResponse::DatabaseError("Database map was resized".to_string()),
            LmdbError::Incompatible =>
                AppResponse::DatabaseError("Database is incompatible".to_string()),
            LmdbError::BadRslot =>
                AppResponse::DatabaseError("Bad reader locktable slot".to_string()),
            LmdbError::BadTxn =>
                AppResponse::DatabaseError("Invalid transaction".to_string()),
            LmdbError::BadValSize =>
                AppResponse::DatabaseError("Value size is invalid".to_string()),
            LmdbError::BadDbi =>
                AppResponse::DatabaseError("Invalid database handle".to_string()),
            LmdbError::Other(code) =>
                AppResponse::DatabaseError(format!("LMDB error code: {}", code)),
            LmdbError::PageNotFound =>
                AppResponse::DatabaseError("Page not found".to_string()),
            LmdbError::VersionMismatch =>
                AppResponse::DatabaseError("Version mismatch".to_string()),
            LmdbError::Invalid =>
                AppResponse::DatabaseError("Invalid LMDB file".to_string()),
            LmdbError::TlsFull =>
                AppResponse::DatabaseError("TLS keys full".to_string()),
        }
    }
}

impl From<SerdeError> for AppResponse {
    fn from(err: SerdeError) -> Self {
        AppResponse::SerializationError(format!("JSON serialization error: {}", err))
    }
}


impl AppResponse {
    pub fn success(msg: impl Into<String>) -> Self {
        AppResponse::Ok(msg.into())
    }
}
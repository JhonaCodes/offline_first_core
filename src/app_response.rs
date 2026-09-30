//! Application response types and error handling.
//!
//! This module provides a unified response system for all database operations
//! and FFI interactions. It defines the [`AppResponse`] enum that encapsulates
//! both successful results and various error conditions, with automatic conversion
//! from LMDB errors and JSON serialization errors.
//!
//! This module is private, so the examples below are marked `ignore`: doctests
//! are compiled as external crates and can only reach the public API.

use std::fmt::{Display, Formatter};

use natdb::Error as MdbError;
use serde::{Deserialize, Serialize};
use serde_json::Error as SerdeError;

use crate::local_db_state::DbError;
use crate::registry::ResetError;

/// Unified response type for all database operations and FFI interactions.
///
/// `AppResponse` provides a consistent way to handle both successful operations
/// and various error conditions that can occur during database operations,
/// JSON serialization, or FFI interactions. All variants are serializable
/// to JSON for easy transmission across language boundaries.
///
/// # Variants
///
/// - [`DatabaseError`] - Database-related errors (LMDB operations)
/// - [`SerializationError`] - JSON serialization/deserialization errors
/// - [`NotFound`] - Resource not found errors
/// - [`ValidationError`] - Input validation errors
/// - [`BadRequest`] - Invalid request parameters
/// - [`Ok`] - Successful operation with result data
///
/// # JSON Format
///
/// When serialized to JSON, each variant produces a structured response:
///
/// ```json
/// // Success response
/// {"Ok": "operation completed successfully"}
///
/// // Error responses
/// {"DatabaseError": "LMDB error: database is corrupted"}
/// {"NotFound": "No record found with id: user_123"}
/// {"BadRequest": "Null pointer passed to function"}
/// ```
///
/// # Examples
///
/// ## Creating responses
///
/// ```ignore
/// use offline_first_core::app_response::AppResponse;
///
/// // Success response
/// let success = AppResponse::Ok("Data saved successfully".to_string());
///
/// // Error responses
/// let not_found = AppResponse::NotFound("User not found".to_string());
/// let bad_request = AppResponse::BadRequest("Invalid ID format".to_string());
/// ```
///
/// ## JSON serialization
///
/// ```ignore
/// use offline_first_core::app_response::AppResponse;
/// use serde_json;
///
/// let response = AppResponse::Ok("Success".to_string());
/// let json = serde_json::to_string(&response)?;
/// println!("JSON: {}", json); // {"Ok":"Success"}
/// # Ok::<(), serde_json::Error>(())
/// ```
///
/// ## Error conversion
///
/// ```ignore
/// use offline_first_core::app_response::AppResponse;
/// use natdb::Error as MdbError;
///
/// // Automatic conversion from LMDB errors
/// let lmdb_error = MdbError::NotFound;
/// let app_response: AppResponse = lmdb_error.into();
///
/// match app_response {
///     AppResponse::NotFound(msg) => println!("Not found: {}", msg),
///     _ => println!("Other error"),
/// }
/// ```
#[derive(Debug, Serialize, Deserialize)]
pub enum AppResponse {
    /// Database operation error.
    ///
    /// This variant represents errors that occur during LMDB database operations,
    /// such as connection failures, transaction errors, or corruption issues.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use offline_first_core::app_response::AppResponse;
    ///
    /// let error = AppResponse::DatabaseError(
    ///     "Failed to open database environment".to_string()
    /// );
    /// ```
    DatabaseError(String),

    /// JSON serialization or deserialization error.
    ///
    /// This variant represents errors that occur during JSON processing,
    /// including malformed JSON, type mismatches, or serialization failures.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use offline_first_core::app_response::AppResponse;
    ///
    /// let error = AppResponse::SerializationError(
    ///     "Invalid JSON: expected object, found array".to_string()
    /// );
    /// ```
    SerializationError(String),

    /// Resource not found error.
    ///
    /// This variant indicates that a requested resource (typically a database record)
    /// could not be found. It's commonly used when querying for records by ID.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use offline_first_core::app_response::AppResponse;
    ///
    /// let error = AppResponse::NotFound(
    ///     "No user found with ID: user_12345".to_string()
    /// );
    /// ```
    NotFound(String),

    /// Input validation error.
    ///
    /// This variant represents errors in input data validation, such as
    /// format violations, constraint failures, or business rule violations.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use offline_first_core::app_response::AppResponse;
    ///
    /// let error = AppResponse::ValidationError(
    ///     "Email address format is invalid".to_string()
    /// );
    /// ```
    ValidationError(String),

    /// Bad request error.
    ///
    /// This variant represents client errors such as null pointers,
    /// invalid parameters, or malformed requests in FFI operations.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use offline_first_core::app_response::AppResponse;
    ///
    /// let error = AppResponse::BadRequest(
    ///     "Null pointer passed to create_db function".to_string()
    /// );
    /// ```
    BadRequest(String),

    /// Successful operation response.
    ///
    /// This variant represents successful operations and contains the
    /// result data or a success message. The data is typically JSON-serialized
    /// when returning from database operations.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use offline_first_core::app_response::AppResponse;
    ///
    /// let success = AppResponse::Ok(
    ///     r#"{"id":"user_123","name":"John Doe"}"#.to_string()
    /// );
    /// ```
    Ok(String),
}

impl Display for AppResponse {
    /// Formats the response for display purposes.
    ///
    /// This implementation provides human-readable string representations
    /// of all response variants, suitable for logging or debugging.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use offline_first_core::app_response::AppResponse;
    ///
    /// let error = AppResponse::DatabaseError("Connection failed".to_string());
    /// println!("{}", error); // "Database error: Connection failed"
    ///
    /// let success = AppResponse::Ok("Data saved".to_string());
    /// println!("{}", success); // "Ok: Data saved"
    /// ```
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AppResponse::DatabaseError(msg) => write!(f, "Database error: {msg}"),
            AppResponse::SerializationError(msg) => write!(f, "Serialization error: {msg}"),
            AppResponse::NotFound(msg) => write!(f, "Not found: {msg}"),
            AppResponse::ValidationError(msg) => write!(f, "Validation error: {msg}"),
            AppResponse::BadRequest(msg) => write!(f, "Bad Request: {msg}"),
            AppResponse::Ok(msg) => write!(f, "Ok: {msg}"),
        }
    }
}

impl From<DbError> for AppResponse {
    /// Converts storage-layer errors into application responses.
    ///
    /// This is the FFI boundary mapping. Engine errors keep the variants and
    /// messages produced by 0.5.0 (see the [`MdbError`] mapping below) so the
    /// JSON contract seen by the Dart plugin does not change.
    ///
    /// # Error Mapping
    ///
    /// - [`DbError::Storage`] → see the [`MdbError`] mapping
    /// - [`DbError::Serialization`] → `AppResponse::SerializationError`
    /// - I/O, corrupt stored records and a closed database → `AppResponse::DatabaseError`
    fn from(err: DbError) -> Self {
        match err {
            DbError::Storage(error) => AppResponse::from(error),
            DbError::Serialization(error) => AppResponse::from(error),
            DbError::Engine(error) => AppResponse::DatabaseError(error.to_string()),
            DbError::Io(_)
            | DbError::Utf8 { .. }
            | DbError::Deserialization { .. }
            | DbError::Closed => AppResponse::DatabaseError(err.to_string()),
        }
    }
}

impl From<ResetError> for AppResponse {
    /// Converts a failed `reset_database` into an application response.
    ///
    /// Storage errors keep the message produced by 0.5.0
    /// (`Error resetting database: <debug form of the error>`).
    fn from(err: ResetError) -> Self {
        let detail = match &err {
            ResetError::Db(error) => format!("{error:?}"),
            ResetError::AlreadyOpen(_) => err.to_string(),
        };
        AppResponse::DatabaseError(format!("Error resetting database: {detail}"))
    }
}

impl From<MdbError> for AppResponse {
    /// Converts LMDB error codes into application responses.
    ///
    /// # Error Mapping
    ///
    /// - `MdbError::NotFound` → `AppResponse::NotFound`
    /// - `MdbError::KeyExist` → `AppResponse::BadRequest`
    /// - Database corruption errors → `AppResponse::DatabaseError`
    /// - Resource limit errors → `AppResponse::DatabaseError`
    /// - Other errors → `AppResponse::DatabaseError`
    fn from(err: MdbError) -> Self {
        match err {
            MdbError::KeyExist => AppResponse::BadRequest("Key already exists".to_string()),
            MdbError::NotFound => AppResponse::NotFound("Record not found".to_string()),
            MdbError::Corrupted => AppResponse::DatabaseError("Database is corrupted".to_string()),
            MdbError::Panic => AppResponse::DatabaseError("Database panic occurred".to_string()),
            MdbError::MapFull => AppResponse::DatabaseError("Database map is full".to_string()),
            MdbError::DbsFull => {
                AppResponse::DatabaseError("Maximum databases reached".to_string())
            }
            MdbError::ReadersFull => {
                AppResponse::DatabaseError("Maximum readers reached".to_string())
            }
            MdbError::TxnFull => AppResponse::DatabaseError("Transaction is full".to_string()),
            MdbError::CursorFull => AppResponse::DatabaseError("Cursor stack is full".to_string()),
            MdbError::PageFull => AppResponse::DatabaseError("Page is full".to_string()),
            MdbError::MapResized => {
                AppResponse::DatabaseError("Database map was resized".to_string())
            }
            MdbError::Incompatible => {
                AppResponse::DatabaseError("Database is incompatible".to_string())
            }
            MdbError::BadRslot => {
                AppResponse::DatabaseError("Bad reader locktable slot".to_string())
            }
            MdbError::BadTxn => AppResponse::DatabaseError("Invalid transaction".to_string()),
            MdbError::BadValSize => AppResponse::DatabaseError("Value size is invalid".to_string()),
            MdbError::BadDbi => AppResponse::DatabaseError("Invalid database handle".to_string()),
            MdbError::Other(code) => AppResponse::DatabaseError(format!("LMDB error code: {code}")),
            MdbError::PageNotFound => AppResponse::DatabaseError("Page not found".to_string()),
            MdbError::VersionMismatch => AppResponse::DatabaseError("Version mismatch".to_string()),
            MdbError::Invalid => AppResponse::DatabaseError("Invalid LMDB file".to_string()),
            MdbError::TlsFull => AppResponse::DatabaseError("TLS keys full".to_string()),
            MdbError::LegacyFormat => AppResponse::DatabaseError(
                "Database was written by LMDB 0.9 and must be migrated".to_string(),
            ),
        }
    }
}

impl From<SerdeError> for AppResponse {
    /// Converts JSON serialization errors into application responses.
    ///
    /// This implementation enables automatic conversion from serde_json errors
    /// into [`AppResponse::SerializationError`] variants, providing consistent
    /// error handling for JSON operations.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use offline_first_core::app_response::AppResponse;
    /// use serde_json;
    ///
    /// // Automatic conversion during JSON parsing
    /// fn parse_json(data: &str) -> Result<serde_json::Value, AppResponse> {
    ///     let value = serde_json::from_str(data)?; // SerdeError becomes AppResponse
    ///     Ok(value)
    /// }
    ///
    /// // Usage
    /// match parse_json("{invalid json") {
    ///     Ok(value) => println!("Parsed: {:?}", value),
    ///     Err(AppResponse::SerializationError(msg)) => println!("JSON error: {}", msg),
    ///     Err(other) => println!("Other error: {}", other),
    /// }
    /// ```
    fn from(err: SerdeError) -> Self {
        AppResponse::SerializationError(format!("JSON serialization error: {err}"))
    }
}

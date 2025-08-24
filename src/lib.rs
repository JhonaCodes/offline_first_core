//! # Offline First Core
//!
//! A high-performance local storage library designed for FFI (Foreign Function Interface) 
//! integration with Flutter and other cross-platform applications. Built on LMDB 
//! (Lightning Memory-Mapped Database) for maximum stability and hot restart support.
//!
//! ## Features
//!
//! - **LMDB-based storage**: Battle-tested database engine used by OpenLDAP and Bitcoin Core
//! - **FFI-optimized**: Designed specifically for Flutter integration with hot restart support
//! - **ACID compliance**: Full transaction support with data integrity guarantees
//! - **Zero-copy reads**: Memory-mapped access for optimal performance
//! - **Safe error handling**: No `unwrap()` calls in production code
//!
//! ## Quick Start
//!
//! ```no_run
//! use offline_first_core::{create_db, post_data as push_data, get_by_id};
//! use std::ffi::CString;
//!
//! // Create database instance
//! let db_name = CString::new("my_database").unwrap();
//! let db_state = create_db(db_name.as_ptr());
//!
//! // Insert data
//! let json_data = CString::new(r#"{"id":"1","hash":"abc","data":{"key":"value"}}"#).unwrap();
//! let result = push_data(db_state, json_data.as_ptr());
//! ```
//!
//! ## FFI Functions
//!
//! This library exposes C-compatible functions for cross-language integration:
//!
//! - [`create_db`] - Initialize database instance
//! - [`post_data`] - Insert new records (alias: `push_data`)
//! - [`get_by_id`] - Retrieve records by ID
//! - [`get_all`] - Retrieve all records
//! - [`update_data`] - Update existing records
//! - [`delete_by_id`] - Delete records by ID
//! - [`clear_all_records`] - Clear all database contents
//! - [`reset_database`] - Reset database to clean state
//! - [`close_database`] - Explicit connection cleanup

pub mod local_db_model;
pub mod local_db_state;
mod test;
mod app_response;

use crate::local_db_model::LocalDbModel;
use crate::local_db_state::AppDbState;

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use log::{info, warn};
use std::path::Path;

use crate::app_response::AppResponse;

/// Creates a new database instance with the specified name.
///
/// This function initializes an LMDB environment and creates the main database
/// for storing key-value pairs. The database will be created as a directory
/// with `.lmdb` extension.
///
/// # Parameters
///
/// * `name` - A null-terminated C string containing the database name
///
/// # Returns
///
/// Returns a pointer to the [`AppDbState`] instance on success, or a null pointer on failure.
/// The caller is responsible for managing the returned pointer's lifetime.
///
/// # Safety
///
/// This function is unsafe because it:
/// - Dereferences a raw pointer without validation
/// - Returns a raw pointer that must be properly managed
/// - Requires the input string to be valid UTF-8
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::create_db;
///
/// let name = CString::new("test_database").unwrap();
/// let db_state = create_db(name.as_ptr());
/// 
/// if !db_state.is_null() {
///     // Database created successfully
/// }
/// ```
///
/// # Errors
///
/// Returns null pointer if:
/// - Input name pointer is null
/// - Input string contains invalid UTF-8
/// - Database initialization fails
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn create_db(name: *const c_char) -> *mut AppDbState {
    if name.is_null() {
        warn!("Null name pointer passed to create_db");
        return std::ptr::null_mut();
    }

    let name_str = match unsafe { CStr::from_ptr(name).to_str() } {
        Ok(s) => s,
        Err(e) => {
            warn!("Invalid UTF-8 in name parameter: {e}");
            return std::ptr::null_mut();
        }
    };

    // Use a more appropriate directory path for cross-platform compatibility
    let db_path = format!("{name_str}");
    let lmdb_dir = format!("{db_path}.lmdb");

    info!("Attempting to create/open database at: {}", lmdb_dir);

    if Path::new(&lmdb_dir).exists() {
        info!("Database already exists; attempting clean close before reopen");
        match AppDbState::init(db_path.clone()) {
            Ok(mut existing) => {
                if let Err(e) = existing.close_database() {
                    warn!("Failed to close existing LMDB environment: {e:?}");
                } else {
                    info!("Existing LMDB environment closed successfully");
                }
            }
            Err(e) => {
                warn!("Could not open existing environment for closing: {e:?}");
            }
        }
    } else {
        info!("Creating new database at: {}", lmdb_dir);
    }

    let state = AppDbState::init(db_path);
    
    match state {
        Ok(response) => {
            info!("✅ Database initialized successfully");
            Box::into_raw(Box::new(response))
        },
        Err(e) => {
            warn!("❌ Failed to initialize database: {:?}", e);
            warn!("LMDB error details: {}", e);
            warn!("Attempted path: {}", lmdb_dir);
            warn!("Current working directory might not be writable");
            std::ptr::null_mut()
        },
    }
}

/// Inserts a new record into the database.
///
/// This function deserializes the provided JSON string into a [`LocalDbModel`]
/// and stores it in the database using the model's ID as the key.
///
/// # Parameters
///
/// * `state` - Pointer to the database state instance
/// * `json_ptr` - Null-terminated C string containing JSON data
///
/// # Returns
///
/// Returns a JSON-formatted C string containing the operation result.
/// The returned string must be freed by the caller.
///
/// # Safety
///
/// This function is unsafe because it dereferences raw pointers.
/// Both parameters must be valid pointers to their respective types.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, push_data};
///
/// let db_name = CString::new("test_db").unwrap();
/// let db_state = create_db(db_name.as_ptr());
///
/// let json = CString::new(r#"{"id":"1","hash":"abc123","data":{"name":"test"}}"#).unwrap();
/// let result = push_data(db_state, json.as_ptr());
/// ```
///
/// # JSON Format
///
/// Expected JSON structure:
/// ```json
/// {
///   "id": "unique_identifier",
///   "hash": "content_hash", 
///   "data": { /* arbitrary JSON data */ }
/// }
/// ```
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn push_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    let state = match unsafe { state.as_ref() } {
        Some(s) => s,
        None => {
            let error = AppResponse::BadRequest("Null state pointer".to_string());
            return response_to_c_string(&error);
        }
    };

    let json_str = match c_ptr_to_string(json_ptr, "JSON") {
        Ok(response) => response,
        Err(err) => return err
    };

    let model: LocalDbModel = match serde_json::from_str(&json_str) {
        Ok(m) => m,
        Err(e) => {
            let error = AppResponse::SerializationError(format!("Invalid JSON: {e}"));
            return response_to_c_string(&error);
        }
    };
    
    match state.post(model) {
        Ok(result_model) => {
            match serde_json::to_string(&result_model) {
                Ok(json) => {
                    let success = AppResponse::Ok(json);
                    response_to_c_string(&success)
                },
                Err(e) => {
                    let error = AppResponse::SerializationError(format!("Failed to serialize result: {e}"));
                    response_to_c_string(&error)
                }
            }
        },
        Err(e) => response_to_c_string(&e)
    }
}

/// Inserts a new record into the database (HTTP-style naming).
///
/// Alias for [`push_data`]. Provided to align with endpoint semantics.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn post_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    push_data(state, json_ptr)
}

/// Retrieves a record from the database by its ID.
///
/// # Parameters
///
/// * `state` - Pointer to the database state instance
/// * `id` - Null-terminated C string containing the record ID
///
/// # Returns
///
/// Returns a JSON-formatted C string containing the record data if found,
/// or an error response if not found or on failure.
///
/// # Safety
///
/// Both parameters must be valid pointers. The ID string must be valid UTF-8.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, get_by_id};
///
/// let db_name = CString::new("test_db").unwrap();
/// let db_state = create_db(db_name.as_ptr());
///
/// let id = CString::new("record_1").unwrap();
/// let result = get_by_id(db_state, id.as_ptr());
/// ```
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn get_by_id(state: *mut AppDbState, id: *const c_char) -> *const c_char {
    if state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to get_by_id".to_string());
        return response_to_c_string(&error);
    }

    if id.is_null() {
        let error = AppResponse::BadRequest("Null id pointer passed to get_by_id".to_string());
        return response_to_c_string(&error);
    }

    let state = unsafe { &*state };

    let id_str = match c_ptr_to_string(id, "id") {
        Ok(json) => json,
        Err(error_ptr) => return error_ptr,
    };

    match state.get_by_id(&id_str) {
        Ok(Some(model)) => {
            match serde_json::to_string(&model) {
                Ok(json) => {
                    let success = AppResponse::Ok(json);
                    response_to_c_string(&success)
                },
                Err(e) => {
                    let error = AppResponse::SerializationError(format!("Error serializing to JSON: {e:?}"));
                    response_to_c_string(&error)
                }
            }
        },
        Ok(None) => {
            let error = AppResponse::NotFound(format!("No model found with id: {id_str}"));
            response_to_c_string(&error)
        },
        Err(e) => {
            let error = AppResponse::from(e);
            response_to_c_string(&error)
        }
    }
}

/// Retrieves all records from the database.
///
/// # Parameters
///
/// * `state` - Pointer to the database state instance
///
/// # Returns
///
/// Returns a JSON-formatted C string containing an array of all records,
/// or an error response on failure.
///
/// # Safety
///
/// The state parameter must be a valid pointer to an [`AppDbState`] instance.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, get_all};
///
/// let db_name = CString::new("test_db").unwrap();
/// let db_state = create_db(db_name.as_ptr());
///
/// let all_records = get_all(db_state);
/// ```
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn get_all(state: *mut AppDbState) -> *const c_char {
    if state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to get_all".to_string());
        return response_to_c_string(&error);
    }

    let state = unsafe { &*state };

    match state.get() {
        Ok(models) => {
            match serde_json::to_string(&models) {
                Ok(json) => {
                    let success = AppResponse::Ok(json);
                    response_to_c_string(&success)
                },
                Err(e) => {
                    let error = AppResponse::SerializationError(format!("Error serializing models: {e:?}"));
                    response_to_c_string(&error)
                }
            }
        },
        Err(e) => {
            let error = AppResponse::from(e);
            response_to_c_string(&error)
        }
    }
}

/// Updates an existing record in the database.
///
/// The record is identified by the ID field in the provided JSON data.
/// If no record with that ID exists, the operation returns an error.
///
/// # Parameters
///
/// * `state` - Pointer to the database state instance
/// * `json_ptr` - Null-terminated C string containing updated JSON data
///
/// # Returns
///
/// Returns a JSON-formatted C string containing the updated record on success,
/// or an error response if the record doesn't exist or on failure.
///
/// # Safety
///
/// Both parameters must be valid pointers.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, update_data};
///
/// let db_name = CString::new("test_db").unwrap();
/// let db_state = create_db(db_name.as_ptr());
///
/// let json = CString::new(r#"{"id":"1","hash":"new_hash","data":{"updated":true}}"#).unwrap();
/// let result = update_data(db_state, json.as_ptr());
/// ```
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn update_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    if state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to update_data".to_string());
        return response_to_c_string(&error);
    }

    if json_ptr.is_null() {
        let error = AppResponse::BadRequest("Null JSON pointer passed to update_data".to_string());
        return response_to_c_string(&error);
    }

    let json_str = match c_ptr_to_string(json_ptr, "JSON") {
        Ok(json) => json,
        Err(error_ptr) => return error_ptr,
    };

    let model: LocalDbModel = match serde_json::from_str(&json_str) {
        Ok(m) => m,
        Err(e) => {
            let error = AppResponse::SerializationError(format!("Error deserializing JSON: {e:?}"));
            return response_to_c_string(&error);
        }
    };

    let state = unsafe { &*state };

    match state.put(model) {
        Ok(Some(updated_model)) => {
            match serde_json::to_string(&updated_model) {
                Ok(json) => {
                    let success = AppResponse::Ok(json);
                    response_to_c_string(&success)
                },
                Err(e) => {
                    let error = AppResponse::SerializationError(format!("Error serializing updated model: {e:?}"));
                    response_to_c_string(&error)
                }
            }
        },
        Ok(None) => {
            let error = AppResponse::NotFound("Model not found for update".to_string());
            response_to_c_string(&error)
        },
        Err(e) => {
            let error = AppResponse::from(e);
            response_to_c_string(&error)
        }
    }
}

/// Updates an existing record (HTTP-style naming).
///
/// Alias for [`update_data`]. Provided to align with endpoint semantics.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn put_data(state: *mut AppDbState, json_ptr: *const c_char) -> *const c_char {
    update_data(state, json_ptr)
}

/// Deletes a record from the database by its ID.
///
/// # Parameters
///
/// * `db_state` - Pointer to the database state instance
/// * `id` - Null-terminated C string containing the record ID to delete
///
/// # Returns
///
/// Returns a JSON-formatted C string indicating success or failure.
/// Success response includes confirmation of deletion.
///
/// # Safety
///
/// Both parameters must be valid pointers.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, delete_by_id};
///
/// let db_name = CString::new("test_db").unwrap();
/// let db_state = create_db(db_name.as_ptr());
///
/// let id = CString::new("record_to_delete").unwrap();
/// let result = delete_by_id(db_state, id.as_ptr());
/// ```
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn delete_by_id(db_state: *mut AppDbState, id: *const c_char) -> *const c_char {
    if db_state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to delete_by_id".to_string());
        return response_to_c_string(&error);
    }

    if id.is_null() {
        let error = AppResponse::BadRequest("Null id pointer passed to delete_by_id".to_string());
        return response_to_c_string(&error);
    }

    let id_str = match c_ptr_to_string(id, "id") {
        Ok(id) => id,
        Err(error_ptr) => return error_ptr,
    };

    let db_state = unsafe { &mut *db_state };

    match db_state.delete_by_id(&id_str) {
        Ok(true) => {
            let success = AppResponse::Ok("Record deleted successfully".to_string());
            response_to_c_string(&success)
        },
        Ok(false) => {
            let not_found = AppResponse::NotFound(format!("No record found with id: {id_str}"));
            response_to_c_string(&not_found)
        },
        Err(e) => {
            let error = AppResponse::from(e);
            response_to_c_string(&error)
        }
    }
}

/// Clears all records from the database.
///
/// This operation removes all records while maintaining the database structure.
/// The database remains operational after this call.
///
/// # Parameters
///
/// * `db_state` - Pointer to the database state instance
///
/// # Returns
///
/// Returns a JSON-formatted C string indicating the number of records cleared
/// or an error response on failure.
///
/// # Safety
///
/// The db_state parameter must be a valid pointer.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, clear_all_records};
///
/// let db_name = CString::new("test_db").unwrap();
/// let db_state = create_db(db_name.as_ptr());
///
/// let result = clear_all_records(db_state);
/// ```
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn clear_all_records(db_state: *mut AppDbState) -> *const c_char {
    if db_state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to clear_all_records".to_string());
        return response_to_c_string(&error);
    }

    let db_state = unsafe { &*db_state };

    match db_state.clear_all_records() {
        Ok(_) => {
            let success = AppResponse::Ok("All records cleared successfully".to_string());
            response_to_c_string(&success)
        },
        Err(e) => {
            let error = AppResponse::from(e);
            response_to_c_string(&error)
        }
    }
}

/// Resets the database to a clean state with a new name.
///
/// This operation:
/// 1. Closes the current database connection
/// 2. Removes the existing database directory
/// 3. Creates a new database with the specified name
///
/// # Parameters
///
/// * `db_state` - Pointer to the database state instance
/// * `name_ptr` - Null-terminated C string containing the new database name
///
/// # Returns
///
/// Returns a JSON-formatted C string indicating success or failure.
///
/// # Safety
///
/// Both parameters must be valid pointers.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, reset_database};
///
/// let db_name = CString::new("test_db").unwrap();
/// let db_state = create_db(db_name.as_ptr());
///
/// let new_name = CString::new("reset_db").unwrap();
/// let result = reset_database(db_state, new_name.as_ptr());
/// ```
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn reset_database(db_state: *mut AppDbState, name_ptr: *const c_char) -> *const c_char {
    if db_state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to reset_database".to_string());
        return response_to_c_string(&error);
    }

    if name_ptr.is_null() {
        let error = AppResponse::BadRequest("Null name pointer passed to reset_database".to_string());
        return response_to_c_string(&error);
    }

    let name = match c_ptr_to_string(name_ptr, "name") {
        Ok(name) => name,
        Err(error_ptr) => return error_ptr,
    };

    let db_state = unsafe { &mut *db_state };

    match db_state.reset_database(&name) {
        Ok(_) => {
            let success = AppResponse::Ok(format!("Database '{name}' was reset successfully"));
            response_to_c_string(&success)
        },
        Err(e) => {
            let error = AppResponse::DatabaseError(format!("Error resetting database: {e:?}"));
            response_to_c_string(&error)
        }
    }
}

/// Explicitly closes the database connection.
///
/// This function provides explicit connection management, which is particularly
/// useful for Flutter hot restart scenarios where resources need to be cleaned up
/// before reconnecting.
///
/// # Parameters
///
/// * `db_state` - Pointer to the database state instance
///
/// # Returns
///
/// Returns a JSON-formatted C string indicating success or failure.
///
/// # Safety
///
/// The db_state parameter must be a valid pointer.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, close_database};
///
/// let db_name = CString::new("test_db").unwrap();
/// let db_state = create_db(db_name.as_ptr());
///
/// // Before hot restart or application shutdown
/// let result = close_database(db_state);
/// ```
///
/// # Notes
///
/// In LMDB, connections are automatically closed when the environment is dropped.
/// This function serves as an explicit indicator that the connection should no longer be used.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn close_database(db_state: *mut AppDbState) -> *const c_char {
    if db_state.is_null() {
        let error = AppResponse::BadRequest("Null state pointer passed to close_database".to_string());
        return response_to_c_string(&error);
    }

    let db_state = unsafe { &mut *db_state };

    match db_state.close_database() {
        Ok(_) => {
            let success = AppResponse::Ok("Database connection closed successfully".to_string());
            response_to_c_string(&success)
        },
        Err(e) => {
            let error = AppResponse::from(e);
            response_to_c_string(&error)
        }
    }
}

/// Converts an [`AppResponse`] to a C-compatible string.
///
/// This internal helper function serializes the response to JSON format
/// and converts it to a C string that can be returned to FFI callers.
///
/// # Parameters
///
/// * `response` - Reference to the response to convert
///
/// # Returns
///
/// Returns a pointer to a null-terminated C string containing the JSON response.
/// The caller is responsible for freeing this memory.
///
/// # Safety
///
/// Returns a null pointer if serialization or C string creation fails.
fn response_to_c_string(response: &AppResponse) -> *const c_char {
    let json = match serde_json::to_string(response) {
        Ok(j) => j,
        Err(e) => {
            warn!("Error serializing response: {e}");
            return std::ptr::null();
        }
    };

    match CString::new(json) {
        Ok(c_str) => c_str.into_raw(),
        Err(e) => {
            warn!("Error creating CString: {e}");
            std::ptr::null()
        }
    }
}

/// Converts a C string pointer to a Rust String with comprehensive error handling.
///
/// This internal helper function safely converts C string pointers to Rust strings,
/// handling all possible error conditions including null pointers and invalid UTF-8.
///
/// # Parameters
///
/// * `ptr` - Pointer to the C string
/// * `field_name` - Name of the field for descriptive error messages
///
/// # Returns
///
/// * `Ok(String)` - If conversion was successful
/// * `Err(*const c_char)` - Pointer to error message in C format if conversion failed
///
/// # Safety
///
/// This function safely handles null pointers and invalid UTF-8 sequences.
fn c_ptr_to_string(ptr: *const c_char, field_name: &str) -> Result<String, *const c_char> {
    if ptr.is_null() {
        let error = AppResponse::BadRequest(format!("Null {field_name} pointer"));
        return Err(response_to_c_string(&error));
    }

    match unsafe { CStr::from_ptr(ptr).to_str() } {
        Ok(s) => Ok(s.to_string()),
        Err(e) => {
            let error = AppResponse::BadRequest(format!("Invalid UTF-8 in {field_name}: {e}"));
            Err(response_to_c_string(&error))
        }
    }
}
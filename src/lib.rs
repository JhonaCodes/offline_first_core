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
//! use offline_first_core::{close_database, create_db, ofc_free_string, push_data};
//! use std::ffi::CString;
//!
//! // Create database instance
//! let db_name = CString::new("/absolute/path/to/my_database").unwrap();
//! let db = unsafe { create_db(db_name.as_ptr()) };
//!
//! // Insert data, then release the response string
//! let json_data = CString::new(r#"{"id":"1","hash":"abc","data":{"key":"value"}}"#).unwrap();
//! let result = unsafe { push_data(db, json_data.as_ptr()) };
//! unsafe { ofc_free_string(result.cast_mut()) };
//!
//! // Release the handle
//! let result = unsafe { close_database(db) };
//! unsafe { ofc_free_string(result.cast_mut()) };
//! ```
//!
//! ## FFI Functions
//!
//! This library exposes C-compatible functions for cross-language integration:
//!
//! - [`create_db`] - Open a database and return a handle to it
//! - [`push_data`] - Insert new records
//! - [`get_by_id`] - Retrieve records by ID
//! - [`get_all`] - Retrieve all records
//! - [`update_data`] - Update existing records
//! - [`delete_by_id`] - Delete records by ID
//! - [`clear_all_records`] - Clear all database contents
//! - [`reset_database`] - Reset database to clean state
//! - [`close_database`] - Release a handle
//! - [`ofc_free_string`] - Release a response string
//!
//! ## Contract of the C ABI
//!
//! - **Responses**: every function except [`create_db`] and
//!   [`ofc_free_string`] returns a JSON envelope `{"<Variant>": "<payload>"}`
//!   (`Ok`, `NotFound`, `DatabaseError`, `SerializationError`,
//!   `ValidationError` or `BadRequest`). The string is owned by the caller and
//!   must be released with [`ofc_free_string`], exactly once; it must not be
//!   released with the C `free`.
//! - **Handles**: [`create_db`] returns an opaque [`DbHandle`] pointer. Each
//!   call returns its own handle, and handles opened on the same path share a
//!   single database, so opening a path twice (several isolates, a Flutter hot
//!   restart that lost its pointer) works and sees the same data. A handle may
//!   be used from several threads at once. [`close_database`] releases a
//!   handle; the database closes with its last handle.
//! - **Paths** are used exactly as given, with `.lmdb` appended. Relative
//!   paths resolve against the working directory of the process, so callers
//!   should pass absolute paths.
//! - **Panics** never cross the boundary: an internal panic is logged and
//!   answered with `{"DatabaseError": "internal panic in <function>: <message>"}`
//!   (a null handle for [`create_db`]). This relies on `panic = "unwind"`,
//!   which the release profile sets.
//! - **Pointer arguments** of the entry points are not checked beyond null,
//!   so every entry point is an `unsafe extern "C" fn`: passing anything else
//!   than what its `# Safety` section allows is undefined behavior. `unsafe`
//!   is not part of the C symbol, so C and Dart callers are unaffected.
//!
//! ## Naming of new symbols
//!
//! New exported symbols use the `ofc_` prefix (for *offline first core*), as
//! [`ofc_free_string`] does. On iOS the library is linked statically into the
//! app, where every exported symbol shares the process's global namespace
//! (Dart looks them up with `DynamicLibrary.process()`), so generic names such
//! as `free_string` could collide with other libraries. The nine original
//! symbols keep their names for compatibility.
//!
//! ## Cargo features
//!
//! - `fault-injection` (off by default, **tests only**): exports
//!   `ofc_fault_injection_arm`, which makes the next entry point called on the
//!   same thread panic inside its guard, to test the panic containment. It
//!   must never be enabled in a shipped build.

mod app_response;
mod boundary;
pub mod engine;
#[cfg(feature = "fault-injection")]
mod fault_injection;
mod handle;
pub mod local_db_model;
pub mod local_db_state;
mod registry;
pub mod wire;

use std::ffi::{c_char, CString};
use std::path::Path;
use std::ptr;

use log::{info, warn};

use crate::app_response::AppResponse;
use crate::boundary::Call;
use crate::engine::OpenOptions;
use crate::local_db_model::LocalDbModel;
use crate::local_db_state::AppDbState;

#[cfg(feature = "fault-injection")]
pub use crate::fault_injection::ofc_fault_injection_arm;
pub use crate::handle::DbHandle;

/// Opens the database stored at `<name>.lmdb`, creating it if needed, and
/// returns a new handle to it.
///
/// The database is an LMDB environment stored as a directory. If the path is
/// already open in this process, the new handle shares that database (it
/// sees the same data); otherwise the database is opened.
///
/// # Parameters
///
/// * `name` - A null-terminated UTF-8 C string with the database path, without
///   the `.lmdb` extension. It is used exactly as given: pass an absolute
///   path, since a relative one resolves against the working directory.
///
/// # Returns
///
/// A new [`DbHandle`] pointer on success, or null on failure. The handle must
/// be released with [`close_database`].
///
/// # Safety
///
/// `name` must be null or point to a NUL-terminated string that stays valid
/// for the duration of the call.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::create_db;
///
/// let name = CString::new("/absolute/path/test_database").unwrap();
/// let db = unsafe { create_db(name.as_ptr()) };
///
/// if !db.is_null() {
///     // Database created successfully
/// }
/// ```
///
/// # Errors
///
/// Returns null if:
/// - `name` is null or not valid UTF-8
/// - the database cannot be opened
/// - an internal panic occurs
#[no_mangle]
pub unsafe extern "C" fn create_db(name: *const c_char) -> *mut DbHandle {
    let call = Call::new("create_db");
    call.guard(
        |_| ptr::null_mut(),
        || {
            let name = match call.string_arg(name, "name") {
                Ok(name) => name,
                Err(response) => {
                    warn!("{response}");
                    return ptr::null_mut();
                }
            };

            match DbHandle::open(&name) {
                Ok(handle) => {
                    info!("Database initialized at {name}");
                    Box::into_raw(Box::new(handle))
                }
                Err(e) => {
                    warn!("Failed to initialize database at {name}: {e}");
                    ptr::null_mut()
                }
            }
        },
    )
}

/// Inserts a new record into the database.
///
/// This function deserializes the provided JSON string into a [`LocalDbModel`]
/// and stores it in the database using the model's ID as the key.
///
/// # Parameters
///
/// * `state` - Handle returned by [`create_db`]
/// * `json_ptr` - Null-terminated C string containing JSON data
///
/// # Returns
///
/// A JSON envelope with the stored record, to be released with
/// [`ofc_free_string`].
///
/// # Safety
///
/// `state` must be null or a handle returned by [`create_db`] that has not
/// been closed; `json_ptr` must be null or a NUL-terminated string. Both must
/// stay valid for the duration of the call.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, push_data};
///
/// let db_name = CString::new("/absolute/path/test_db").unwrap();
/// let db = unsafe { create_db(db_name.as_ptr()) };
///
/// let json = CString::new(r#"{"id":"1","hash":"abc123","data":{"name":"test"}}"#).unwrap();
/// let result = unsafe { push_data(db, json.as_ptr()) };
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
pub unsafe extern "C" fn push_data(state: *mut DbHandle, json_ptr: *const c_char) -> *const c_char {
    Call::new("push_data").respond(|call| {
        let handle = call.handle(state)?;
        let json = call.string_arg(json_ptr, "JSON")?;
        let model: LocalDbModel = serde_json::from_str(&json)
            .map_err(|e| AppResponse::SerializationError(format!("Invalid JSON: {e}")))?;

        let stored = handle.legacy(|db| db.push(model.clone()))?;
        serde_json::to_string(&stored).map_err(|e| {
            AppResponse::SerializationError(format!("Failed to serialize result: {e}"))
        })
    })
}

/// Retrieves a record from the database by its ID.
///
/// # Parameters
///
/// * `state` - Handle returned by [`create_db`]
/// * `id` - Null-terminated C string containing the record ID
///
/// # Returns
///
/// A JSON envelope with the record, or `NotFound`, to be released with
/// [`ofc_free_string`].
///
/// # Safety
///
/// `state` must be null or a handle returned by [`create_db`] that has not
/// been closed; `id` must be null or a NUL-terminated string. Both must stay
/// valid for the duration of the call.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, get_by_id};
///
/// let db_name = CString::new("/absolute/path/test_db").unwrap();
/// let db = unsafe { create_db(db_name.as_ptr()) };
///
/// let id = CString::new("record_1").unwrap();
/// let result = unsafe { get_by_id(db, id.as_ptr()) };
/// ```
#[no_mangle]
pub unsafe extern "C" fn get_by_id(state: *mut DbHandle, id: *const c_char) -> *const c_char {
    Call::new("get_by_id").respond(|call| {
        let handle = call.handle(state)?;
        let id = call.string_arg(id, "id")?;

        let model = handle
            .db()
            .get_by_id(&id)?
            .ok_or_else(|| AppResponse::NotFound(format!("No model found with id: {id}")))?;
        serde_json::to_string(&model).map_err(|e| {
            AppResponse::SerializationError(format!("Error serializing to JSON: {e:?}"))
        })
    })
}

/// Retrieves all records from the database.
///
/// # Parameters
///
/// * `state` - Handle returned by [`create_db`]
///
/// # Returns
///
/// A JSON envelope with an array of every record, to be released with
/// [`ofc_free_string`].
///
/// # Safety
///
/// `state` must be null or a handle returned by [`create_db`] that has not
/// been closed.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, get_all};
///
/// let db_name = CString::new("/absolute/path/test_db").unwrap();
/// let db = unsafe { create_db(db_name.as_ptr()) };
///
/// let all_records = unsafe { get_all(db) };
/// ```
#[no_mangle]
pub unsafe extern "C" fn get_all(state: *mut DbHandle) -> *const c_char {
    Call::new("get_all").respond(|call| {
        let models = call.handle(state)?.db().get()?;
        serde_json::to_string(&models).map_err(|e| {
            AppResponse::SerializationError(format!("Error serializing models: {e:?}"))
        })
    })
}

/// Updates an existing record in the database.
///
/// The record is identified by the ID field in the provided JSON data.
/// If no record with that ID exists, the operation returns `NotFound`.
///
/// # Parameters
///
/// * `state` - Handle returned by [`create_db`]
/// * `json_ptr` - Null-terminated C string containing updated JSON data
///
/// # Returns
///
/// A JSON envelope with the updated record, to be released with
/// [`ofc_free_string`].
///
/// # Safety
///
/// `state` must be null or a handle returned by [`create_db`] that has not
/// been closed; `json_ptr` must be null or a NUL-terminated string. Both must
/// stay valid for the duration of the call.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, update_data};
///
/// let db_name = CString::new("/absolute/path/test_db").unwrap();
/// let db = unsafe { create_db(db_name.as_ptr()) };
///
/// let json = CString::new(r#"{"id":"1","hash":"new_hash","data":{"updated":true}}"#).unwrap();
/// let result = unsafe { update_data(db, json.as_ptr()) };
/// ```
#[no_mangle]
pub unsafe extern "C" fn update_data(
    state: *mut DbHandle,
    json_ptr: *const c_char,
) -> *const c_char {
    Call::new("update_data").respond(|call| {
        let handle = call.handle(state)?;
        let json = call.string_arg(json_ptr, "JSON")?;
        let model: LocalDbModel = serde_json::from_str(&json).map_err(|e| {
            AppResponse::SerializationError(format!("Error deserializing JSON: {e:?}"))
        })?;

        let updated = handle
            .legacy(|db| db.update(model.clone()))?
            .ok_or_else(|| AppResponse::NotFound("Model not found for update".to_string()))?;
        serde_json::to_string(&updated).map_err(|e| {
            AppResponse::SerializationError(format!("Error serializing updated model: {e:?}"))
        })
    })
}

/// Deletes a record from the database by its ID.
///
/// # Parameters
///
/// * `db_state` - Handle returned by [`create_db`]
/// * `id` - Null-terminated C string containing the record ID to delete
///
/// # Returns
///
/// A JSON envelope confirming the deletion, or `NotFound`, to be released
/// with [`ofc_free_string`].
///
/// # Safety
///
/// `db_state` must be null or a handle returned by [`create_db`] that has not
/// been closed; `id` must be null or a NUL-terminated string. Both must stay
/// valid for the duration of the call.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, delete_by_id};
///
/// let db_name = CString::new("/absolute/path/test_db").unwrap();
/// let db = unsafe { create_db(db_name.as_ptr()) };
///
/// let id = CString::new("record_to_delete").unwrap();
/// let result = unsafe { delete_by_id(db, id.as_ptr()) };
/// ```
#[no_mangle]
pub unsafe extern "C" fn delete_by_id(db_state: *mut DbHandle, id: *const c_char) -> *const c_char {
    Call::new("delete_by_id").respond(|call| {
        let handle = call.handle(db_state)?;
        let id = call.string_arg(id, "id")?;

        if handle.legacy(|db| db.delete_by_id(&id))? {
            Ok("Record deleted successfully".to_string())
        } else {
            Err(AppResponse::NotFound(format!(
                "No record found with id: {id}"
            )))
        }
    })
}

/// Clears all records from the database.
///
/// This operation removes all records while maintaining the database structure.
/// The database remains operational after this call.
///
/// # Parameters
///
/// * `db_state` - Handle returned by [`create_db`]
///
/// # Returns
///
/// A JSON envelope confirming the operation, to be released with
/// [`ofc_free_string`].
///
/// # Safety
///
/// `db_state` must be null or a handle returned by [`create_db`] that has not
/// been closed.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, clear_all_records};
///
/// let db_name = CString::new("/absolute/path/test_db").unwrap();
/// let db = unsafe { create_db(db_name.as_ptr()) };
///
/// let result = unsafe { clear_all_records(db) };
/// ```
#[no_mangle]
pub unsafe extern "C" fn clear_all_records(db_state: *mut DbHandle) -> *const c_char {
    Call::new("clear_all_records").respond(|call| {
        call.handle(db_state)?
            .legacy(AppDbState::clear_all_records)?;
        Ok("All records cleared successfully".to_string())
    })
}

/// Resets the database to a clean state with a new name.
///
/// This operation:
/// 1. Closes the current database environment
/// 2. Removes the existing database directory
/// 3. Creates a new database at `<name>.lmdb`
///
/// It acts on the database shared by every handle opened on the same path:
/// all of them keep working, on the new database. It waits for the operations
/// in flight on those handles and blocks new ones until it finishes.
///
/// # Parameters
///
/// * `db_state` - Handle returned by [`create_db`]
/// * `name_ptr` - Null-terminated C string with the new database path, used
///   exactly as given (see [`create_db`])
///
/// # Returns
///
/// A JSON envelope indicating success or failure, to be released with
/// [`ofc_free_string`].
///
/// # Errors
///
/// - If another database of this process is open at `name`, nothing changes
///   and a `DatabaseError` reports that the path is already open.
/// - If a later step fails, the database stays closed: every handle sharing
///   it answers `DatabaseError` until it is closed.
///
/// # Safety
///
/// `db_state` must be null or a handle returned by [`create_db`] that has not
/// been closed; `name_ptr` must be null or a NUL-terminated string. Both must
/// stay valid for the duration of the call.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, reset_database};
///
/// let db_name = CString::new("/absolute/path/test_db").unwrap();
/// let db = unsafe { create_db(db_name.as_ptr()) };
///
/// let new_name = CString::new("/absolute/path/reset_db").unwrap();
/// let result = unsafe { reset_database(db, new_name.as_ptr()) };
/// ```
#[no_mangle]
pub unsafe extern "C" fn reset_database(
    db_state: *mut DbHandle,
    name_ptr: *const c_char,
) -> *const c_char {
    Call::new("reset_database").respond(|call| {
        let handle = call.handle(db_state)?;
        let name = call.string_arg(name_ptr, "name")?;

        handle.reset(&name)?;
        Ok(format!("Database '{name}' was reset successfully"))
    })
}

/// Releases a handle returned by [`create_db`].
///
/// Other handles opened on the same path keep working; the database
/// environment is closed, and its files released, when its last handle is
/// released. Closing is required before reopening the same path from another
/// process, and useful before a Flutter hot restart.
///
/// # Parameters
///
/// * `db_state` - Handle returned by [`create_db`]
///
/// # Returns
///
/// A JSON envelope indicating success or failure, to be released with
/// [`ofc_free_string`].
///
/// # Safety
///
/// `db_state` must be null or a handle returned by [`create_db`] that has not
/// been closed, and no other call may be using it concurrently.
///
/// This call takes ownership of the handle: **the pointer is invalid once it
/// returns, whatever the response**. Using it afterwards, or closing it a
/// second time, is undefined behavior, exactly like calling `free` twice.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, close_database};
///
/// let db_name = CString::new("/absolute/path/test_db").unwrap();
/// let db = unsafe { create_db(db_name.as_ptr()) };
///
/// // Before hot restart or application shutdown
/// let result = unsafe { close_database(db) };
/// ```
#[no_mangle]
pub unsafe extern "C" fn close_database(db_state: *mut DbHandle) -> *const c_char {
    Call::new("close_database").respond(|call| {
        drop(call.take_handle(db_state)?);
        Ok("Database connection closed successfully".to_string())
    })
}

/// Releases a string returned by any function of this library.
///
/// Every response string must be released with this function exactly once.
/// Passing null does nothing.
///
/// # Safety
///
/// `ptr` must be null or a string returned by a function of this library that
/// has not been released yet. It must not be used after this call.
///
/// # Examples
///
/// ```no_run
/// use std::ffi::CString;
/// use offline_first_core::{create_db, get_all, ofc_free_string};
///
/// let db_name = CString::new("/absolute/path/test_db").unwrap();
/// let db = unsafe { create_db(db_name.as_ptr()) };
///
/// let records = unsafe { get_all(db) };
/// // ... read the JSON envelope ...
/// unsafe { ofc_free_string(records.cast_mut()) };
/// ```
#[no_mangle]
pub unsafe extern "C" fn ofc_free_string(ptr: *mut c_char) {
    Call::new("ofc_free_string").guard(
        |_| (),
        || {
            if ptr.is_null() {
                return;
            }
            // SAFETY: the caller guarantees `ptr` came from `CString::into_raw`
            // in this library and has not been released yet.
            drop(unsafe { CString::from_raw(ptr) });
        },
    );
}

/// Opens the database stored at `<path>.lmdb` for the query engine and the
/// legacy API, and writes a new handle to `*out`.
///
/// Unlike [`create_db`], failures are reported: the return value is a
/// wire-protocol response (see [`wire`]), `{"v":1,"ok":{}}` on success or
/// `{"v":1,"error":{"code":"LegacyFormat",...}}` for a database written by
/// offline_first_core 0.5 or older (LMDB 0.9), which LMDB 1.0 cannot read.
///
/// `options` is null or a JSON object with the fields of
/// [`engine::OpenOptions`] (`max_dbs`, `initial_map_size`,
/// `max_map_size`); they only apply if the path is not open yet.
///
/// The handle is released with [`close_database`]; the response string with
/// [`ofc_free_string`].
///
/// # Safety
///
/// `path` and `options` must be null or point to NUL-terminated strings valid
/// for the duration of the call; `out` must be null or point to writable
/// memory for one pointer. On failure `*out` is set to null.
#[no_mangle]
pub unsafe extern "C" fn ofc_open(
    path: *const c_char,
    options: *const c_char,
    out: *mut *mut DbHandle,
) -> *const c_char {
    let call = Call::new("ofc_open");
    call.respond_wire(|call| {
        if out.is_null() {
            return wire::error_response("InvalidRequest", "null out pointer passed to ofc_open");
        }
        // SAFETY: `out` is non-null and writable per the contract above.
        unsafe { out.write(ptr::null_mut()) };
        let path = match call.wire_str(path, "path") {
            Ok(path) => path,
            Err(response) => return response,
        };
        let options: OpenOptions = if options.is_null() {
            OpenOptions::default()
        } else {
            let json = match call.wire_str(options, "options") {
                Ok(json) => json,
                Err(response) => return response,
            };
            match serde_json::from_str(&json) {
                Ok(options) => options,
                Err(e) => return wire::error_response("InvalidRequest", &format!("options: {e}")),
            }
        };
        let dir = local_db_state::db_dir_name(&path);
        match DbHandle::open_dir(Path::new(&dir), &options) {
            Ok(handle) => {
                info!("Database opened at {dir}");
                // SAFETY: as above.
                unsafe { out.write(Box::into_raw(Box::new(handle))) };
                format!(r#"{{"v":{},"ok":{{}}}}"#, wire::PROTOCOL_VERSION)
            }
            Err(error) => {
                warn!("Failed to open database at {dir}: {error}");
                wire::error_response(error.code(), &error.to_string())
            }
        }
    })
}

/// Executes one request of the wire protocol (see [`wire`]) on `handle` and
/// returns the response JSON, to be released with [`ofc_free_string`].
///
/// # Safety
///
/// `handle` must be null or a live handle from [`create_db`] or [`ofc_open`];
/// `request` must be null or point to a NUL-terminated string valid for the
/// duration of the call.
#[no_mangle]
pub unsafe extern "C" fn ofc_execute(
    handle: *mut DbHandle,
    request: *const c_char,
) -> *const c_char {
    let call = Call::new("ofc_execute");
    call.respond_wire(|call| {
        // SAFETY: a non-null handle is live per the contract above.
        let Some(handle) = (unsafe { handle.as_ref() }) else {
            return wire::error_response("InvalidRequest", "null handle passed to ofc_execute");
        };
        match call.wire_str(request, "request") {
            Ok(request) => wire::handle(handle.shared(), &request),
            Err(response) => response,
        }
    })
}

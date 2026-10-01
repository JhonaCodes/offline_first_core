//! The C ABI v2 (RFC-001 §12.2), next to the `ofc_*` symbols, which stay.
//!
//! - **Handles are `u64` values, never pointers.** Each is validated against
//!   a registry; ids are never reused, and the kind (database or buffer) is
//!   part of the id. A closed, released, foreign or made-up handle answers
//!   [`LDB_INVALID_HANDLE`] instead of undefined behaviour: a second close
//!   or release is harmless.
//! - **Requests and responses are bytes with a length**, so a request may
//!   hold any byte (a NUL in a string value included), and a response is a
//!   buffer that the library owns until [`ldb_buffer_release`].
//! - Every entry point contains panics ([`LDB_PANIC`]) and answers a status
//!   code; the details of a failed operation travel in its response, as
//!   wire-protocol JSON (`{"v": 1, "error": {...}}`).
//!
//! The header is `include/localdb.h`.

use std::collections::HashMap;
use std::path::Path;
use std::ptr;
use std::slice;
use std::str;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use log::warn;

use crate::boundary::Call;
use crate::engine::OpenOptions;
use crate::handle::DbHandle;
use crate::{local_db_state, wire};

/// A database handle of the ABI v2.
pub type LdbHandle = u64;

/// A response buffer of the ABI v2.
pub type LdbBufferHandle = u64;

/// The version of this ABI, answered by [`ldb_abi_version`].
pub const LDB_ABI_VERSION: u32 = 2;

/// The call ran; its response (for calls that answer one) says how the
/// operation went.
pub const LDB_OK: i32 = 0;
/// The handle is not open: closed, released, of the other kind, or never
/// issued.
pub const LDB_INVALID_HANDLE: i32 = 1;
/// A required pointer argument is null.
pub const LDB_NULL_POINTER: i32 = 2;
/// A text argument is not UTF-8.
pub const LDB_INVALID_UTF8: i32 = 3;
/// The library failed internally; the panic was contained.
pub const LDB_PANIC: i32 = 4;
/// The request is larger than [`LDB_MAX_REQUEST_BYTES`] (RFC §12.4).
pub const LDB_REQUEST_TOO_LARGE: i32 = 5;

/// Largest request accepted, in bytes.
pub const LDB_MAX_REQUEST_BYTES: usize = 256 << 20;

const KIND_MASK: u64 = 0b11 << 62;
const DATABASE: u64 = 0b01 << 62;
const BUFFER: u64 = 0b10 << 62;

/// The open handles. A database is an `Arc<DbHandle>`: a request in flight
/// keeps it alive after a concurrent close, and the last owner releases it
/// through the registry (`DbHandle::drop`).
#[derive(Default)]
struct Handles {
    databases: HashMap<u64, Arc<DbHandle>>,
    buffers: HashMap<u64, Box<[u8]>>,
}

static HANDLES: OnceLock<Mutex<Handles>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn handles() -> MutexGuard<'static, Handles> {
    HANDLES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// A new id of `kind`; ids never repeat (2^62 of each kind).
fn next_id(kind: u64) -> u64 {
    kind | (NEXT_ID.fetch_add(1, Ordering::Relaxed) & !KIND_MASK)
}

/// Stores `bytes` as a new buffer and writes its handle to `out`.
///
/// # Safety
///
/// `out` must be non-null and writable.
unsafe fn give_buffer(out: *mut LdbBufferHandle, bytes: String) {
    let id = next_id(BUFFER);
    handles()
        .buffers
        .insert(id, bytes.into_bytes().into_boxed_slice());
    // SAFETY: the caller guarantees `out` is writable.
    unsafe { out.write(id) };
}

/// The text of `ptr` + `len`, or the status code of why not.
///
/// # Safety
///
/// A non-null `ptr` must point to `len` readable bytes for the call.
unsafe fn text<'a>(ptr: *const u8, len: usize) -> Result<&'a str, i32> {
    if ptr.is_null() {
        return if len == 0 {
            Ok("")
        } else {
            Err(LDB_NULL_POINTER)
        };
    }
    // SAFETY: the caller guarantees `len` readable bytes at `ptr`.
    let bytes = unsafe { slice::from_raw_parts(ptr, len) };
    str::from_utf8(bytes).map_err(|_| LDB_INVALID_UTF8)
}

/// The version of the ABI: [`LDB_ABI_VERSION`].
#[no_mangle]
pub extern "C" fn ldb_abi_version() -> u32 {
    LDB_ABI_VERSION
}

/// Opens (or shares) the database at `<path>.lmdb` with `options` (JSON;
/// empty for the defaults). Writes the handle to `out` (0 when the open
/// failed) and the wire response to `response`: `{"v": 1, "ok": {}}`, or the
/// error (`LegacyFormat`, `InvalidRequest`, ...).
///
/// # Safety
///
/// `path` and `options` must point to `path_len` and `options_len` readable
/// bytes (or be null with a length of 0); `out` and `response` must be null
/// or writable.
#[no_mangle]
pub unsafe extern "C" fn ldb_open(
    path: *const u8,
    path_len: usize,
    options: *const u8,
    options_len: usize,
    out: *mut LdbHandle,
    response: *mut LdbBufferHandle,
) -> i32 {
    Call::new("ldb_open").guard(
        |_| LDB_PANIC,
        || {
            if out.is_null() || response.is_null() {
                return LDB_NULL_POINTER;
            }
            // SAFETY: both are non-null and writable per the contract.
            unsafe {
                out.write(0);
                response.write(0);
            }
            // SAFETY: per the contract.
            let (path, options) =
                match unsafe { (text(path, path_len), text(options, options_len)) } {
                    (Ok(path), Ok(options)) => (path, options),
                    (Err(code), _) | (_, Err(code)) => return code,
                };
            let options: OpenOptions = if options.is_empty() {
                OpenOptions::default()
            } else {
                match serde_json::from_str(options) {
                    Ok(options) => options,
                    Err(e) => {
                        let error =
                            wire::error_response("InvalidRequest", &format!("options: {e}"));
                        // SAFETY: `response` is writable.
                        unsafe { give_buffer(response, error) };
                        return LDB_OK;
                    }
                }
            };
            let dir = local_db_state::db_dir_name(path);
            let answer = match DbHandle::open_dir(Path::new(&dir), &options) {
                Ok(handle) => {
                    let id = next_id(DATABASE);
                    handles().databases.insert(id, Arc::new(handle));
                    // SAFETY: `out` is writable.
                    unsafe { out.write(id) };
                    format!(r#"{{"v":{},"ok":{{}}}}"#, wire::PROTOCOL_VERSION)
                }
                Err(error) => {
                    warn!("Failed to open database at {dir}: {error}");
                    wire::error_response(error.code(), &error.to_string())
                }
            };
            // SAFETY: `response` is writable.
            unsafe { give_buffer(response, answer) };
            LDB_OK
        },
    )
}

/// Runs one request of the wire protocol (`request_len` bytes of JSON) on
/// `database` and writes the handle of its response to `response`.
///
/// # Safety
///
/// `request` must point to `request_len` readable bytes; `response` must be
/// null or writable.
#[no_mangle]
pub unsafe extern "C" fn ldb_execute(
    database: LdbHandle,
    request: *const u8,
    request_len: usize,
    response: *mut LdbBufferHandle,
) -> i32 {
    Call::new("ldb_execute").guard(
        |_| LDB_PANIC,
        || {
            if response.is_null() {
                return LDB_NULL_POINTER;
            }
            // SAFETY: `response` is non-null and writable per the contract.
            unsafe { response.write(0) };
            if request_len > LDB_MAX_REQUEST_BYTES {
                return LDB_REQUEST_TOO_LARGE;
            }
            // SAFETY: per the contract.
            let request = match unsafe { text(request, request_len) } {
                Ok(request) => request,
                Err(code) => return code,
            };
            // The lock is released before the request runs.
            let Some(handle) = handles().databases.get(&database).cloned() else {
                return LDB_INVALID_HANDLE;
            };
            let answer = wire::handle(handle.shared(), request);
            drop(handle);
            // SAFETY: `response` is writable.
            unsafe { give_buffer(response, answer) };
            LDB_OK
        },
    )
}

/// Writes where the bytes of `buffer` are and how many: valid until the
/// buffer is released.
///
/// # Safety
///
/// `data` and `len` must be null or writable.
#[no_mangle]
pub unsafe extern "C" fn ldb_buffer_view(
    buffer: LdbBufferHandle,
    data: *mut *const u8,
    len: *mut usize,
) -> i32 {
    Call::new("ldb_buffer_view").guard(
        |_| LDB_PANIC,
        || {
            if data.is_null() || len.is_null() {
                return LDB_NULL_POINTER;
            }
            let handles = handles();
            let Some(bytes) = handles.buffers.get(&buffer) else {
                // SAFETY: both are non-null and writable per the contract.
                unsafe {
                    data.write(ptr::null());
                    len.write(0);
                }
                return LDB_INVALID_HANDLE;
            };
            // SAFETY: as above; the bytes stay allocated (a boxed slice
            // does not move when the map grows) until released.
            unsafe {
                data.write(bytes.as_ptr());
                len.write(bytes.len());
            }
            LDB_OK
        },
    )
}

/// Releases `buffer`; a second release answers [`LDB_INVALID_HANDLE`].
#[no_mangle]
pub extern "C" fn ldb_buffer_release(buffer: LdbBufferHandle) -> i32 {
    Call::new("ldb_buffer_release").guard(
        |_| LDB_PANIC,
        || match handles().buffers.remove(&buffer) {
            Some(_) => LDB_OK,
            None => LDB_INVALID_HANDLE,
        },
    )
}

/// Closes `database`; a second close answers [`LDB_INVALID_HANDLE`]. A
/// request still running on it finishes first; the database closes with its
/// last handle (of either ABI).
#[no_mangle]
pub extern "C" fn ldb_close(database: LdbHandle) -> i32 {
    Call::new("ldb_close").guard(
        |_| LDB_PANIC,
        || {
            // Dropped after the lock: closing the environment may wait.
            let removed = handles().databases.remove(&database);
            match removed {
                Some(handle) => {
                    drop(handle);
                    LDB_OK
                }
                None => LDB_INVALID_HANDLE,
            }
        },
    )
}

//! Plumbing shared by every `extern "C"` entry point: argument decoding,
//! response encoding and panic containment.
//!
//! Every entry point runs its body through [`Call::guard`] (directly, or via
//! [`Call::respond`]), so that behavior is defined in one place.

use std::any::Any;
use std::ffi::{c_char, CStr, CString};
use std::panic::{self, AssertUnwindSafe};

use log::{error, warn};

use crate::app_response::AppResponse;
use crate::handle::DbHandle;

/// One call through the C ABI, named after its entry point for messages and
/// logs.
#[derive(Clone, Copy)]
pub(crate) struct Call {
    entry_point: &'static str,
}

impl Call {
    pub(crate) const fn new(entry_point: &'static str) -> Self {
        Self { entry_point }
    }

    /// Runs the body of an entry point, containing any panic.
    ///
    /// A panic must not unwind out of an `extern "C"` function (since Rust
    /// 1.81 that aborts the process, and with `panic = "abort"` any panic
    /// does). It is caught here, logged, and turned into `on_panic(message)`.
    ///
    /// The body is asserted unwind-safe: after a caught panic the shared state
    /// is either untouched or behind a lock that recovers from poisoning (see
    /// `registry::SharedDb`), and no partially updated value is used again.
    pub(crate) fn guard<T>(self, on_panic: impl FnOnce(&str) -> T, body: impl FnOnce() -> T) -> T {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            #[cfg(feature = "fault-injection")]
            crate::fault_injection::trip(self.entry_point);
            body()
        }));
        match outcome {
            Ok(value) => value,
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                error!("internal panic in {}: {message}", self.entry_point);
                on_panic(message)
            }
        }
    }

    /// Runs the body of an entry point that answers with a response string:
    /// `Ok(payload)` becomes `{"Ok": payload}`, `Err(response)` is sent as is.
    ///
    /// The returned string must be released with
    /// [`ofc_free_string`](crate::ofc_free_string).
    pub(crate) fn respond(
        self,
        body: impl FnOnce(Self) -> Result<String, AppResponse>,
    ) -> *const c_char {
        let on_panic = |message: &str| {
            response_to_c_string(&AppResponse::DatabaseError(format!(
                "internal panic in {}: {message}",
                self.entry_point
            )))
        };
        self.guard(on_panic, || {
            let response = match body(self) {
                Ok(payload) => AppResponse::Ok(payload),
                Err(error) => error,
            };
            response_to_c_string(&response)
        })
    }

    /// Borrows the handle behind `ptr`.
    /// Runs `body` and returns its wire-protocol response as a C string,
    /// answering a panic with an `InternalPanic` error response.
    pub(crate) fn respond_wire(self, body: impl FnOnce(Self) -> String) -> *const c_char {
        let on_panic = |message: &str| {
            into_c_string(crate::wire::error_response(
                "InternalPanic",
                &format!("internal panic in {}: {message}", self.entry_point),
            ))
        };
        self.guard(on_panic, || into_c_string(body(self)))
    }

    /// Reads a string argument for a wire-protocol entry point.
    pub(crate) fn wire_str(self, ptr: *const c_char, field: &str) -> Result<String, String> {
        if ptr.is_null() {
            return Err(crate::wire::error_response(
                "InvalidRequest",
                &format!("null {field} pointer passed to {}", self.entry_point),
            ));
        }
        // SAFETY: the caller of the entry point guarantees that a non-null
        // pointer points to a NUL-terminated string valid for the call.
        match unsafe { CStr::from_ptr(ptr) }.to_str() {
            Ok(value) => Ok(value.to_owned()),
            Err(e) => Err(crate::wire::error_response(
                "InvalidRequest",
                &format!("invalid UTF-8 in {field}: {e}"),
            )),
        }
    }

    pub(crate) fn handle<'a>(self, ptr: *mut DbHandle) -> Result<&'a DbHandle, AppResponse> {
        // SAFETY: every entry point documents that a non-null handle must come
        // from `create_db` and must not have been closed. Only a shared
        // reference is created, so concurrent calls on one handle, from any
        // thread, never alias a mutable reference. The reference does not
        // outlive the call.
        unsafe { ptr.as_ref() }.ok_or_else(|| self.null_pointer("state"))
    }

    /// Takes back ownership of the handle behind `ptr`, to release it.
    pub(crate) fn take_handle(self, ptr: *mut DbHandle) -> Result<Box<DbHandle>, AppResponse> {
        if ptr.is_null() {
            return Err(self.null_pointer("state"));
        }
        // SAFETY: a non-null handle comes from `create_db` (`Box::into_raw`)
        // and the caller hands its ownership over with this call, never
        // using the pointer again (documented on `close_database`).
        Ok(unsafe { Box::from_raw(ptr) })
    }

    /// Copies the C string argument `field`.
    pub(crate) fn string_arg(self, ptr: *const c_char, field: &str) -> Result<String, AppResponse> {
        if ptr.is_null() {
            return Err(self.null_pointer(field));
        }
        // SAFETY: `ptr` is non-null and, per the entry-point contract, points
        // to a NUL-terminated string that stays valid for the whole call.
        match unsafe { CStr::from_ptr(ptr) }.to_str() {
            Ok(value) => Ok(value.to_owned()),
            Err(e) => Err(AppResponse::BadRequest(format!(
                "Invalid UTF-8 in {field}: {e}"
            ))),
        }
    }

    fn null_pointer(self, field: &str) -> AppResponse {
        AppResponse::BadRequest(format!(
            "Null {field} pointer passed to {}",
            self.entry_point
        ))
    }
}

/// Text of a panic payload (`panic!` produces a `&str` or a `String`).
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Encodes a response as a C string owned by the caller, or null if it cannot
/// be encoded.
fn response_to_c_string(response: &AppResponse) -> *const c_char {
    let json = match serde_json::to_string(response) {
        Ok(json) => json,
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

/// Converts a wire response into a C string owned by the caller (released
/// with `ofc_free_string`); null only if the response contains a NUL byte.
fn into_c_string(response: String) -> *const c_char {
    match CString::new(response) {
        Ok(c_str) => c_str.into_raw(),
        Err(e) => {
            warn!("Error creating CString: {e}");
            std::ptr::null()
        }
    }
}

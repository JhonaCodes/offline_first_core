//! Shared helpers for the integration test suite.
//!
//! Every file under `tests/` is compiled as its own test binary and pulls this
//! module in with `mod common;`. Each binary uses only a subset of the helpers,
//! hence the module-wide `dead_code` allowance.
#![allow(dead_code)]

use std::env;
use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::mem;
use std::path::{Path, PathBuf};
use std::process;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use offline_first_core::local_db_model::LocalDbModel;
use offline_first_core::local_db_state::AppDbState;
use offline_first_core::DbHandle;
use offline_first_core::{
    close_database, create_db, get_all, get_by_id, ofc_execute, ofc_free_string, ofc_open,
    push_data, reset_database,
};
use serde_json::{json, Map, Value};

/// Scratch directory Cargo provides to integration tests (inside the target
/// directory, so it is git-ignored). With the default target directory it also
/// sits under the package root, which is the working directory Cargo uses when
/// running tests; the FFI tests rely on that (see [`TestDir::relative_db_name`]).
const TEST_ROOT: &str = env!("CARGO_TARGET_TMPDIR");

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A unique, per-test directory that is removed when the guard is dropped.
///
/// Declare it before any database handle living in it, so the handles are
/// dropped (and their files closed) before the directory is removed.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is set before the UNIX epoch")
            .as_nanos();
        let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = Path::new(TEST_ROOT).join(format!(
            "{label}_{pid}_{nanos}_{sequence}",
            pid = process::id()
        ));
        fs::create_dir_all(&path).expect("failed to create the per-test directory");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Absolute database name for [`AppDbState::init`] and
    /// [`AppDbState::reset_database`], which append `.lmdb` to it.
    pub fn db_name(&self, name: &str) -> String {
        path_to_string(&self.path.join(name))
    }

    /// Directory LMDB creates for `db_name(name)`.
    pub fn db_dir(&self, name: &str) -> PathBuf {
        self.path.join(format!("{name}.lmdb"))
    }

    /// Database name relative to the working directory, to check that relative
    /// names keep resolving against it.
    pub fn relative_db_name(&self, name: &str) -> String {
        let cwd = env::current_dir().expect("the working directory must be readable");
        let absolute = self.path.join(name);
        let relative = absolute.strip_prefix(&cwd).expect(
            "CARGO_TARGET_TMPDIR must live under the working directory for the FFI tests \
             (run them through `cargo test` with the default target directory)",
        );
        path_to_string(relative)
    }

    /// Opens (or creates) the database `name` inside this directory.
    pub fn open(&self, name: &str) -> AppDbState {
        AppDbState::init(self.db_name(name)).expect("failed to initialize the test database")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        // Best effort: a cleanup failure must not mask the test outcome.
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Raw database handle as returned by [`create_db`].
pub type RawDb = *mut DbHandle;

/// Owns a handle returned by [`create_db`] and closes it on drop through
/// [`close_database`], as a foreign caller must. Null handles are accepted and
/// ignored.
pub struct FfiDb {
    ptr: RawDb,
}

// SAFETY: a handle may be used from any thread; the library only ever takes
// shared references to it, and the database behind it is `Sync`. Tests move
// `FfiDb` values to other threads to exercise exactly that.
unsafe impl Send for FfiDb {}

impl FfiDb {
    /// Takes ownership of a pointer returned by `create_db`, null or not.
    pub fn adopt(ptr: RawDb) -> Self {
        Self { ptr }
    }

    /// Creates or opens the database `name` inside `dir` through the FFI entry
    /// point, with its absolute path.
    pub fn open(dir: &TestDir, name: &str) -> Self {
        let name = c_string(&dir.db_name(name));
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        let db = Self::adopt(unsafe { create_db(name.as_ptr()) });
        assert!(!db.ptr.is_null(), "create_db returned a null pointer");
        db
    }

    pub fn ptr(&self) -> RawDb {
        self.ptr
    }

    /// Closes the handle through `close_database` and returns its response.
    pub fn close(self) -> FfiResponse {
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        FfiResponse::take(unsafe { close_database(self.into_raw()) })
    }

    /// Gives up ownership without closing, like a caller that loses the
    /// pointer (for example a Flutter hot restart).
    pub fn into_raw(self) -> RawDb {
        let ptr = self.ptr;
        mem::forget(self);
        ptr
    }

    /// Pushes `create_test_model(id, None)` through `push_data`.
    pub fn push(&self, id: &str) -> FfiResponse {
        let json = serde_json::to_string(&create_test_model(id, None))
            .expect("a test model must serialize");
        let json = c_string(&json);
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        FfiResponse::take(unsafe { push_data(self.ptr, json.as_ptr()) })
    }

    /// Looks `id` up through `get_by_id`.
    pub fn get(&self, id: &str) -> FfiResponse {
        let id = c_string(id);
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        FfiResponse::take(unsafe { get_by_id(self.ptr, id.as_ptr()) })
    }

    /// Sorted ids of every record, through `get_all`.
    pub fn ids(&self) -> Vec<String> {
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        let response = FfiResponse::take(unsafe { get_all(self.ptr) });
        assert_eq!(response.variant, "Ok", "get_all failed: {response:?}");
        let mut ids: Vec<String> = response.models().into_iter().map(|m| m.id).collect();
        ids.sort();
        ids
    }

    /// Resets the database onto the absolute name `name` through `reset_database`.
    pub fn reset(&self, name: &str) -> FfiResponse {
        let name = c_string(name);
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        FfiResponse::take(unsafe { reset_database(self.ptr, name.as_ptr()) })
    }
}

impl Drop for FfiDb {
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }
        // No assertions here: this may run while a failed test unwinds.
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        let response = unsafe { close_database(self.ptr) };
        // SAFETY: `response` was just returned by `close_database` and is
        // freed exactly once (null is accepted).
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        unsafe { ofc_free_string(response.cast_mut()) };
    }
}

/// Decoded `{"<Variant>": "<payload>"}` envelope returned by every FFI call.
#[derive(Debug)]
pub struct FfiResponse {
    pub variant: String,
    pub payload: String,
}

impl FfiResponse {
    /// Takes ownership of a response pointer returned by an FFI entry point,
    /// decodes it and releases it through [`ofc_free_string`], exactly as a
    /// foreign caller must.
    pub fn take(ptr: *const c_char) -> Self {
        assert!(!ptr.is_null(), "FFI call returned a null response pointer");
        // SAFETY: `ptr` is a live, NUL-terminated response string returned by
        // the crate; it is only read here, before being freed below.
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        let text = unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .expect("FFI response must be valid UTF-8")
            .to_owned();
        // SAFETY: `ptr` was returned by an FFI entry point of this library and
        // is freed exactly once, after its last use.
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        unsafe { ofc_free_string(ptr.cast_mut()) };
        let envelope: Map<String, Value> =
            serde_json::from_str(&text).expect("FFI response must be a JSON object");
        assert_eq!(
            envelope.len(),
            1,
            "FFI response must carry exactly one variant: {text}"
        );
        let (variant, payload) = envelope
            .into_iter()
            .next()
            .expect("envelope has exactly one entry");
        let payload = payload
            .as_str()
            .expect("FFI response payload must be a string")
            .to_owned();
        Self { variant, payload }
    }

    /// Payload of an `Ok` response carrying a single serialized model.
    pub fn model(&self) -> LocalDbModel {
        serde_json::from_str(&self.payload).expect("payload must be a serialized LocalDbModel")
    }

    /// Payload of an `Ok` response carrying a serialized list of models.
    pub fn models(&self) -> Vec<LocalDbModel> {
        serde_json::from_str(&self.payload).expect("payload must be a serialized model list")
    }
}

/// Builds a model whose hash is `hash_<id>`; `data` defaults to `{"test": "data"}`.
pub fn create_test_model(id: &str, data: Option<Value>) -> LocalDbModel {
    LocalDbModel {
        id: id.to_string(),
        hash: format!("hash_{id}"),
        data: data.unwrap_or_else(|| json!({"test": "data"})),
    }
}

pub fn c_string(value: &str) -> CString {
    CString::new(value).expect("test input must not contain interior NUL bytes")
}

pub fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is set before the UNIX epoch")
        .as_secs()
}

fn path_to_string(path: &Path) -> String {
    path.to_str()
        .expect("test paths must be valid UTF-8")
        .to_owned()
}

/// A handle opened with `ofc_open`, closed on drop.
pub struct Wire(pub *mut DbHandle);

impl Wire {
    pub fn open(dir: &TestDir, name: &str) -> Self {
        let path = CString::new(dir.db_name(name)).expect("path");
        let mut handle = ptr::null_mut();
        // SAFETY: `path` is a live CString and `handle` is writable.
        let response = take_wire(unsafe { ofc_open(path.as_ptr(), ptr::null(), &mut handle) });
        assert!(response.get("ok").is_some(), "{response}");
        Self(handle)
    }

    pub fn call(&self, request: Value) -> Value {
        let text = CString::new(request.to_string()).expect("request");
        // SAFETY: `self.0` is a live handle and `text` a live CString.
        take_wire(unsafe { ofc_execute(self.0, text.as_ptr()) })
    }

    pub fn ok(&self, request: Value) -> Value {
        let response = self.call(request.clone());
        assert_eq!(response["v"], 1, "{response}");
        response
            .get("ok")
            .cloned()
            .unwrap_or_else(|| panic!("{request} failed: {response}"))
    }

    pub fn error_code(&self, request: Value) -> String {
        let response = self.call(request);
        response["error"]["code"]
            .as_str()
            .unwrap_or_else(|| panic!("expected an error: {response}"))
            .to_string()
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `ofc_open` and is closed exactly once.
        take_wire(unsafe { close_database(self.0) });
    }
}

/// Reads and releases a response string.
pub fn take_wire(response: *const c_char) -> Value {
    assert!(!response.is_null());
    // SAFETY: `response` is a NUL-terminated string returned by the library.
    let text = unsafe { CStr::from_ptr(response) }
        .to_str()
        .expect("UTF-8")
        .to_owned();
    // SAFETY: released exactly once.
    unsafe { ofc_free_string(response.cast_mut()) };
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

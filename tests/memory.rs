//! Ownership of the strings returned across the C ABI.
//!
//! Every response string is allocated by the library and must be released
//! with `ofc_free_string`. This binary installs a counting global allocator
//! (the crate is linked as an rlib, so its allocations go through it too) and
//! checks that the live Rust heap returns to its baseline after many
//! request/free cycles.
//!
//! The allocator counts every thread of the process, so the tests in this
//! binary are serialized with [`MEASUREMENT`] and nothing else runs here.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::{c_char, CString};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use common::{c_string, FfiDb, FfiResponse, TestDir, Wire};
use offline_first_core::{get_by_id, ofc_execute, ofc_free_string, push_data};
use serde_json::json;

/// `System` wrapped with a counter of live (allocated and not yet freed) bytes.
struct CountingAllocator;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

/// Converts an allocation size into a signed delta for [`LIVE_BYTES`].
fn signed(size: usize) -> isize {
    isize::try_from(size).unwrap_or(isize::MAX)
}

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged; the only addition is bookkeeping on an atomic counter, which
// neither allocates nor touches the returned memory.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim; the caller upholds `alloc`'s contract.
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(signed(layout.size()), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded verbatim; the caller upholds `alloc_zeroed`'s contract.
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(signed(layout.size()), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded verbatim; the caller upholds `dealloc`'s contract.
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        unsafe { System.dealloc(ptr, layout) };
        LIVE_BYTES.fetch_sub(signed(layout.size()), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded verbatim; the caller upholds `realloc`'s contract.
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            LIVE_BYTES.fetch_add(signed(new_size) - signed(layout.size()), Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Serializes the measurements: the counter is process-wide.
static MEASUREMENT: Mutex<()> = Mutex::new(());

/// Request/free cycles of the leak check (acceptance criterion of the task).
const CYCLES: usize = 100_000;

/// Cycles run before taking the baseline, so one-off lazy allocations
/// (thread-locals, registries, first-use buffers) are not counted as leaks.
const WARM_UP_CYCLES: usize = 1_000;

/// Allowed drift of the live heap after [`CYCLES`] cycles.
///
/// Every allocation made by a cycle is paired with a free, so the expected
/// drift is 0. The tolerance only absorbs allocations of other threads of the
/// test harness. It is smaller than [`CYCLES`] bytes, so a leak of even one
/// byte per cycle exceeds it.
const TOLERANCE_BYTES: isize = 64 * 1024;

const RECORD_ID: &str = "leak_probe";
const RECORD_JSON: &str = r#"{"id":"leak_probe","hash":"h","data":{"k":"v"}}"#;

fn live_bytes() -> isize {
    LIVE_BYTES.load(Ordering::SeqCst)
}

fn lock_measurement() -> MutexGuard<'static, ()> {
    MEASUREMENT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opens a database holding one record and checks that `get_by_id` answers
/// `Ok` for it, so the measured loop exercises the success path.
fn seeded_db(dir: &TestDir) -> FfiDb {
    let db = FfiDb::open(dir, "db");
    let json = c_string(RECORD_JSON);
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let pushed = FfiResponse::take(unsafe { push_data(db.ptr(), json.as_ptr()) });
    assert_eq!(pushed.variant, "Ok", "seed push failed: {pushed:?}");
    let id = c_string(RECORD_ID);
    // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
    let found = FfiResponse::take(unsafe { get_by_id(db.ptr(), id.as_ptr()) });
    assert_eq!(found.variant, "Ok", "seed lookup failed: {found:?}");
    db
}

/// Runs `cycles` lookups; frees each response through the library when `free`.
fn lookup_cycles(db: &FfiDb, id: &CString, cycles: usize, free: bool) {
    for _ in 0..cycles {
        // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
        let response: *const c_char = unsafe { get_by_id(db.ptr(), id.as_ptr()) };
        assert!(!response.is_null(), "get_by_id returned a null response");
        if free {
            // SAFETY: `response` was just returned by `get_by_id` and is
            // freed exactly once.
            // SAFETY: every pointer argument is null (checked by the callee) or a live pointer from `create_db` / `CString`.
            unsafe { ofc_free_string(response.cast_mut()) };
        }
    }
}

#[test]
fn test_get_by_id_and_free_keeps_heap_stable() {
    let _serial = lock_measurement();
    let dir = TestDir::new("memory_free");
    let db = seeded_db(&dir);
    let id = c_string(RECORD_ID);

    lookup_cycles(&db, &id, WARM_UP_CYCLES, true);
    let baseline = live_bytes();
    lookup_cycles(&db, &id, CYCLES, true);
    let drift = live_bytes() - baseline;
    println!("live heap drift after {CYCLES} cycles: {drift} bytes");

    assert!(
        drift.abs() <= TOLERANCE_BYTES,
        "live heap drifted by {drift} bytes after {CYCLES} get_by_id + ofc_free_string cycles \
         (tolerance {TOLERANCE_BYTES})"
    );
}

/// Control for the oracle above: without freeing, the same loop must leak
/// every response. If this test ever fails, the counter no longer sees the
/// library's allocations and the stability test would pass vacuously.
#[test]
fn test_counter_detects_unfreed_responses() {
    const CONTROL_CYCLES: usize = 10_000;
    let _serial = lock_measurement();
    let dir = TestDir::new("memory_control");
    let db = seeded_db(&dir);
    let id = c_string(RECORD_ID);

    lookup_cycles(&db, &id, WARM_UP_CYCLES, true);
    let baseline = live_bytes();
    lookup_cycles(&db, &id, CONTROL_CYCLES, false);
    let growth = live_bytes() - baseline;
    println!("live heap growth after {CONTROL_CYCLES} unfreed cycles: {growth} bytes");

    // Each leaked response holds at least the serialized record.
    let minimum = signed(CONTROL_CYCLES * RECORD_JSON.len());
    assert!(
        growth >= minimum,
        "expected at least {minimum} leaked bytes without freeing, measured {growth}"
    );
}

/// Requests of the wire protocol, freed with `ofc_free_string`, keep the heap
/// stable too: rows, a sync state, and an error answer.
#[test]
fn test_wire_requests_and_free_keep_heap_stable() {
    const WIRE_CYCLES: usize = 20_000;
    let _serial = lock_measurement();
    let dir = TestDir::new("memory_wire");
    let db = Wire::open(&dir, "wire");
    db.ok(json!({"v": 1, "op": "define_table", "table": {
        "name": "notes", "primary_key": "id", "sync": "primary"
    }}));
    db.ok(json!({"v": 1, "op": "execute", "statement": {
        "op": "insert", "table": "notes", "rows": [{"id": "a", "title": "t"}]
    }}));
    let requests: Vec<CString> = [
        json!({"v": 1, "op": "execute", "statement": {"op": "find", "table": "notes", "key": "a"}}),
        json!({"v": 1, "op": "execute", "statement": {"op": "select", "table": "notes"}}),
        json!({"v": 1, "op": "sync_state", "table": "notes", "key": "a"}),
        json!({"v": 1, "op": "sync_status", "remote": "primary"}),
        json!({"v": 1, "op": "no_such_operation"}),
    ]
    .iter()
    .map(|request| c_string(&request.to_string()))
    .collect();
    let cycles = |count: usize| {
        for _ in 0..count {
            for request in &requests {
                // SAFETY: `db.0` is a live handle and `request` a live CString.
                let response = unsafe { ofc_execute(db.0, request.as_ptr()) };
                assert!(!response.is_null(), "ofc_execute returned a null response");
                // SAFETY: `response` was just returned and is freed exactly once.
                unsafe { ofc_free_string(response.cast_mut()) };
            }
        }
    };

    cycles(WARM_UP_CYCLES);
    let baseline = live_bytes();
    cycles(WIRE_CYCLES);
    let drift = live_bytes() - baseline;
    println!("live heap drift after {WIRE_CYCLES} wire cycles: {drift} bytes");

    assert!(
        drift.abs() <= TOLERANCE_BYTES,
        "live heap drifted by {drift} bytes after {WIRE_CYCLES} cycles of {} requests",
        requests.len()
    );
}

//! Test-only fault injection (cargo feature `fault-injection`).
//!
//! No public API can make an entry point panic, yet the panic containment of
//! the C ABI must be tested through the real entry points. With this feature,
//! a caller arms a fault on its thread and the next entry point it calls
//! panics inside its guard, before running its body. The fault is
//! thread-local, so tests running in parallel do not interfere.

use std::cell::Cell;

use crate::boundary::Call;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static ARMED_LOCK: Cell<Option<LockSite>> = const { Cell::new(None) };
}

/// A lock a fault can be armed inside, to test that a panic while it is
/// held leaves it recoverable: a poisoned lock must not disable the
/// database for the rest of the process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LockSite {
    /// The registry of open databases.
    Registry,
    /// The write lock of one open database.
    Database,
}

/// Makes the next entry point called on this thread panic inside its guard.
///
/// Test only: exported with the `fault-injection` feature, which must never
/// be enabled in a shipped build. Arming twice fires the pending fault on the
/// second arming call itself, leaving the thread disarmed.
#[no_mangle]
pub extern "C" fn ofc_fault_injection_arm() {
    Call::new("ofc_fault_injection_arm").guard(|_| (), || ARMED.with(|armed| armed.set(true)));
}

/// Panics if a fault is armed on this thread, disarming it.
pub(crate) fn trip(entry_point: &str) {
    if ARMED.with(|armed| armed.replace(false)) {
        panic!("fault injected in {entry_point}");
    }
}

/// Makes the next acquisition of the registry lock on this thread panic
/// while the lock is held, which poisons it.
///
/// Test only, like [`ofc_fault_injection_arm`].
#[no_mangle]
pub extern "C" fn ofc_fault_injection_arm_in_registry_lock() {
    arm_lock(LockSite::Registry);
}

/// Makes the next acquisition of a database write lock on this thread panic
/// while the lock is held, which poisons it.
///
/// Test only, like [`ofc_fault_injection_arm`].
#[no_mangle]
pub extern "C" fn ofc_fault_injection_arm_in_database_lock() {
    arm_lock(LockSite::Database);
}

fn arm_lock(site: LockSite) {
    Call::new("ofc_fault_injection_arm_in_lock")
        .guard(|_| (), || ARMED_LOCK.with(|armed| armed.set(Some(site))));
}

/// Panics, with `site` held by the caller, if a fault is armed on this
/// thread for `site`; disarms it.
pub(crate) fn trip_in_lock(site: LockSite) {
    if ARMED_LOCK.with(|armed| armed.get() == Some(site)) {
        ARMED_LOCK.with(|armed| armed.set(None));
        panic!("fault injected inside the {site:?} lock");
    }
}

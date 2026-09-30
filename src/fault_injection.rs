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

//! SIGTERM, and why this process handles it at all.
//!
//! The parent's teardown ladder sends SIGTERM before it ever sends SIGKILL, and
//! that rung only helps if this process HANDLES it. SIGTERM's default
//! disposition terminates without unwinding, so the render aggregate's `Drop`
//! never runs and a private aggregate is left wrapping the user's output
//! device — which is the leak the ladder's last rung exists to detect. `std`
//! has no way for a process to install a signal handler (`Child::kill()` is the
//! parent's side, and it is SIGKILL), so this is one `libc::signal` call.
//!
//! **The handler body is one relaxed-ordering store and it must stay that way.**
//! A store to an `AtomicBool` is async-signal-safe. Allocating, locking,
//! logging and formatting are not: a handler that took a lock the interrupted
//! thread already held would deadlock the process with the device still open.
//!
//! There is no way to CLEAR the flag, deliberately. A reset hook would be a
//! second way to un-arm a safety abort, and the process is short-lived enough
//! that "armed once, armed for good" costs nothing.

use std::sync::atomic::{AtomicBool, Ordering};

/// The one abort cell. `abort\n` on stdin, EOF, and SIGTERM all write HERE, so
/// the render loop polls one predicate and every trigger fades rather than
/// stopping.
static ABORT: AtomicBool = AtomicBool::new(false);

/// The SIGTERM handler.
///
/// `extern "C"` and non-unwinding: a panic crossing a signal frame is undefined
/// behaviour, and there is nothing here that can panic.
pub extern "C" fn on_sigterm(_sig: libc::c_int) {
    // The ONLY statement allowed in here. See the module header.
    ABORT.store(true, Ordering::SeqCst);
}

/// Install the handler. Called once, from `run`, before any other thread
/// exists.
pub fn arm() {
    // A function ITEM cast straight to an integer is a lint (and the pointer
    // is what `sighandler_t` actually holds), so go through a pointer.
    let handler = on_sigterm as *const () as libc::sighandler_t;
    // SAFETY: `on_sigterm` is `extern "C"`, never unwinds, and touches only a
    // `static AtomicBool`; `SIGTERM` is a valid signal number on every
    // supported target; and `signal` is called exactly once, from the main
    // thread, before this process spawns any other.
    unsafe { libc::signal(libc::SIGTERM, handler) };
}

/// Arm the abort from the stdin reader (`abort\n`, or EOF).
///
/// The SAME cell [`on_sigterm`] writes. That is the reason SIGTERM ramps rather
/// than clicks: the two triggers are not two paths that both happen to stop
/// playback, they are one flag.
pub fn request_abort() {
    ABORT.store(true, Ordering::SeqCst);
}

/// Has anything asked for an abort? Polled once per block by the render loop.
pub fn aborting() -> bool {
    ABORT.load(Ordering::SeqCst)
}

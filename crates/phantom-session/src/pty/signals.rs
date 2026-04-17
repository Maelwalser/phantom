//! Process-global signal state and async-signal-safe handlers.
//!
//! **Do not move these statics or handlers into a `Mutex`, `OnceCell`, or any
//! other synchronization primitive.** POSIX async-signal-safety restricts
//! what a handler may touch — only `sig_atomic_t` (approximated here by
//! `AtomicBool`/`AtomicI32`) and a small set of syscalls including `ioctl`.
//!
//! This file is also the reason only one PTY session may run per process at a
//! time: `SIGINT_RECEIVED` and `SIGWINCH_MASTER_FD` are inherently global,
//! and a second concurrent session would race on them silently. The guard in
//! `session.rs` makes that single-session constraint explicit.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Flag set by our SIGINT handler to prevent process termination during PTY sessions.
pub(super) static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Guards against concurrent `spawn_with_pty` calls within the same process.
/// POSIX signal handlers can only access global state, so `SIGINT_RECEIVED` is
/// inherently process-global. This flag makes the single-session constraint
/// explicit rather than silently racy.
pub(super) static PTY_SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Master PTY fd for the SIGWINCH handler to propagate terminal resize.
pub(super) static SIGWINCH_MASTER_FD: AtomicI32 = AtomicI32::new(-1);

/// Async-signal-safe SIGINT handler that sets a flag instead of terminating.
pub(super) extern "C" fn handle_sigint(_sig: libc::c_int) {
    SIGINT_RECEIVED.store(true, Ordering::Release);
}

/// Async-signal-safe SIGWINCH handler that propagates terminal size to the PTY.
pub(super) extern "C" fn handle_sigwinch(_sig: libc::c_int) {
    let master = SIGWINCH_MASTER_FD.load(Ordering::Acquire);
    if master < 0 {
        return;
    }
    // SAFETY: ioctl is async-signal-safe. STDIN_FILENO is valid during the
    // session. master fd is valid while PTY_SESSION_ACTIVE is true, which is
    // guaranteed because the guard clears SIGWINCH_MASTER_FD before closing it.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 {
            libc::ioctl(master, libc::TIOCSWINSZ, &ws);
        }
    }
}

//! RAII guards used by the PTY session.
//!
//! Each guard cleans up on drop — including during panic unwind — so the
//! `spawn_with_pty` body never has to remember cleanup on error paths.
//!
//! **Drop order matters.** `session.rs` drops `sigwinch_guard` before closing
//! the master fd, and `termios_guard` before `sigint_guard`. See that file
//! for the rationale; the compiler cannot enforce this ordering.

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::process::ExitStatus;
use std::sync::atomic::Ordering;

use anyhow::Context;
use nix::sys::termios::{self, SetArg, Termios};

use super::signals::{
    PTY_SESSION_ACTIVE, SIGINT_RECEIVED, SIGWINCH_MASTER_FD, handle_sigint, handle_sigwinch,
};

// ---------------------------------------------------------------------------
// Terminal guard
// ---------------------------------------------------------------------------

/// RAII guard that restores terminal settings on drop.
pub struct TermiosGuard {
    fd: OwnedFd,
    original: Termios,
}

impl TermiosGuard {
    /// Save current terminal settings for `fd` and return a guard that restores
    /// them when dropped.
    ///
    /// # Safety
    /// `raw_fd` must be a valid, open file descriptor that outlives the guard.
    /// We duplicate it so the guard owns its own copy.
    pub fn save(raw_fd: BorrowedFd<'_>) -> anyhow::Result<Self> {
        let owned = nix::unistd::dup(raw_fd).context("failed to dup fd for termios guard")?;
        let original = termios::tcgetattr(&owned).context("failed to get terminal attributes")?;
        Ok(Self {
            fd: owned,
            original,
        })
    }
}

impl Drop for TermiosGuard {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(&self.fd, SetArg::TCSANOW, &self.original);
    }
}

// ---------------------------------------------------------------------------
// Signal handler guard
// ---------------------------------------------------------------------------

/// RAII guard that restores the previous SIGINT handler on drop.
///
/// If a panic unwinds past `spawn_with_pty`, the original handler is still
/// restored — unlike a bare `unsafe { sigaction(...) }` at the end of the
/// function, which would be skipped.
pub(super) struct SigactionGuard {
    old: libc::sigaction,
}

impl SigactionGuard {
    /// Install a custom SIGINT handler that sets `SIGINT_RECEIVED` instead of
    /// terminating the process. Returns a guard that restores the previous
    /// handler when dropped.
    ///
    /// # Safety
    /// `handle_sigint` must be async-signal-safe (it is — atomic store only).
    pub(super) fn install() -> Self {
        let old: libc::sigaction = unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle_sigint as *const () as usize;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&raw mut sa.sa_mask);
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGINT, &raw const sa, &raw mut old);
            old
        };
        SIGINT_RECEIVED.store(false, Ordering::Release);
        Self { old }
    }
}

impl Drop for SigactionGuard {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGINT, &raw const self.old, std::ptr::null_mut());
        }
    }
}

// ---------------------------------------------------------------------------
// SIGWINCH handler guard
// ---------------------------------------------------------------------------

/// RAII guard that installs a SIGWINCH handler to propagate terminal resize
/// events to the child PTY, and restores the previous handler on drop.
pub(super) struct SigwinchGuard {
    old: libc::sigaction,
}

impl SigwinchGuard {
    /// Install a SIGWINCH handler that copies the parent terminal size to the
    /// PTY master fd. The master fd must remain valid for the lifetime of this
    /// guard.
    ///
    /// # Safety
    /// `handle_sigwinch` is async-signal-safe (ioctl + atomic load only).
    pub(super) fn install(master_raw_fd: RawFd) -> Self {
        SIGWINCH_MASTER_FD.store(master_raw_fd, Ordering::Release);
        let old: libc::sigaction = unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle_sigwinch as *const () as usize;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&raw mut sa.sa_mask);
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGWINCH, &raw const sa, &raw mut old);
            old
        };
        Self { old }
    }
}

impl Drop for SigwinchGuard {
    fn drop(&mut self) {
        // Clear the master fd before restoring the old handler to ensure the
        // signal handler never accesses a closed fd.
        SIGWINCH_MASTER_FD.store(-1, Ordering::Release);
        unsafe {
            libc::sigaction(libc::SIGWINCH, &raw const self.old, std::ptr::null_mut());
        }
    }
}

// ---------------------------------------------------------------------------
// Child process guard
// ---------------------------------------------------------------------------

/// RAII guard that ensures a child process is terminated if it has not been
/// successfully waited on before the guard is dropped (e.g., during panic unwind).
pub(super) struct ChildGuard(Option<std::process::Child>);

impl ChildGuard {
    pub(super) fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    /// Wait for the child process, consuming the guard without triggering cleanup.
    pub(super) fn wait(mut self) -> io::Result<ExitStatus> {
        // take() ensures Drop sees None and skips the kill.
        self.0.take().unwrap().wait()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            // Best-effort: SIGKILL the child to prevent orphaning.
            let _ = child.kill();
            // Reap the zombie to avoid pid leak.
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// Single-session guard
// ---------------------------------------------------------------------------

/// RAII guard that clears `PTY_SESSION_ACTIVE` on drop.
///
/// Only one PTY session may run per process at a time because the SIGINT /
/// SIGWINCH handlers share process-global state (see `signals.rs`).
pub(super) struct SessionActiveGuard;

impl SessionActiveGuard {
    /// Attempt to acquire the session slot. Returns `None` if another session
    /// is already active in this process.
    pub(super) fn acquire() -> Option<Self> {
        if PTY_SESSION_ACTIVE.swap(true, Ordering::AcqRel) {
            None
        } else {
            Some(Self)
        }
    }
}

impl Drop for SessionActiveGuard {
    fn drop(&mut self) {
        PTY_SESSION_ACTIVE.store(false, Ordering::Release);
    }
}

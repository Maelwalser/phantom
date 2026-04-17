//! PTY-based process spawning for interactive CLI sessions.
//!
//! Provides terminal management (raw mode, signal handling) and output capture
//! for session ID extraction.
//!
//! Module layout:
//! - [`signals`] — process-global signal state and async-signal-safe handlers.
//!   Must stay a single file because POSIX async-signal-safety restricts what
//!   handlers may touch. Do not replace statics with `Mutex`/`OnceCell`.
//! - [`guards`] — RAII guards for terminal state, signal handlers, child
//!   processes, and the single-session slot.
//! - [`io`] — non-blocking write helper and tail-buffer capacity constant.
//! - [`session`] — [`spawn_with_pty`] orchestration. Cleanup ordering is
//!   documented at the top of that file and must not be rearranged.
//! - [`direct`] — [`spawn_direct`] fallback for non-TTY inputs.

mod direct;
mod guards;
mod io;
mod session;
mod signals;

pub use direct::spawn_direct;
pub use guards::TermiosGuard;
pub use session::spawn_with_pty;

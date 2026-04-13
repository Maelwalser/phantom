//! PTY-based process spawning for interactive CLI sessions.
//!
//! Provides terminal management (raw mode, signal handling) and output capture
//! for session ID extraction.

use std::collections::VecDeque;
use std::io::{self, Write as _};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use nix::pty::openpty;
use nix::sys::termios::{self, SetArg, Termios};

use crate::adapter::CliAdapter;

/// Flag set by our SIGINT handler to prevent process termination during PTY sessions.
static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Async-signal-safe SIGINT handler that sets a flag instead of terminating.
extern "C" fn handle_sigint(_sig: libc::c_int) {
    SIGINT_RECEIVED.store(true, Ordering::Release);
}

/// Size of the rolling buffer that captures the tail of terminal output (bytes).
const OUTPUT_TAIL_CAP: usize = 8192;

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
// PTY-based process spawning
// ---------------------------------------------------------------------------

/// Spawn an interactive CLI process inside a PTY.
///
/// Returns the exit status and an optional session ID extracted from the
/// trailing output buffer.
pub fn spawn_with_pty(
    adapter: &dyn CliAdapter,
    work_dir: &Path,
    session_id: Option<&str>,
    env_vars: &[(&str, &str)],
) -> anyhow::Result<(ExitStatus, Option<String>)> {
    // 1. Open a PTY pair.
    let pty = openpty(None, None).context("failed to open PTY")?;
    let master_fd = pty.master;
    let slave_fd = pty.slave;

    // 2. Build the command via the adapter.
    let mut cmd = adapter.build_command(work_dir, session_id, env_vars);

    // Set the slave fd as stdin/stdout/stderr for the child.
    // SAFETY: slave_fd is a valid open file descriptor. We use `dup` so the
    // child gets its own copies and `slave_fd` can be dropped in the parent.
    unsafe {
        use std::process::Stdio;
        let slave_raw = slave_fd.as_raw_fd();
        cmd.stdin(Stdio::from_raw_fd(libc::dup(slave_raw)));
        cmd.stdout(Stdio::from_raw_fd(libc::dup(slave_raw)));
        cmd.stderr(Stdio::from_raw_fd(libc::dup(slave_raw)));
    }

    // 3. Switch the real terminal to raw mode so keystrokes pass through.
    //    SAFETY: stdin (fd 0) is valid while the process is alive.
    let stdin_borrowed = unsafe { BorrowedFd::borrow_raw(libc::STDIN_FILENO) };
    let guard = TermiosGuard::save(stdin_borrowed)?;

    let mut raw =
        termios::tcgetattr(stdin_borrowed).context("failed to get terminal attributes")?;
    termios::cfmakeraw(&mut raw);
    termios::tcsetattr(stdin_borrowed, SetArg::TCSANOW, &raw)
        .context("failed to set raw terminal mode")?;

    // Install a SIGINT handler that prevents process termination.
    // The child process (Claude Code) handles Ctrl+C itself via the PTY.
    // We only need to survive SIGINT so we can clean up the terminal.
    //
    // SAFETY: handle_sigint is async-signal-safe (atomic store only).
    // We save the old handler to restore after cleanup.
    let old_sigint: libc::sigaction = unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_sigint as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGINT, &sa, &mut old);
        old
    };
    SIGINT_RECEIVED.store(false, Ordering::Release);

    // 4. Spawn the child process.
    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to launch '{}' -- is it installed and on PATH?",
            adapter.name()
        )
    })?;

    // Close the slave fd in the parent -- the child inherited copies.
    drop(slave_fd);

    // 5. Forward stdin -> master and master -> stdout in separate threads.
    //    Duplicate master_fd so each thread owns its own fd.
    let master_write_fd: OwnedFd =
        nix::unistd::dup(&master_fd).context("failed to dup master PTY for write")?;
    let master_read_fd: OwnedFd =
        nix::unistd::dup(&master_fd).context("failed to dup master PTY for read")?;

    // Create a pipe used to signal both threads to stop when the child exits.
    // When we close `shutdown_write`, poll() on the read ends returns POLLHUP.
    let (shutdown_read, shutdown_write) =
        nix::unistd::pipe().context("failed to create shutdown pipe")?;
    let capture_shutdown_read: OwnedFd =
        nix::unistd::dup(&shutdown_read).context("failed to dup shutdown pipe for capture")?;

    // stdin -> master (forwarder thread)
    // Uses poll() to multiplex stdin and the shutdown pipe so the thread
    // exits promptly when the child process terminates.
    let stdin_thread = std::thread::spawn(move || {
        use nix::poll::{PollFd, PollFlags, PollTimeout};

        let stdin_raw: RawFd = libc::STDIN_FILENO;
        let shutdown_raw: RawFd = shutdown_read.as_raw_fd();
        let mut master_write = std::fs::File::from(master_write_fd);
        let mut buf = [0u8; 4096];

        loop {
            // SAFETY: both fds are valid open file descriptors for the
            // lifetime of this loop iteration. `shutdown_read` is owned by
            // this thread; stdin is process-global and valid.
            let stdin_borrow = unsafe { BorrowedFd::borrow_raw(stdin_raw) };
            let shutdown_borrow = unsafe { BorrowedFd::borrow_raw(shutdown_raw) };

            let mut fds = [
                PollFd::new(stdin_borrow, PollFlags::POLLIN),
                PollFd::new(shutdown_borrow, PollFlags::POLLIN),
            ];

            match nix::poll::poll(&mut fds, PollTimeout::NONE) {
                Ok(0) => continue,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(_) => break,
                Ok(_) => {}
            }

            // Shutdown pipe signalled -- child exited, stop forwarding.
            if let Some(revents) = fds[1].revents()
                && revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
            {
                break;
            }

            // stdin has data ready.
            if let Some(revents) = fds[0].revents() {
                if revents.intersects(PollFlags::POLLIN) {
                    // SAFETY: stdin_raw is STDIN_FILENO, valid and open.
                    let n = unsafe { libc::read(stdin_raw, buf.as_mut_ptr().cast(), buf.len()) };
                    if n < 0 {
                        let err = io::Error::last_os_error();
                        if err.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    if n == 0 {
                        break;
                    }
                    if master_write.write_all(&buf[..n as usize]).is_err() {
                        break;
                    }
                }
                if revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
                    break;
                }
            }
        }

        drop(shutdown_read);
    });

    // master -> stdout + rolling buffer (capture thread)
    // Uses poll() to multiplex PTY master reads and the shutdown pipe so
    // the thread exits promptly even if orphaned child processes keep the
    // PTY slave open after Claude Code exits.
    let capture_thread = std::thread::spawn(move || -> VecDeque<u8> {
        use nix::poll::{PollFd, PollFlags, PollTimeout};

        let master_raw: RawFd = master_read_fd.as_raw_fd();
        let shutdown_raw: RawFd = capture_shutdown_read.as_raw_fd();
        let mut stdout = io::stdout().lock();
        let mut tail_buf: VecDeque<u8> = VecDeque::with_capacity(OUTPUT_TAIL_CAP);
        let mut buf = [0u8; 4096];

        loop {
            // SAFETY: both fds are valid open file descriptors for the
            // lifetime of this loop iteration.
            let master_borrow = unsafe { BorrowedFd::borrow_raw(master_raw) };
            let shutdown_borrow = unsafe { BorrowedFd::borrow_raw(shutdown_raw) };

            let mut fds = [
                PollFd::new(master_borrow, PollFlags::POLLIN),
                PollFd::new(shutdown_borrow, PollFlags::POLLIN),
            ];

            match nix::poll::poll(&mut fds, PollTimeout::NONE) {
                Ok(0) => continue,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(_) => break,
                Ok(_) => {}
            }

            // Shutdown pipe signalled -- child exited, stop capturing.
            if let Some(revents) = fds[1].revents()
                && revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
            {
                break;
            }

            // PTY master has data ready.
            if let Some(revents) = fds[0].revents() {
                if revents.intersects(PollFlags::POLLIN) {
                    // SAFETY: master_raw is a valid open fd owned by master_read_fd.
                    let n = unsafe { libc::read(master_raw, buf.as_mut_ptr().cast(), buf.len()) };
                    if n < 0 {
                        let err = io::Error::last_os_error();
                        if err.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        break;
                    }
                    if n == 0 {
                        break;
                    }

                    // Forward to real stdout.
                    let _ = stdout.write_all(&buf[..n as usize]);
                    let _ = stdout.flush();

                    // Append to rolling buffer.
                    for &b in &buf[..n as usize] {
                        if tail_buf.len() >= OUTPUT_TAIL_CAP {
                            tail_buf.pop_front();
                        }
                        tail_buf.push_back(b);
                    }
                }
                if revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
                    break;
                }
            }
        }

        drop(capture_shutdown_read);
        drop(master_read_fd);
        tail_buf
    });

    // 6. Wait for the child to exit.
    let exit_status = child
        .wait()
        .context("failed to wait for interactive session")?;

    // Block SIGINT during cleanup to ensure terminal restoration completes.
    // Our custom handler already prevents termination, but masking ensures
    // no interruption of the cleanup sequence at all.
    use nix::sys::signal::{SigSet, SigmaskHow, Signal};
    let mut sigint_mask = SigSet::empty();
    sigint_mask.add(Signal::SIGINT);
    let mut old_sigmask = SigSet::empty();
    let _ = nix::sys::signal::sigprocmask(
        SigmaskHow::SIG_BLOCK,
        Some(&sigint_mask),
        Some(&mut old_sigmask),
    );

    // Signal the stdin thread to stop by closing the shutdown pipe write end.
    // This causes POLLHUP on the read end, breaking the poll loop.
    drop(shutdown_write);

    // Drop the master fd to signal EOF to the capture thread.
    drop(master_fd);

    // Join threads -- both will now exit promptly.
    let _ = stdin_thread.join();
    let tail_buf = capture_thread
        .join()
        .map_err(|_| anyhow::anyhow!("capture thread panicked"))?;

    // 7. Restore terminal FIRST (while SIGINT is still blocked).
    drop(guard);

    // Restore the original SIGINT handler.
    // SAFETY: old_sigint was captured from the previous handler.
    unsafe {
        libc::sigaction(libc::SIGINT, &old_sigint, std::ptr::null_mut());
    }

    // Unmask SIGINT.
    let _ = nix::sys::signal::sigprocmask(SigmaskHow::SIG_SETMASK, Some(&old_sigmask), None);

    // 8. Extract session ID from the captured tail.
    let tail_bytes = Vec::from(tail_buf);
    let tail_str = String::from_utf8_lossy(&tail_bytes);
    let extracted_id = adapter.extract_session_id(&tail_str);

    Ok((exit_status, extracted_id))
}

// ---------------------------------------------------------------------------
// Direct spawn fallback (no PTY, no output capture)
// ---------------------------------------------------------------------------

/// Spawn the CLI process with inherited stdio (no output capture).
///
/// Used when stdin is not a terminal (tests, CI, piped input).
pub fn spawn_direct(
    adapter: &dyn CliAdapter,
    work_dir: &Path,
    session_id: Option<&str>,
    env_vars: &[(&str, &str)],
) -> anyhow::Result<(ExitStatus, Option<String>)> {
    use std::process::Stdio;

    let mut cmd = adapter.build_command(work_dir, session_id, env_vars);
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to launch '{}' -- is it installed and on PATH?",
            adapter.name()
        )
    })?;

    let exit_status = child
        .wait()
        .context("failed to wait for interactive session")?;

    // No output capture possible without PTY -- session ID not available.
    Ok((exit_status, None))
}

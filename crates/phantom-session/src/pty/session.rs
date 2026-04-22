//! PTY session orchestration — the wiring for [`spawn_with_pty`].
//!
//! ## Cleanup ordering
//!
//! The end-of-session cleanup sequence is order-sensitive and **must not be
//! rearranged** without understanding why:
//!
//! 1. Drop `shutdown_write` — signals the two forwarder threads to exit.
//! 2. Drop `sigwinch_guard` — clears `SIGWINCH_MASTER_FD` *before* the master
//!    fd is closed, so the signal handler can never touch a stale fd.
//! 3. Drop `master_fd` — EOF on the capture thread.
//! 4. Join both threads.
//! 5. Drop `termios_guard` — restore raw/cooked mode.
//! 6. Drop `sigint_guard` — restore the previous SIGINT handler.
//! 7. `_session_guard` drops (implicitly), clearing `PTY_SESSION_ACTIVE`.
//!
//! SIGINT is masked around steps 5–6 so a well-timed Ctrl+C cannot interrupt
//! terminal restoration.

use std::collections::VecDeque;
use std::io::{self, Write as _};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::path::Path;
use std::process::ExitStatus;

use anyhow::Context;
use nix::pty::openpty;
use nix::sys::termios::{self, SetArg};

use crate::adapter::CliAdapter;

use super::guards::{ChildGuard, SessionActiveGuard, SigactionGuard, SigwinchGuard, TermiosGuard};
use super::io::{OUTPUT_TAIL_CAP, nb_write_all, set_nonblocking};

/// Spawn an interactive CLI process inside a PTY.
///
/// Returns the exit status and an optional session ID extracted from the
/// trailing output buffer.
pub fn spawn_with_pty(
    adapter: &dyn CliAdapter,
    work_dir: &Path,
    session_id: Option<&str>,
    env_vars: &[(&str, &str)],
    system_prompt_file: Option<&Path>,
    hook_settings_file: Option<&Path>,
) -> anyhow::Result<(ExitStatus, Option<String>)> {
    // Guard: only one PTY session per process. The SIGINT handler writes to a
    // global AtomicBool, so concurrent sessions would race on it.
    let _session_guard = SessionActiveGuard::acquire()
        .ok_or_else(|| anyhow::anyhow!("a PTY session is already active in this process"))?;

    // 1. Open a PTY pair, inheriting the parent terminal's dimensions.
    let initial_winsize = {
        // SAFETY: STDIN_FILENO is valid while the process is alive.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) };
        if ret == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            Some(ws)
        } else {
            None
        }
    };
    let pty = openpty(initial_winsize.as_ref(), None).context("failed to open PTY")?;
    let master_fd = pty.master;
    let slave_fd = pty.slave;

    // 2. Build the command via the adapter.
    let mut cmd = adapter.build_command(
        work_dir,
        session_id,
        env_vars,
        system_prompt_file,
        hook_settings_file,
    );

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
    let termios_guard = TermiosGuard::save(stdin_borrowed)?;

    let mut raw =
        termios::tcgetattr(stdin_borrowed).context("failed to get terminal attributes")?;
    termios::cfmakeraw(&mut raw);
    termios::tcsetattr(stdin_borrowed, SetArg::TCSANOW, &raw)
        .context("failed to set raw terminal mode")?;

    // Install a SIGINT handler that prevents process termination.
    // The child process (Claude Code) handles Ctrl+C itself via the PTY.
    // We only need to survive SIGINT so we can clean up the terminal.
    // The RAII guard restores the previous handler on drop (including panics).
    let sigint_guard = SigactionGuard::install();

    // Install a SIGWINCH handler that propagates terminal resize events to the
    // child PTY. Without this the child sees a fixed size and never reflows.
    let sigwinch_guard = SigwinchGuard::install(master_fd.as_raw_fd());

    // 4. Spawn the child process.
    let child = ChildGuard::new(cmd.spawn().with_context(|| {
        format!(
            "failed to launch '{}' -- is it installed and on PATH?",
            adapter.name()
        )
    })?);

    // Close the slave fd in the parent -- the child inherited copies.
    drop(slave_fd);

    // 5. Forward stdin -> master and master -> stdout in separate threads.
    //    Duplicate master_fd so each thread owns its own fd.
    let master_write_fd: OwnedFd =
        nix::unistd::dup(&master_fd).context("failed to dup master PTY for write")?;
    let master_read_fd: OwnedFd =
        nix::unistd::dup(&master_fd).context("failed to dup master PTY for read")?;

    // Set the write fd to non-blocking so writes cannot deadlock the stdin
    // thread when the PTY buffer is full (e.g. child process stops reading).
    set_nonblocking(&master_write_fd)
        .context("failed to set master PTY write fd to non-blocking")?;

    // Create a pipe used to signal both threads to stop when the child exits.
    // When we close `shutdown_write`, poll() on the read ends returns POLLHUP.
    let (shutdown_read, shutdown_write) =
        nix::unistd::pipe().context("failed to create shutdown pipe")?;
    let capture_shutdown_read: OwnedFd =
        nix::unistd::dup(&shutdown_read).context("failed to dup shutdown pipe for capture")?;

    let stdin_thread = spawn_stdin_forwarder(shutdown_read, master_write_fd);
    let capture_thread = spawn_output_capture(capture_shutdown_read, master_read_fd);

    // 6. Wait for the child to exit (consumes the guard, preventing kill-on-drop).
    let exit_status = child
        .wait()
        .map_err(|e| anyhow::anyhow!(e))
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

    // Restore the SIGWINCH handler before closing the master fd, so the
    // signal handler never accesses a closed fd.
    drop(sigwinch_guard);

    // Drop the master fd to signal EOF to the capture thread.
    drop(master_fd);

    // Join threads -- both will now exit promptly.
    let _ = stdin_thread.join();
    let tail_buf = capture_thread
        .join()
        .map_err(|_| anyhow::anyhow!("capture thread panicked"))?;

    // 7. Restore terminal and SIGINT handler (while SIGINT is still blocked).
    //    Both are RAII guards — drop order is reverse declaration order, but
    //    we drop explicitly here for clarity: terminal first, then signal handler.
    drop(termios_guard);
    drop(sigint_guard);

    // Unmask SIGINT.
    let _ = nix::sys::signal::sigprocmask(SigmaskHow::SIG_SETMASK, Some(&old_sigmask), None);

    // _session_guard drops here, clearing PTY_SESSION_ACTIVE.

    // 8. Extract session ID from the captured tail.
    let tail_bytes = Vec::from(tail_buf);
    let tail_str = String::from_utf8_lossy(&tail_bytes);
    let extracted_id = adapter.extract_session_id(&tail_str);

    Ok((exit_status, extracted_id))
}

/// stdin -> master forwarder thread.
///
/// Uses `poll()` to multiplex stdin and the shutdown pipe so the thread exits
/// promptly when the child process terminates. Writes to the master fd use
/// [`nb_write_all`] which polls the shutdown pipe on `WouldBlock`, preventing
/// deadlock if the PTY buffer is full.
fn spawn_stdin_forwarder(
    shutdown_read: OwnedFd,
    master_write_fd: OwnedFd,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        use nix::poll::{PollFd, PollFlags, PollTimeout};

        let stdin_raw: RawFd = libc::STDIN_FILENO;
        let shutdown_raw: RawFd = shutdown_read.as_raw_fd();
        let master_write_raw: RawFd = master_write_fd.as_raw_fd();
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
                Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
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
                    if !nb_write_all(master_write_raw, shutdown_raw, &buf[..n as usize]) {
                        break;
                    }
                }
                if revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
                    break;
                }
            }
        }

        drop(shutdown_read);
        drop(master_write_fd);
    })
}

/// master -> stdout + rolling buffer capture thread.
///
/// Uses `poll()` to multiplex PTY master reads and the shutdown pipe so the
/// thread exits promptly even if orphaned child processes keep the PTY slave
/// open after the CLI exits. Stdout writes use `let _ =` to ignore errors —
/// stdout is a shared resource and setting it non-blocking would affect the
/// entire process.
fn spawn_output_capture(
    capture_shutdown_read: OwnedFd,
    master_read_fd: OwnedFd,
) -> std::thread::JoinHandle<VecDeque<u8>> {
    std::thread::spawn(move || -> VecDeque<u8> {
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
                Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
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

                    // Forward to real stdout (ignore errors — stdout is shared).
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
    })
}

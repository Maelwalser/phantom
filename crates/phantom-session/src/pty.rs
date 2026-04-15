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
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use anyhow::Context;
use nix::pty::openpty;
use nix::sys::termios::{self, SetArg, Termios};

use crate::adapter::CliAdapter;

/// Flag set by our SIGINT handler to prevent process termination during PTY sessions.
static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Guards against concurrent `spawn_with_pty` calls within the same process.
/// POSIX signal handlers can only access global state, so `SIGINT_RECEIVED` is
/// inherently process-global. This flag makes the single-session constraint
/// explicit rather than silently racy.
static PTY_SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Master PTY fd for the SIGWINCH handler to propagate terminal resize.
static SIGWINCH_MASTER_FD: AtomicI32 = AtomicI32::new(-1);

/// Async-signal-safe SIGINT handler that sets a flag instead of terminating.
extern "C" fn handle_sigint(_sig: libc::c_int) {
    SIGINT_RECEIVED.store(true, Ordering::Release);
}

/// Async-signal-safe SIGWINCH handler that propagates terminal size to the PTY.
extern "C" fn handle_sigwinch(_sig: libc::c_int) {
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
// Signal handler guard
// ---------------------------------------------------------------------------

/// RAII guard that restores the previous SIGINT handler on drop.
///
/// If a panic unwinds past `spawn_with_pty`, the original handler is still
/// restored — unlike a bare `unsafe { sigaction(...) }` at the end of the
/// function, which would be skipped.
struct SigactionGuard {
    old: libc::sigaction,
}

impl SigactionGuard {
    /// Install a custom SIGINT handler that sets `SIGINT_RECEIVED` instead of
    /// terminating the process. Returns a guard that restores the previous
    /// handler when dropped.
    ///
    /// # Safety
    /// `handle_sigint` must be async-signal-safe (it is — atomic store only).
    fn install() -> Self {
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
struct SigwinchGuard {
    old: libc::sigaction,
}

impl SigwinchGuard {
    /// Install a SIGWINCH handler that copies the parent terminal size to the
    /// PTY master fd. The master fd must remain valid for the lifetime of this
    /// guard.
    ///
    /// # Safety
    /// `handle_sigwinch` is async-signal-safe (ioctl + atomic load only).
    fn install(master_raw_fd: RawFd) -> Self {
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
struct ChildGuard(Option<std::process::Child>);

impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        Self(Some(child))
    }

    /// Wait for the child process, consuming the guard without triggering cleanup.
    fn wait(mut self) -> io::Result<ExitStatus> {
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
// Non-blocking write helper
// ---------------------------------------------------------------------------

/// Write `data` to `fd` without blocking indefinitely.
///
/// The fd must be in non-blocking mode. On `WouldBlock`, polls `fd` and
/// `shutdown_fd` together. Returns `false` if the shutdown pipe fires or an
/// unrecoverable error occurs, signalling the caller to exit its loop.
fn nb_write_all(fd: RawFd, shutdown_fd: RawFd, data: &[u8]) -> bool {
    use nix::poll::{PollFd, PollFlags, PollTimeout};

    let mut offset = 0;
    while offset < data.len() {
        let n = unsafe { libc::write(fd, data[offset..].as_ptr().cast(), data.len() - offset) };
        if n > 0 {
            offset += n as usize;
            continue;
        }
        if n == 0 {
            return false;
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if err.kind() != io::ErrorKind::WouldBlock {
            return false;
        }
        // WouldBlock — poll until writable or shutdown.
        let fd_borrow = unsafe { BorrowedFd::borrow_raw(fd) };
        let shutdown_borrow = unsafe { BorrowedFd::borrow_raw(shutdown_fd) };
        let mut fds = [
            PollFd::new(fd_borrow, PollFlags::POLLOUT),
            PollFd::new(shutdown_borrow, PollFlags::POLLIN),
        ];
        match nix::poll::poll(&mut fds, PollTimeout::from(5000u16)) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return false,
            Ok(_) => {}
        }
        if let Some(revents) = fds[1].revents()
            && revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
        {
            return false;
        }
    }
    true
}

/// Set a file descriptor to non-blocking mode.
fn set_nonblocking(fd: &OwnedFd) -> anyhow::Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
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
    system_prompt_file: Option<&Path>,
) -> anyhow::Result<(ExitStatus, Option<String>)> {
    // Guard: only one PTY session per process. The SIGINT handler writes to a
    // global AtomicBool, so concurrent sessions would race on it.
    if PTY_SESSION_ACTIVE.swap(true, Ordering::AcqRel) {
        anyhow::bail!("a PTY session is already active in this process");
    }
    // Ensure the flag is cleared on all exit paths (including panic unwind).
    struct SessionActiveGuard;
    impl Drop for SessionActiveGuard {
        fn drop(&mut self) {
            PTY_SESSION_ACTIVE.store(false, Ordering::Release);
        }
    }
    let _session_guard = SessionActiveGuard;

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
    let mut cmd = adapter.build_command(work_dir, session_id, env_vars, system_prompt_file);

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

    // stdin -> master (forwarder thread)
    // Uses poll() to multiplex stdin and the shutdown pipe so the thread
    // exits promptly when the child process terminates.
    // Writes to the master fd use nb_write_all() which polls the shutdown
    // pipe on WouldBlock, preventing deadlock if the PTY buffer is full.
    let stdin_thread = std::thread::spawn(move || {
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
    });

    // master -> stdout + rolling buffer (capture thread)
    // Uses poll() to multiplex PTY master reads and the shutdown pipe so
    // the thread exits promptly even if orphaned child processes keep the
    // PTY slave open after Claude Code exits.
    // Stdout writes use `let _ =` to ignore errors — stdout is a shared
    // resource and setting it non-blocking would affect the entire process.
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
    });

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
    system_prompt_file: Option<&Path>,
) -> anyhow::Result<(ExitStatus, Option<String>)> {
    use std::process::Stdio;

    let mut cmd = adapter.build_command(work_dir, session_id, env_vars, system_prompt_file);
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let child = ChildGuard::new(cmd.spawn().with_context(|| {
        format!(
            "failed to launch '{}' -- is it installed and on PATH?",
            adapter.name()
        )
    })?);

    let exit_status = child
        .wait()
        .map_err(|e| anyhow::anyhow!(e))
        .context("failed to wait for interactive session")?;

    // No output capture possible without PTY -- session ID not available.
    Ok((exit_status, None))
}

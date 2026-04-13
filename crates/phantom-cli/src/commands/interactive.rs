//! Interactive session launcher for `phantom dispatch`.
//!
//! Spawns a CLI process (defaults to `claude`) inside the agent's overlay
//! directory. Uses a PTY (pseudo-terminal) so the child gets a real terminal
//! for TUI rendering while we capture output to extract session IDs for
//! resume support.
//!
//! Handles post-session automation: auto-submit and auto-materialize.

use std::collections::VecDeque;
use std::io::{self, Write as _};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::path::Path;
use std::process::ExitStatus;

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use chrono::Utc;
use nix::pty::openpty;
use nix::sys::termios::{self, SetArg, Termios};
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_orchestrator::materializer::MaterializeResult;
use tracing::warn;

use super::dispatch::DispatchArgs;
use crate::cli_adapter::{self, CliAdapter, CliSession};
use crate::context::PhantomContext;

/// Flag set by our SIGINT handler to prevent process termination during PTY sessions.
static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

/// Async-signal-safe SIGINT handler that sets a flag instead of terminating.
extern "C" fn handle_sigint(_sig: libc::c_int) {
    SIGINT_RECEIVED.store(true, Ordering::Release);
}

/// Name of the generated context file placed in the overlay.
const CONTEXT_FILE: &str = ".phantom-task.md";

/// Size of the rolling buffer that captures the tail of terminal output (bytes).
const OUTPUT_TAIL_CAP: usize = 8192;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run an interactive CLI session inside the agent's overlay.
///
/// Blocks until the spawned process exits, then optionally auto-submits and
/// auto-materializes the changeset.
pub async fn run_interactive_session(
    ctx: &mut PhantomContext,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    work_dir: &Path,
    args: &DispatchArgs,
) -> anyhow::Result<()> {
    let command = args.command.as_deref().unwrap_or("claude");
    let adapter = cli_adapter::adapter_for(command);

    // Write context file into the working directory.
    write_context_file(work_dir, agent_id, changeset_id, base_commit, args.task.as_deref())?;

    // Load a previously saved session for this agent + CLI combination.
    let existing_session = cli_adapter::load_session(&ctx.phantom_dir, agent_id);
    let session_id = existing_session
        .as_ref()
        .filter(|s| s.cli_name == adapter.name())
        .map(|s| s.session_id.as_str());

    // Environment variables passed to the CLI process.
    let env_vars: Vec<(&str, String)> = vec![
        ("PHANTOM_AGENT_ID", agent_id.0.clone()),
        ("PHANTOM_CHANGESET_ID", changeset_id.0.clone()),
        (
            "PHANTOM_OVERLAY_DIR",
            work_dir.to_str().unwrap_or_default().to_string(),
        ),
        (
            "PHANTOM_REPO_ROOT",
            ctx.repo_root.to_str().unwrap_or_default().to_string(),
        ),
        ("PHANTOM_INTERACTIVE", "1".to_string()),
    ];
    let env_refs: Vec<(&str, &str)> = env_vars
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .collect();

    // Use PTY when stdin is a terminal (enables output capture for session IDs).
    // Fall back to direct Stdio::inherit() when not a TTY (tests, CI, piped input).
    let is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
    let (exit_status, captured_session_id) = if is_tty {
        spawn_with_pty(adapter.as_ref(), work_dir, session_id, &env_refs)?
    } else {
        spawn_direct(adapter.as_ref(), work_dir, session_id, &env_refs)?
    };

    // Persist the session ID for next dispatch.
    if let Some(ref sid) = captured_session_id {
        let session = CliSession {
            cli_name: adapter.name().to_string(),
            session_id: sid.clone(),
            last_used: Utc::now(),
        };
        if let Err(e) = cli_adapter::save_session(&ctx.phantom_dir, agent_id, &session) {
            warn!(error = %e, "failed to save CLI session for resume");
        }
    }

    // Cleanup context file if auto-submitting.
    let auto_submit = args.auto_submit || args.auto_materialize;
    if auto_submit {
        cleanup_context_file(work_dir);
        if let Ok(upper_dir) = ctx.overlays.upper_dir(agent_id) {
            cleanup_context_file(upper_dir);
        }
    }

    println!();
    if let Some(code) = exit_status.code() {
        if code != 0 {
            println!("Interactive session exited with code {code}.");
        } else {
            println!("Interactive session ended.");
        }
    } else {
        println!("Interactive session terminated by signal.");
    }

    // Post-session automation.
    post_session_flow(
        ctx,
        agent_id,
        changeset_id,
        auto_submit,
        args.auto_materialize,
    )
    .await
}

// ---------------------------------------------------------------------------
// PTY-based process spawning
// ---------------------------------------------------------------------------

/// RAII guard that restores terminal settings on drop.
struct TermiosGuard {
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
    fn save(raw_fd: BorrowedFd<'_>) -> anyhow::Result<Self> {
        let owned = nix::unistd::dup(raw_fd).context("failed to dup fd for termios guard")?;
        let original = termios::tcgetattr(&owned).context("failed to get terminal attributes")?;
        Ok(Self { fd: owned, original })
    }
}

impl Drop for TermiosGuard {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(&self.fd, SetArg::TCSANOW, &self.original);
    }
}

/// Spawn an interactive CLI process inside a PTY.
///
/// Returns the exit status and an optional session ID extracted from the
/// trailing output buffer.
fn spawn_with_pty(
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

    let mut raw = termios::tcgetattr(stdin_borrowed)
        .context("failed to get terminal attributes")?;
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
            "failed to launch '{}' — is it installed and on PATH?",
            adapter.name()
        )
    })?;

    // Close the slave fd in the parent — the child inherited copies.
    drop(slave_fd);

    // 5. Forward stdin → master and master → stdout in separate threads.
    //    Duplicate master_fd so each thread owns its own fd.
    let master_write_fd: OwnedFd =
        nix::unistd::dup(&master_fd).context("failed to dup master PTY for write")?;
    let master_read_fd: OwnedFd =
        nix::unistd::dup(&master_fd).context("failed to dup master PTY for read")?;

    // Create a pipe used to signal both threads to stop when the child exits.
    // When we close `shutdown_write`, poll() on the read ends returns POLLHUP.
    let (shutdown_read, shutdown_write) = nix::unistd::pipe()
        .context("failed to create shutdown pipe")?;
    let capture_shutdown_read: OwnedFd =
        nix::unistd::dup(&shutdown_read).context("failed to dup shutdown pipe for capture")?;

    // stdin → master (forwarder thread)
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

            // Shutdown pipe signalled — child exited, stop forwarding.
            if let Some(revents) = fds[1].revents() {
                if revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
                {
                    break;
                }
            }

            // stdin has data ready.
            if let Some(revents) = fds[0].revents() {
                if revents.intersects(PollFlags::POLLIN) {
                    // SAFETY: stdin_raw is STDIN_FILENO, valid and open.
                    let n = unsafe {
                        libc::read(stdin_raw, buf.as_mut_ptr().cast(), buf.len())
                    };
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

    // master → stdout + rolling buffer (capture thread)
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

            // Shutdown pipe signalled — child exited, stop capturing.
            if let Some(revents) = fds[1].revents() {
                if revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
                {
                    break;
                }
            }

            // PTY master has data ready.
            if let Some(revents) = fds[0].revents() {
                if revents.intersects(PollFlags::POLLIN) {
                    // SAFETY: master_raw is a valid open fd owned by master_read_fd.
                    let n = unsafe {
                        libc::read(master_raw, buf.as_mut_ptr().cast(), buf.len())
                    };
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
    let exit_status = child.wait().context("failed to wait for interactive session")?;

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

    // Join threads — both will now exit promptly.
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
fn spawn_direct(
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
            "failed to launch '{}' — is it installed and on PATH?",
            adapter.name()
        )
    })?;

    let exit_status = child.wait().context("failed to wait for interactive session")?;

    // No output capture possible without PTY — session ID not available.
    Ok((exit_status, None))
}

// ---------------------------------------------------------------------------
// Context file management
// ---------------------------------------------------------------------------

/// Write a context file into the overlay with agent metadata and optional task.
pub(super) fn write_context_file(
    upper_dir: &Path,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    task: Option<&str>,
) -> anyhow::Result<()> {
    let base_hex = base_commit.to_hex();
    let base_short = &base_hex[..12.min(base_hex.len())];

    let task_section = match task {
        Some(t) if !t.is_empty() => format!("\n## Task\n{t}\n"),
        _ => String::new(),
    };

    let content = format!(
        r#"# Phantom Agent Session

You are working inside a Phantom overlay. Your changes are isolated from
trunk and other agents.
{task_section}
## Agent Info
- Agent: {agent_id}
- Changeset: {changeset_id}
- Base commit: {base_short}

## Commands
- `phantom submit {agent_id}` — submit your changes
- `phantom materialize {changeset_id}` — merge to trunk
- `phantom status` — view all agents and changesets
"#
    );

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write context file to {}", path.display()))?;

    Ok(())
}

/// Remove the generated context file from the overlay.
fn cleanup_context_file(upper_dir: &Path) {
    let path = upper_dir.join(CONTEXT_FILE);
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!(path = %path.display(), error = %e, "failed to clean up context file");
    }
}

// ---------------------------------------------------------------------------
// Post-session automation
// ---------------------------------------------------------------------------

/// Handle post-session submit and materialize automation.
async fn post_session_flow(
    ctx: &mut PhantomContext,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    auto_submit: bool,
    auto_materialize: bool,
) -> anyhow::Result<()> {
    let layer = ctx
        .overlays
        .get_layer(agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let modified = layer.modified_files().map_err(|e| anyhow::anyhow!("{e}"))?;

    if modified.is_empty() {
        println!("No changes detected in overlay.");
        return Ok(());
    }

    println!("{} file(s) modified in overlay.", modified.len());

    if !auto_submit {
        println!(
            "Run `phantom submit {agent_id}` to submit, then `phantom materialize {changeset_id}` to merge."
        );
        return Ok(());
    }

    // Auto-submit
    println!("Auto-submitting changeset...");
    match super::submit::submit_agent(ctx, agent_id).await? {
        Some(cs_id) => {
            println!("Changeset {cs_id} submitted.");

            if auto_materialize {
                println!("Auto-materializing...");
                match super::materialize::materialize_changeset(ctx, &cs_id, &agent_id.0).await? {
                    MaterializeResult::Success { new_commit } => {
                        let hex = new_commit.to_hex();
                        let short = &hex[..12.min(hex.len())];
                        println!("Materialized {cs_id} → commit {short}");
                    }
                    MaterializeResult::Conflict { details } => {
                        eprintln!("Materialization failed with {} conflict(s):", details.len());
                        for detail in &details {
                            eprintln!(
                                "  [{:?}] {} — {}",
                                detail.kind,
                                detail.file.display(),
                                detail.description
                            );
                        }
                        anyhow::bail!("materialization failed due to conflicts");
                    }
                }
            } else {
                println!("Run `phantom materialize {cs_id}` to merge to trunk.");
            }
        }
        None => {
            println!("No changes to submit (files may have been reverted).");
        }
    }

    Ok(())
}

//! Fallback spawn path when stdin is not a terminal (tests, CI, piped input).
//!
//! Inherits stdio directly — no PTY, no output capture, no session-ID
//! extraction possible.

use std::path::Path;
use std::process::ExitStatus;

use anyhow::Context;

use crate::adapter::CliAdapter;

use super::guards::ChildGuard;

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

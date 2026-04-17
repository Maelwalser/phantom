//! Spawn the `_agent-monitor` subprocess for a background agent.
//!
//! The monitor is the parent of the agent CLI, so it can `waitpid` for the
//! real exit code and perform post-completion work (auto-submit, cleanup).

use std::path::Path;

use anyhow::Context;
use phantom_core::id::ChangesetId;

/// Spawn the `phantom _agent-monitor` process which will in turn spawn and
/// monitor the agent CLI process. This ensures the monitor is the parent of
/// the agent and can `waitpid` to get the real exit code.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_agent_monitor(
    phantom_dir: &Path,
    repo_root: &Path,
    agent: &str,
    changeset_id: &ChangesetId,
    task: &str,
    work_dir: &Path,
    cli_command: &str,
    system_prompt_file: Option<&Path>,
    depends_on_agents: &[String],
) -> anyhow::Result<()> {
    let phantom_bin = std::env::current_exe().context("failed to find phantom binary")?;
    let overlay_root = phantom_dir.join("overlays").join(agent);
    let monitor_pid_file = overlay_root.join("monitor.pid");

    let mut cmd = std::process::Command::new(&phantom_bin);
    cmd.arg("_agent-monitor")
        .arg("--agent")
        .arg(agent)
        .arg("--changeset-id")
        .arg(&changeset_id.0)
        .arg("--task")
        .arg(task)
        .arg("--work-dir")
        .arg(work_dir.as_os_str())
        .arg("--repo-root")
        .arg(repo_root)
        .arg("--cli-command")
        .arg(cli_command);

    if let Some(path) = system_prompt_file {
        cmd.arg("--system-prompt-file").arg(path);
    }

    if !depends_on_agents.is_empty() {
        cmd.arg("--depends-on-agents")
            .arg(depends_on_agents.join(","));
    }

    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn agent monitor")?;

    crate::pid_guard::write_pid_file(&monitor_pid_file, child.id() as i32)
        .context("failed to write monitor PID file")?;

    Ok(())
}

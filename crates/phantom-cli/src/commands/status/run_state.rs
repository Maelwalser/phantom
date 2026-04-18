//! Background-agent run-state detection by reading PID, status, and
//! waiting-marker files from disk.

use std::path::Path;
use std::time::Duration;

use super::super::agent_monitor;

/// Run state of a background agent process.
#[derive(Debug)]
pub enum AgentRunState {
    /// Agent process is currently running.
    Running { pid: u32, elapsed: Duration },
    /// Agent monitor is waiting for upstream dependencies to materialize.
    WaitingForDependencies { upstream: Vec<String> },
    /// Agent process finished successfully.
    Finished,
    /// Agent process failed or crashed.
    Failed {
        status: Option<agent_monitor::AgentStatus>,
    },
    /// No background process — interactive or not yet launched.
    Idle,
}

/// Read the run state of a background agent from disk.
pub fn read_agent_run_state(phantom_dir: &Path, agent: &str) -> AgentRunState {
    // Check for completion marker first.
    let status_file = agent_monitor::status_path(phantom_dir, agent);
    if let Ok(content) = std::fs::read_to_string(&status_file)
        && let Ok(status) = serde_json::from_str::<agent_monitor::AgentStatus>(&content)
    {
        return if status.exit_code == Some(0) && status.error.is_none() {
            AgentRunState::Finished
        } else {
            AgentRunState::Failed {
                status: Some(status),
            }
        };
    }

    // Check for dependency wait marker (monitor running, claude not yet spawned).
    let waiting_file = phantom_dir
        .join("overlays")
        .join(agent)
        .join("waiting.json");
    if let Ok(content) = std::fs::read_to_string(&waiting_file) {
        // Verify the monitor is still alive (with PID reuse protection).
        let monitor_pid_file = agent_monitor::monitor_pid_path(phantom_dir, agent);
        let monitor_alive = crate::pid_guard::read_pid_file(&monitor_pid_file)
            .is_some_and(|r| crate::pid_guard::is_process_alive(&r));

        if monitor_alive {
            let upstream: Vec<String> = serde_json::from_str(&content).unwrap_or_default();
            return AgentRunState::WaitingForDependencies { upstream };
        }
        // Monitor died while waiting — clean up marker and fall through to Failed.
        let _ = std::fs::remove_file(&waiting_file);
    }

    // Check for running process (with PID reuse protection).
    let pid_file = agent_monitor::pid_path(phantom_dir, agent);
    if let Some(record) = crate::pid_guard::read_pid_file(&pid_file) {
        if crate::pid_guard::is_process_alive(&record) {
            // Estimate elapsed time from PID file modification time.
            let elapsed = std::fs::metadata(&pid_file)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .unwrap_or_default();
            return AgentRunState::Running {
                pid: record.pid as u32,
                elapsed,
            };
        }

        // CLI child exited but agent.status isn't written yet. The monitor
        // writes the status only after the post-session flow (submit +
        // materialize) finishes, which can take tens of seconds for a large
        // first commit. If the monitor is still alive, the agent is
        // effectively "finalizing" — never a failure. Only treat a dead
        // agent.pid as a crash once monitor.pid is also gone/dead.
        let monitor_pid_file = agent_monitor::monitor_pid_path(phantom_dir, agent);
        let monitor_alive = crate::pid_guard::read_pid_file(&monitor_pid_file)
            .is_some_and(|r| crate::pid_guard::is_process_alive(&r));

        if monitor_alive {
            let elapsed = std::fs::metadata(&pid_file)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .unwrap_or_default();
            return AgentRunState::Running {
                pid: record.pid as u32,
                elapsed,
            };
        }

        // Both agent CLI and monitor are dead with no status written — crashed.
        return AgentRunState::Failed { status: None };
    }

    AgentRunState::Idle
}

/// Format a duration as "Xh Ym Zs" or "Xm Zs" or "Zs".
pub fn format_duration(d: &Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else if secs == 0 {
        "just started".to_string()
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup(
        tmp: &TempDir,
        agent: &str,
        agent_pid: Option<i32>,
        monitor_pid: Option<i32>,
    ) -> std::path::PathBuf {
        let overlay = tmp.path().join("overlays").join(agent);
        fs::create_dir_all(&overlay).unwrap();
        if let Some(p) = agent_pid {
            crate::pid_guard::write_pid_file(&overlay.join("agent.pid"), p).unwrap();
        }
        if let Some(p) = monitor_pid {
            crate::pid_guard::write_pid_file(&overlay.join("monitor.pid"), p).unwrap();
        }
        tmp.path().to_path_buf()
    }

    #[test]
    fn running_when_monitor_alive_even_if_cli_dead() {
        let tmp = TempDir::new().unwrap();
        let self_pid = std::process::id() as i32;
        let phantom_dir = setup(&tmp, "alpha", Some(i32::MAX), Some(self_pid));

        let state = read_agent_run_state(&phantom_dir, "alpha");
        assert!(
            matches!(state, AgentRunState::Running { .. }),
            "expected Running (monitor finalizing), got {state:?}"
        );
    }

    #[test]
    fn failed_when_both_cli_and_monitor_dead_and_no_status() {
        let tmp = TempDir::new().unwrap();
        let phantom_dir = setup(&tmp, "beta", Some(i32::MAX), Some(i32::MAX));

        let state = read_agent_run_state(&phantom_dir, "beta");
        assert!(
            matches!(state, AgentRunState::Failed { status: None }),
            "expected Failed, got {state:?}"
        );
    }

    #[test]
    fn failed_when_cli_dead_and_no_monitor_pid() {
        let tmp = TempDir::new().unwrap();
        let phantom_dir = setup(&tmp, "gamma", Some(i32::MAX), None);

        let state = read_agent_run_state(&phantom_dir, "gamma");
        assert!(
            matches!(state, AgentRunState::Failed { status: None }),
            "expected Failed (no monitor), got {state:?}"
        );
    }

    #[test]
    fn status_file_wins_over_pid_check() {
        let tmp = TempDir::new().unwrap();
        let self_pid = std::process::id() as i32;
        let phantom_dir = setup(&tmp, "delta", Some(i32::MAX), Some(self_pid));

        let status = agent_monitor::AgentStatus {
            exit_code: Some(0),
            completed_at: chrono::Utc::now(),
            materialized: true,
            error: None,
        };
        fs::write(
            phantom_dir
                .join("overlays")
                .join("delta")
                .join("agent.status"),
            serde_json::to_string(&status).unwrap(),
        )
        .unwrap();

        let state = read_agent_run_state(&phantom_dir, "delta");
        assert!(matches!(state, AgentRunState::Finished), "got {state:?}");
    }
}

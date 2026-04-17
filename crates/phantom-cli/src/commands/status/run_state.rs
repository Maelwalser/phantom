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

        // Process is dead but no status file — crashed.
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

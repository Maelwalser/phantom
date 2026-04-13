//! CLI adapter trait and implementations for session resumption.
//!
//! Each coding CLI (Claude Code, Aider, Codex, etc.) has its own mechanism for
//! session resumption. This module provides a trait-based abstraction so that
//! `phantom <agent>` can capture and replay session IDs regardless of which
//! CLI is being used.

use std::path::Path;
use std::process::Command;

use anyhow::Context;
use chrono::{DateTime, Utc};
use phantom_core::id::AgentId;
use regex::Regex;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Session persistence
// ---------------------------------------------------------------------------

/// Persisted session state for a coding CLI session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliSession {
    /// Which CLI produced this session (e.g. "claude").
    pub cli_name: String,
    /// The opaque session identifier (UUID for Claude Code).
    pub session_id: String,
    /// When this session was last used.
    pub last_used: DateTime<Utc>,
}

/// Path to the session file for an agent overlay.
fn session_path(phantom_dir: &Path, agent_id: &AgentId) -> std::path::PathBuf {
    phantom_dir
        .join("overlays")
        .join(&agent_id.0)
        .join("cli_session.json")
}

/// Load a previously saved CLI session for this agent, if one exists.
pub fn load_session(phantom_dir: &Path, agent_id: &AgentId) -> Option<CliSession> {
    let path = session_path(phantom_dir, agent_id);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Persist a CLI session to disk so it can be resumed on the next task invocation.
pub fn save_session(
    phantom_dir: &Path,
    agent_id: &AgentId,
    session: &CliSession,
) -> anyhow::Result<()> {
    let path = session_path(phantom_dir, agent_id);
    let json = serde_json::to_string_pretty(session).context("failed to serialize CLI session")?;
    std::fs::write(&path, json)
        .with_context(|| format!("failed to write CLI session to {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI adapter trait
// ---------------------------------------------------------------------------

/// Abstraction over different coding CLIs for session management.
///
/// Each implementation knows how to:
/// - Build the CLI command (with or without a resume flag)
/// - Extract a session ID from the CLI's terminal output
pub trait CliAdapter {
    /// Short name used to match against stored sessions (e.g. "claude").
    fn name(&self) -> &str;

    /// Build the `Command` to spawn. When `session_id` is `Some`, the command
    /// should include whatever flag the CLI uses to resume a prior session.
    fn build_command(
        &self,
        work_dir: &Path,
        session_id: Option<&str>,
        env_vars: &[(&str, &str)],
    ) -> Command;

    /// Build a headless (non-interactive) command for background execution.
    ///
    /// Returns `Some(Command)` if the CLI supports headless mode, `None` otherwise.
    /// For Claude Code this uses `-p` (prompt mode) instead of interactive mode.
    ///
    /// When `system_prompt_file` is `Some`, the CLI should append the file's
    /// contents to its system prompt (e.g. `--append-system-prompt-file`).
    fn build_headless_command(
        &self,
        _work_dir: &Path,
        _task: &str,
        _env_vars: &[(&str, &str)],
        _system_prompt_file: Option<&Path>,
    ) -> Option<Command> {
        None
    }

    /// Scan the trailing output buffer for a session ID.
    ///
    /// Called with the last ~8 KB of terminal output after the process exits.
    /// Returns `Some(id)` if a resumable session ID is found.
    fn extract_session_id(&self, output_tail: &str) -> Option<String>;
}

// ---------------------------------------------------------------------------
// Claude Code adapter
// ---------------------------------------------------------------------------

pub struct ClaudeAdapter;

impl CliAdapter for ClaudeAdapter {
    fn name(&self) -> &str {
        "claude"
    }

    fn build_command(
        &self,
        work_dir: &Path,
        session_id: Option<&str>,
        env_vars: &[(&str, &str)],
    ) -> Command {
        let mut cmd = Command::new("claude");
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        // Resume a prior session if we have one.
        if let Some(id) = session_id {
            cmd.args(["--resume", id]);
        }

        cmd.args(["--allowedTools", "Edit", "Write", "Read", "Bash"]);

        if let Some(dir_str) = work_dir.to_str() {
            cmd.args(["--add-dir", dir_str]);
        }

        cmd
    }

    fn build_headless_command(
        &self,
        work_dir: &Path,
        task: &str,
        env_vars: &[(&str, &str)],
        system_prompt_file: Option<&Path>,
    ) -> Option<Command> {
        let mut cmd = Command::new("claude");
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        // Use -p for non-interactive prompt mode.
        cmd.args(["-p", task]);
        cmd.args(["--allowedTools", "Edit", "Write", "Read", "Bash"]);

        if let Some(dir_str) = work_dir.to_str() {
            cmd.args(["--add-dir", dir_str]);
        }

        // Inject custom instructions while preserving built-in capabilities.
        if let Some(path) = system_prompt_file
            && let Some(path_str) = path.to_str()
        {
            cmd.args(["--append-system-prompt-file", path_str]);
        }

        Some(cmd)
    }

    fn extract_session_id(&self, output_tail: &str) -> Option<String> {
        // Claude Code prints: "claude --resume <UUID>" near the end of output.
        let re = Regex::new(
            r"claude --resume ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})",
        )
        .ok()?;
        re.captures(output_tail)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }
}

// ---------------------------------------------------------------------------
// Generic adapter (no session support)
// ---------------------------------------------------------------------------

pub struct GenericAdapter {
    command: String,
}

impl CliAdapter for GenericAdapter {
    fn name(&self) -> &str {
        &self.command
    }

    fn build_command(
        &self,
        work_dir: &Path,
        _session_id: Option<&str>,
        env_vars: &[(&str, &str)],
    ) -> Command {
        let mut cmd = Command::new(&self.command);
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        cmd
    }

    fn extract_session_id(&self, _output_tail: &str) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Return the appropriate CLI adapter for the given command.
pub fn adapter_for(command: &str) -> Box<dyn CliAdapter> {
    if is_claude_command(command) {
        Box::new(ClaudeAdapter)
    } else {
        Box::new(GenericAdapter {
            command: command.to_string(),
        })
    }
}

/// Check whether the command is the Claude Code CLI.
fn is_claude_command(command: &str) -> bool {
    let basename = Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(command);
    basename == "claude"
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_extract_session_id() {
        let adapter = ClaudeAdapter;

        let output = "\
Interactive session ended.

Resume this session with:
claude --resume b6578224-e8f1-4959-8644-20632f24eba8
";
        assert_eq!(
            adapter.extract_session_id(output),
            Some("b6578224-e8f1-4959-8644-20632f24eba8".to_string())
        );
    }

    #[test]
    fn test_claude_extract_no_match() {
        let adapter = ClaudeAdapter;
        assert_eq!(adapter.extract_session_id("no session here"), None);
    }

    #[test]
    fn test_claude_extract_with_ansi_noise() {
        let adapter = ClaudeAdapter;
        // The output buffer may contain ANSI escape codes around the text,
        // but the UUID itself should be clean.
        let output = "claude --resume a1b2c3d4-e5f6-7890-abcd-ef1234567890\r\n";
        assert_eq!(
            adapter.extract_session_id(output),
            Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string())
        );
    }

    #[test]
    fn test_generic_adapter_no_session() {
        let adapter = GenericAdapter {
            command: "vim".to_string(),
        };
        assert_eq!(
            adapter.extract_session_id("claude --resume aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            None
        );
    }

    #[test]
    fn test_is_claude_command() {
        assert!(is_claude_command("claude"));
        assert!(is_claude_command("/usr/bin/claude"));
        assert!(!is_claude_command("aider"));
        assert!(!is_claude_command("/usr/bin/vim"));
    }

    #[test]
    fn test_session_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let phantom_dir = dir.path();
        let agent_id = AgentId("test-agent".to_string());

        // Create the overlay directory structure.
        std::fs::create_dir_all(phantom_dir.join("overlays").join("test-agent")).unwrap();

        let session = CliSession {
            cli_name: "claude".to_string(),
            session_id: "b6578224-e8f1-4959-8644-20632f24eba8".to_string(),
            last_used: Utc::now(),
        };

        save_session(phantom_dir, &agent_id, &session).unwrap();
        let loaded = load_session(phantom_dir, &agent_id).unwrap();

        assert_eq!(loaded.cli_name, "claude");
        assert_eq!(loaded.session_id, "b6578224-e8f1-4959-8644-20632f24eba8");
    }

    #[test]
    fn test_load_session_missing() {
        let dir = tempfile::tempdir().unwrap();
        let agent_id = AgentId("no-such-agent".to_string());
        assert!(load_session(dir.path(), &agent_id).is_none());
    }
}

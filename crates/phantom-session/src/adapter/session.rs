//! Persistence of CLI sessions to disk so they can be resumed on subsequent
//! `phantom <agent>` invocations.
//!
//! The on-disk format is `overlays/<agent>/cli_session.json` — this file must
//! remain stable; breaking changes here would invalidate every resumable
//! session across existing users' repos.

use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, Utc};
use phantom_core::id::AgentId;
use serde::{Deserialize, Serialize};

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

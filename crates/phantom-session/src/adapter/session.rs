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
///
/// Rejects session ids whose shape doesn't match any expected format. The
/// value flows into `Command::args(["--resume", id])` for the coding CLI;
/// a crafted value inside `cli_session.json` would otherwise be handed to
/// the child process verbatim. We accept UUIDs (with optional dashes) and
/// the `ses_` prefix used by opencode.
pub fn load_session(phantom_dir: &Path, agent_id: &AgentId) -> Option<CliSession> {
    let path = session_path(phantom_dir, agent_id);
    let content = std::fs::read_to_string(&path).ok()?;
    let session: CliSession = serde_json::from_str(&content).ok()?;
    if !is_well_formed_session_id(&session.session_id) {
        tracing::warn!(
            path = %path.display(),
            session_id = %session.session_id,
            "ignoring stored CLI session with unrecognized session_id shape",
        );
        return None;
    }
    Some(session)
}

/// Allow only the formats currently emitted by supported adapters.
fn is_well_formed_session_id(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    // UUID with dashes.
    let is_uuid = s.len() == 36 && s.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
    if is_uuid {
        return true;
    }
    // UUID without dashes.
    let is_bare_hex = s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit());
    if is_bare_hex {
        return true;
    }
    // opencode `ses_…` prefix with alphanumeric/underscore/hyphen.
    if let Some(rest) = s.strip_prefix("ses_") {
        return !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    }
    false
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

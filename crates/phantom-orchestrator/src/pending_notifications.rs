//! Per-changeset notification queue for active session delivery.
//!
//! Ripple writes one file per materialized changeset into
//! `.phantom/overlays/<agent>/pending-notifications/<changeset-id>.json`.
//!
//! The `_notify-hook` CLI subcommand, invoked by Claude Code hooks
//! (`UserPromptSubmit`, `PostToolUse`, `SessionStart`), reads any files in this
//! directory, emits their rendered markdown as the next turn's
//! `hookSpecificOutput.additionalContext`, and moves the consumed files to
//! `pending-notifications/consumed/` for audit.
//!
//! This queue is intentionally separate from the pre-existing audit files
//! (`trunk-updated.json`, `.phantom-trunk-update.md`) so that:
//! - A CLI that does not support hooks still sees the markdown in its overlay.
//! - A CLI that does support hooks can drain pending notifications exactly
//!   once per turn without racing the audit-write path.
//!
//! See `/home/mael/.claude/plans/help-me-research-and-linear-lollipop.md` for
//! the full notification architecture.
//!
//! ## File format
//!
//! ```json
//! {
//!   "changeset_id": "<changeset-id>",
//!   "submitting_agent": "<agent-id>",
//!   "notification": { ...TrunkNotification ... },
//!   "summary_md": "# Trunk Update\n..."
//! }
//! ```

use std::path::{Path, PathBuf};

use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::notification::TrunkNotification;
use serde::{Deserialize, Serialize};

/// Queue directory name inside each agent overlay.
pub const QUEUE_DIR: &str = "pending-notifications";

/// Sub-directory that receives drained files for audit.
pub const CONSUMED_DIR: &str = "consumed";

/// Envelope serialised to `<queue>/<changeset-id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingNotification {
    /// Changeset that produced this notification.
    pub changeset_id: ChangesetId,
    /// Agent whose submit triggered the ripple.
    pub submitting_agent: AgentId,
    /// Structured notification payload.
    pub notification: TrunkNotification,
    /// Pre-rendered markdown summary (what the agent should see).
    pub summary_md: String,
}

/// Absolute path to the queue directory for an agent overlay.
#[must_use]
pub fn queue_dir(phantom_dir: &Path, agent_id: &AgentId) -> PathBuf {
    phantom_dir
        .join("overlays")
        .join(&agent_id.0)
        .join(QUEUE_DIR)
}

/// Absolute path to the consumed directory for an agent overlay.
#[must_use]
pub fn consumed_dir(phantom_dir: &Path, agent_id: &AgentId) -> PathBuf {
    queue_dir(phantom_dir, agent_id).join(CONSUMED_DIR)
}

/// Write a pending notification for an agent atomically.
///
/// Atomic via tmp-file + rename. Idempotent on `changeset_id` — a second
/// write for the same changeset overwrites the first (the latest enriched
/// notification wins).
///
/// Safe to call when the overlay directory does not yet exist: the queue
/// directory is created on demand. Caller-side errors are logged by the
/// ripple pipeline but never bubble up — queue-write failure must not
/// abort materialization.
pub fn write(
    phantom_dir: &Path,
    agent_id: &AgentId,
    payload: &PendingNotification,
) -> std::io::Result<()> {
    let dir = queue_dir(phantom_dir, agent_id);
    std::fs::create_dir_all(&dir)?;

    let final_path = dir.join(notification_filename(&payload.changeset_id));
    let tmp_path = dir.join(format!(
        ".{}.tmp",
        notification_filename(&payload.changeset_id)
    ));

    let json = serde_json::to_string_pretty(payload).map_err(std::io::Error::other)?;
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// List all unconsumed pending notifications for an agent, oldest first.
///
/// Returns an empty vector if the queue directory does not exist yet.
pub fn list(phantom_dir: &Path, agent_id: &AgentId) -> std::io::Result<Vec<PathBuf>> {
    let dir = queue_dir(phantom_dir, agent_id);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // Skip tmp sentinels and the consumed/ subdirectory.
        if name_str.starts_with('.') || !name_str.ends_with(".json") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        entries.push((modified, path));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(entries.into_iter().map(|(_, p)| p).collect())
}

/// Load a pending notification file.
pub fn load(path: &Path) -> std::io::Result<PendingNotification> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

/// Move a consumed file into `consumed/` as an audit trail.
///
/// Creates `consumed/` lazily. Propagates `NotFound` so a caller can treat
/// a concurrent consumer as a no-op. Falls back to copy+delete when
/// `rename` cannot cross filesystem boundaries — this preserves the
/// "delivered exactly once" guarantee even on exotic setups.
pub fn mark_consumed(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("notification path has no parent"))?;
    let consumed = parent.join(CONSUMED_DIR);
    std::fs::create_dir_all(&consumed)?;

    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("notification path has no file name"))?;
    let dest = consumed.join(name);

    match std::fs::rename(path, &dest) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(e),
        Err(_) => {
            // Cross-filesystem rename (EXDEV) or similar: fall back to
            // copy + delete so the source cannot be delivered twice.
            std::fs::copy(path, &dest)?;
            std::fs::remove_file(path)?;
            Ok(())
        }
    }
}

/// Filename for a changeset's notification entry.
fn notification_filename(changeset_id: &ChangesetId) -> String {
    // Changeset IDs are UUIDv7 strings — safe for filenames, but we still
    // sanitise any path separators defensively.
    let safe = changeset_id.0.replace(['/', '\\'], "_");
    format!("{safe}.json")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;
    use phantom_core::id::GitOid;
    use phantom_core::notification::{TrunkFileStatus, TrunkNotification};

    use super::*;

    fn sample_notification() -> TrunkNotification {
        TrunkNotification {
            new_commit: GitOid::zero(),
            timestamp: Utc::now(),
            files: vec![(PathBuf::from("src/lib.rs"), TrunkFileStatus::TrunkVisible)],
            dependency_impacts: vec![],
        }
    }

    fn sample_payload(cs: &str) -> PendingNotification {
        PendingNotification {
            changeset_id: ChangesetId(cs.to_string()),
            submitting_agent: AgentId("agent-a".into()),
            notification: sample_notification(),
            summary_md: "# Trunk Update\n".into(),
        }
    }

    #[test]
    fn write_creates_queue_dir_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());
        std::fs::create_dir_all(tmp.path().join("overlays/agent-b")).unwrap();

        write(tmp.path(), &agent_id, &sample_payload("cs-1")).unwrap();

        let path = queue_dir(tmp.path(), &agent_id).join("cs-1.json");
        assert!(path.exists(), "pending notification file should exist");
        let reloaded = load(&path).unwrap();
        assert_eq!(reloaded.changeset_id.0, "cs-1");
        assert_eq!(reloaded.submitting_agent.0, "agent-a");
    }

    #[test]
    fn write_is_idempotent_on_changeset_id() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());

        let mut first = sample_payload("cs-1");
        first.summary_md = "first".into();
        write(tmp.path(), &agent_id, &first).unwrap();

        let mut second = sample_payload("cs-1");
        second.summary_md = "second".into();
        write(tmp.path(), &agent_id, &second).unwrap();

        let listed = list(tmp.path(), &agent_id).unwrap();
        assert_eq!(listed.len(), 1);
        let reloaded = load(&listed[0]).unwrap();
        assert_eq!(reloaded.summary_md, "second");
    }

    #[test]
    fn list_skips_tmp_files_and_consumed_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());
        let dir = queue_dir(tmp.path(), &agent_id);
        std::fs::create_dir_all(dir.join(CONSUMED_DIR)).unwrap();

        // Tmp sentinel and a non-json noise file — both must be skipped.
        std::fs::write(dir.join(".cs-1.json.tmp"), "tmp").unwrap();
        std::fs::write(dir.join("notes.txt"), "noise").unwrap();
        // A consumed file inside consumed/ — must not appear in list.
        std::fs::write(dir.join(CONSUMED_DIR).join("old.json"), "{}").unwrap();

        write(tmp.path(), &agent_id, &sample_payload("cs-2")).unwrap();
        let listed = list(tmp.path(), &agent_id).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].file_name().unwrap() == "cs-2.json");
    }

    #[test]
    fn list_returns_empty_when_queue_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("never-started".into());
        let listed = list(tmp.path(), &agent_id).unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn mark_consumed_moves_file_into_consumed_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());

        write(tmp.path(), &agent_id, &sample_payload("cs-3")).unwrap();
        let path = queue_dir(tmp.path(), &agent_id).join("cs-3.json");
        assert!(path.exists());

        mark_consumed(&path).unwrap();
        assert!(!path.exists());
        let consumed_path = consumed_dir(tmp.path(), &agent_id).join("cs-3.json");
        assert!(consumed_path.exists(), "file should be in consumed/ now");
    }

    #[test]
    fn mark_consumed_is_tolerant_of_repeat_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());

        write(tmp.path(), &agent_id, &sample_payload("cs-4")).unwrap();
        let path = queue_dir(tmp.path(), &agent_id).join("cs-4.json");

        mark_consumed(&path).unwrap();
        // Second call: source already moved — should error (I/O not-found)
        // but critically must not panic. The hook caller treats NotFound as
        // a race (another process consumed it) and moves on.
        let err = mark_consumed(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn filename_sanitises_path_separators() {
        // Defensive: a malformed changeset id with a slash must not escape
        // the queue directory.
        let cs = ChangesetId("evil/../cs".into());
        let name = notification_filename(&cs);
        assert!(!name.contains('/'));
        assert!(!name.contains('\\'));
    }

    #[test]
    fn roundtrip_preserves_notification_content() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());
        let payload = sample_payload("cs-5");
        let original = payload.clone();

        write(tmp.path(), &agent_id, &payload).unwrap();
        let listed = list(tmp.path(), &agent_id).unwrap();
        let reloaded = load(&listed[0]).unwrap();

        assert_eq!(reloaded, original);
    }
}

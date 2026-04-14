//! Live rebase — auto-merge trunk changes into agent overlays.
//!
//! After a changeset materializes, [`rebase_agent`] performs a three-way merge
//! on each shadowed file in an agent's upper layer. Clean merges are written
//! atomically; conflicts are left untouched so the agent keeps its version.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use phantom_core::conflict::{ConflictDetail, ConflictKind};
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_core::traits::{MergeResult, SemanticAnalyzer};

use crate::error::OrchestratorError;
use crate::git::GitOps;

/// Summary of a live rebase operation for one agent.
#[derive(Debug)]
pub struct LiveRebaseResult {
    /// The agent whose overlay was rebased.
    pub agent_id: AgentId,
    /// The agent's base commit before the rebase.
    pub old_base: GitOid,
    /// The new trunk commit the agent is now based on.
    pub new_base: GitOid,
    /// Files that were cleanly merged into the agent's upper layer.
    pub merged: Vec<PathBuf>,
    /// Files that had conflicts — upper was left unchanged.
    pub conflicted: Vec<(PathBuf, Vec<ConflictDetail>)>,
}

/// Three-way merge each shadowed file and atomically update upper on success.
///
/// For each file in `shadowed_files`:
/// 1. Read the base version at `old_base`
/// 2. Read the trunk version at `new_head`
/// 3. Read the agent's version from `upper_dir`
/// 4. Run `analyzer.three_way_merge(base, trunk, agent, path)`
/// 5. On clean merge → atomically overwrite upper
/// 6. On conflict → leave upper unchanged, record in result
pub fn rebase_agent(
    git: &GitOps,
    analyzer: &dyn SemanticAnalyzer,
    agent_id: &AgentId,
    old_base: &GitOid,
    new_head: &GitOid,
    upper_dir: &Path,
    shadowed_files: &[PathBuf],
) -> Result<LiveRebaseResult, OrchestratorError> {
    let mut merged = Vec::new();
    let mut conflicted = Vec::new();

    for file in shadowed_files {
        let theirs_path = upper_dir.join(file);

        // Read agent's version from upper layer.
        let theirs = match std::fs::read(&theirs_path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Agent deleted this file (shouldn't be classified as Shadowed, but
                // handle defensively).
                debug!(file = %file.display(), "skipping — not in upper");
                continue;
            }
            Err(e) => return Err(OrchestratorError::Io(e)),
        };

        // Read base version (at the agent's old base commit).
        let base = match git.read_file_at_commit(old_base, file) {
            Ok(content) => Some(content),
            Err(OrchestratorError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };

        // Read trunk's current version.
        let ours = match git.read_file_at_commit(new_head, file) {
            Ok(content) => content,
            Err(OrchestratorError::NotFound(_)) => {
                // Trunk deleted this file — conflict (modify-delete).
                warn!(
                    agent = %agent_id,
                    file = %file.display(),
                    "trunk deleted file that agent modified"
                );
                conflicted.push((
                    file.clone(),
                    vec![ConflictDetail {
                        kind: ConflictKind::ModifyDeleteSymbol,
                        file: file.clone(),
                        symbol_id: None,
                        ours_changeset: ChangesetId("trunk".into()),
                        theirs_changeset: ChangesetId(format!("overlay-{agent_id}")),
                        description: format!(
                            "file {} was deleted on trunk but modified by agent",
                            file.display()
                        ),
                        ours_span: None,
                        theirs_span: None,
                        base_span: None,
                    }],
                ));
                continue;
            }
            Err(e) => return Err(e),
        };

        match base {
            None => {
                // File didn't exist at base — both trunk and agent added it.
                let result = analyzer
                    .three_way_merge(&[], &ours, &theirs, file)
                    .map_err(|e| OrchestratorError::Semantic(e.to_string()))?;
                match result {
                    MergeResult::Clean(content) => {
                        atomic_write_upper(upper_dir, file, &content)?;
                        merged.push(file.clone());
                    }
                    MergeResult::Conflict(details) => {
                        conflicted.push((file.clone(), details));
                    }
                }
            }
            Some(base_content) => {
                // Defensive: if trunk hasn't actually changed this file, skip.
                if ours == base_content {
                    debug!(file = %file.display(), "trunk unchanged — skipping");
                    continue;
                }

                let result = analyzer
                    .three_way_merge(&base_content, &ours, &theirs, file)
                    .map_err(|e| OrchestratorError::Semantic(e.to_string()))?;
                match result {
                    MergeResult::Clean(content) => {
                        atomic_write_upper(upper_dir, file, &content)?;
                        merged.push(file.clone());
                    }
                    MergeResult::Conflict(details) => {
                        conflicted.push((file.clone(), details));
                    }
                }
            }
        }
    }

    info!(
        agent = %agent_id,
        merged = merged.len(),
        conflicted = conflicted.len(),
        "live rebase complete"
    );

    Ok(LiveRebaseResult {
        agent_id: agent_id.clone(),
        old_base: *old_base,
        new_base: *new_head,
        merged,
        conflicted,
    })
}

/// Atomically write content to a file in the upper directory.
///
/// Writes to a temporary sibling file, then renames over the target. On Unix,
/// rename within the same filesystem is atomic, preventing partial reads.
fn atomic_write_upper(
    upper_dir: &Path,
    rel_path: &Path,
    content: &[u8],
) -> Result<(), OrchestratorError> {
    let target = upper_dir.join(rel_path);
    let tmp = target.with_extension("phantom-rebase-tmp");

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, &target)?;

    Ok(())
}

/// Read the persisted `current_base` commit for an agent.
///
/// Returns `None` if the file does not exist (pre-existing agent that predates
/// live rebase). The file contains a 40-character hex OID.
pub fn read_current_base(
    phantom_dir: &Path,
    agent_id: &AgentId,
) -> Result<Option<GitOid>, OrchestratorError> {
    let path = phantom_dir
        .join("overlays")
        .join(&agent_id.0)
        .join("current_base");

    match std::fs::read_to_string(&path) {
        Ok(hex) => {
            let hex = hex.trim();
            if hex.len() != 40 {
                return Err(OrchestratorError::LiveRebase(format!(
                    "invalid current_base for {agent_id}: expected 40 hex chars, got {}",
                    hex.len()
                )));
            }
            let mut bytes = [0u8; 20];
            for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
                let high = hex_nibble(chunk[0]).ok_or_else(|| {
                    OrchestratorError::LiveRebase(format!(
                        "invalid hex in current_base for {agent_id}"
                    ))
                })?;
                let low = hex_nibble(chunk[1]).ok_or_else(|| {
                    OrchestratorError::LiveRebase(format!(
                        "invalid hex in current_base for {agent_id}"
                    ))
                })?;
                bytes[i] = (high << 4) | low;
            }
            Ok(Some(GitOid::from_bytes(bytes)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(OrchestratorError::Io(e)),
    }
}

/// Persist `current_base` for an agent atomically (write .tmp + rename).
pub fn write_current_base(
    phantom_dir: &Path,
    agent_id: &AgentId,
    base: &GitOid,
) -> Result<(), OrchestratorError> {
    let dir = phantom_dir.join("overlays").join(&agent_id.0);
    std::fs::create_dir_all(&dir)?;

    let path = dir.join("current_base");
    let tmp = dir.join("current_base.tmp");

    std::fs::write(&tmp, base.to_hex())?;
    std::fs::rename(&tmp, &path)?;

    Ok(())
}

/// Convert a hex ASCII byte to its 4-bit value.
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "live_rebase_tests.rs"]
mod tests;

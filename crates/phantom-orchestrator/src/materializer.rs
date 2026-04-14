//! Changeset materialization — applying a changeset to trunk atomically.
//!
//! The [`Materializer`] coordinates git operations, semantic analysis, and
//! event logging to commit an agent's changeset to the shared trunk. It
//! handles three scenarios:
//!
//! 1. **Direct apply** — trunk hasn't advanced since the agent started; changes
//!    are committed without merging.
//! 2. **Clean merge** — trunk advanced but all changed files merge cleanly at
//!    the semantic level.
//! 3. **Conflict** — one or more files have symbol-level conflicts; the
//!    changeset is rejected and a conflict event is recorded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::debug;

use phantom_core::changeset::Changeset;
use phantom_core::conflict::ConflictDetail;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{EventId, GitOid};
use phantom_core::traits::{EventStore, MergeResult, SemanticAnalyzer};

use crate::error::OrchestratorError;
use crate::git::{self, GitOps};

/// Result of a materialization attempt.
#[derive(Debug)]
pub enum MaterializeResult {
    /// The changeset was successfully committed to trunk.
    Success {
        /// The new trunk commit OID.
        new_commit: GitOid,
        /// Files that were merged via line-based text fallback because no
        /// tree-sitter grammar is available for their language. These files
        /// had no syntax validation after merging.
        text_fallback_files: Vec<PathBuf>,
    },
    /// The changeset had conflicts and was not committed.
    Conflict {
        /// Details of each conflict found.
        details: Vec<ConflictDetail>,
    },
}

/// Coordinates changeset materialization to trunk.
pub struct Materializer {
    git: GitOps,
}

impl Materializer {
    /// Create a materializer backed by the given git operations handle.
    pub fn new(git: GitOps) -> Self {
        Self { git }
    }

    /// Borrow the inner `GitOps` for inspection.
    pub fn git(&self) -> &GitOps {
        &self.git
    }

    /// Attempt to materialize a changeset to trunk.
    ///
    /// `upper_dir` is the agent's overlay upper directory containing modified
    /// files. The materializer reads agent changes from there, runs semantic
    /// merge checks if trunk has advanced, and either commits the result or
    /// reports conflicts.
    pub async fn materialize(
        &self,
        changeset: &Changeset,
        upper_dir: &Path,
        event_store: &dyn EventStore,
        analyzer: &dyn SemanticAnalyzer,
        message: &str,
    ) -> Result<MaterializeResult, OrchestratorError> {
        let head = self.git.head_oid()?;
        let trunk_path = self
            .git
            .repo()
            .workdir()
            .ok_or_else(|| {
                OrchestratorError::NotFound("repository has no working directory".into())
            })?
            .to_path_buf();

        if head == changeset.base_commit {
            return self
                .direct_apply(changeset, upper_dir, &head, message, event_store)
                .await;
        }

        let ctx = MergeContext {
            upper_dir,
            trunk_path: &trunk_path,
            head: &head,
            message,
            event_store,
            analyzer,
        };
        self.merge_apply(changeset, &ctx).await
    }

    /// Fast path: trunk hasn't moved, apply overlay directly.
    ///
    /// Reads overlay files into memory and commits via the git object database
    /// (blobs → tree → commit) without copying files into the working tree.
    /// This eliminates the TOCTOU window that the old `commit_overlay_changes`
    /// approach had.
    async fn direct_apply(
        &self,
        changeset: &Changeset,
        upper_dir: &Path,
        head: &GitOid,
        message: &str,
        event_store: &dyn EventStore,
    ) -> Result<MaterializeResult, OrchestratorError> {
        debug!(changeset = %changeset.id, "direct apply — trunk has not advanced");

        let file_oids = git::create_blobs_from_overlay(self.git.repo(), upper_dir)?;
        let new_commit =
            self.commit_from_oids(&file_oids, head, message, &changeset.agent_id.0)?;

        // Update working tree to match the new commit (best-effort).
        if let Err(e) = self
            .git
            .repo()
            .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        {
            debug!(error = %e, "checkout_head after direct apply failed (non-fatal)");
        }

        self.append_materialized_event(changeset, &new_commit, event_store)
            .await?;

        Ok(MaterializeResult::Success {
            new_commit,
            text_fallback_files: vec![],
        })
    }

    /// Slow path: trunk advanced, run three-way semantic merge per file.
    async fn merge_apply(
        &self,
        changeset: &Changeset,
        ctx: &MergeContext<'_>,
    ) -> Result<MaterializeResult, OrchestratorError> {
        debug!(
            changeset = %changeset.id,
            base = %changeset.base_commit,
            head = %ctx.head,
            "trunk advanced — running semantic merge"
        );

        let mut all_conflicts = Vec::new();
        let mut merged_files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
        let mut text_fallback_files: Vec<PathBuf> = Vec::new();

        let agent_ops_by_file = group_ops_by_file(&changeset.operations);

        for file in &changeset.files_touched {
            self.validate_path(file, ctx.trunk_path)?;

            let agent_file_ops = agent_ops_by_file.get(file);
            match self.merge_single_file(file, changeset, ctx, agent_file_ops)? {
                MergeFileOutcome::Merged {
                    content,
                    text_fallback,
                } => {
                    if text_fallback {
                        text_fallback_files.push(file.clone());
                    }
                    merged_files.push((file.clone(), content));
                }
                MergeFileOutcome::Conflicted(conflicts) => {
                    all_conflicts.extend(conflicts);
                }
                MergeFileOutcome::Skipped => {}
            }
        }

        if !all_conflicts.is_empty() {
            self.append_conflicted_event(changeset, &all_conflicts, ctx.event_store)
                .await?;
            return Ok(MaterializeResult::Conflict {
                details: all_conflicts,
            });
        }

        let merged_oids = git::create_blobs_from_content(self.git.repo(), &merged_files)?;
        let new_commit =
            self.commit_from_oids(&merged_oids, ctx.head, ctx.message, &changeset.agent_id.0)?;

        if let Err(e) = self
            .git
            .repo()
            .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        {
            debug!(error = %e, "checkout_head after merge commit failed (non-fatal)");
        }

        if !text_fallback_files.is_empty() {
            tracing::warn!(
                changeset = %changeset.id,
                files = ?text_fallback_files,
                "materialized with {} file(s) merged via line-based text fallback (no syntax validation)",
                text_fallback_files.len()
            );
        }

        self.append_materialized_event(changeset, &new_commit, ctx.event_store)
            .await?;

        Ok(MaterializeResult::Success {
            new_commit,
            text_fallback_files,
        })
    }

    /// Merge a single file during the merge-apply path.
    ///
    /// Handles: reading agent/base/trunk versions, new-file vs existing-file
    /// logic, symbol-disjoint text merge optimization, and semantic three-way merge.
    fn merge_single_file(
        &self,
        file: &Path,
        changeset: &Changeset,
        ctx: &MergeContext<'_>,
        agent_file_ops: Option<&HashSet<String>>,
    ) -> Result<MergeFileOutcome, OrchestratorError> {
        let theirs_path = ctx.upper_dir.join(file);

        // Read agent's version from overlay.
        let theirs = if theirs_path.exists() {
            std::fs::read(&theirs_path)?
        } else {
            return Ok(MergeFileOutcome::Skipped);
        };

        // Read base version.
        let base = match self.git.read_file_at_commit(&changeset.base_commit, file) {
            Ok(content) => Some(content),
            Err(OrchestratorError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };

        match base {
            None => self.merge_new_file(file, ctx, &theirs),
            Some(base_content) => {
                self.merge_existing_file(file, changeset, ctx, &base_content, &theirs, agent_file_ops)
            }
        }
    }

    /// Handle merge of a file that didn't exist at the base commit.
    fn merge_new_file(
        &self,
        file: &Path,
        ctx: &MergeContext<'_>,
        theirs: &[u8],
    ) -> Result<MergeFileOutcome, OrchestratorError> {
        match self.git.read_file_at_commit(ctx.head, file) {
            Ok(ours) => {
                // File added on trunk too — merge with empty base.
                let result = ctx
                    .analyzer
                    .three_way_merge(&[], &ours, theirs, file)
                    .map_err(|e| OrchestratorError::Semantic(e.to_string()))?;
                Ok(merge_result_to_outcome(result, file, ctx.analyzer))
            }
            Err(OrchestratorError::NotFound(_)) => {
                // New file not on trunk either — just add it.
                Ok(MergeFileOutcome::Merged {
                    content: theirs.to_vec(),
                    text_fallback: false,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Handle merge of a file that existed at the base commit.
    fn merge_existing_file(
        &self,
        file: &Path,
        changeset: &Changeset,
        ctx: &MergeContext<'_>,
        base_content: &[u8],
        theirs: &[u8],
        agent_file_ops: Option<&HashSet<String>>,
    ) -> Result<MergeFileOutcome, OrchestratorError> {
        // Read trunk's current version.
        let ours = match self.git.read_file_at_commit(ctx.head, file) {
            Ok(content) => content,
            Err(OrchestratorError::NotFound(_)) => {
                // File deleted on trunk since base.
                return Ok(MergeFileOutcome::Conflicted(vec![ConflictDetail {
                    kind: phantom_core::conflict::ConflictKind::ModifyDeleteSymbol,
                    file: file.to_path_buf(),
                    symbol_id: None,
                    ours_changeset: phantom_core::id::ChangesetId("trunk".into()),
                    theirs_changeset: changeset.id.clone(),
                    description: format!(
                        "file {} was deleted on trunk but modified by agent",
                        file.display()
                    ),
                    ours_span: None,
                    theirs_span: None,
                    base_span: None,
                }]));
            }
            Err(e) => return Err(e),
        };

        // Trunk unchanged from base — use agent's version directly.
        if ours == base_content {
            return Ok(MergeFileOutcome::Merged {
                content: theirs.to_vec(),
                text_fallback: false,
            });
        }

        // Symbol-level disjoint check: skip expensive semantic merge when
        // agent and trunk modified different symbols.
        let symbols_disjoint = match agent_file_ops {
            Some(agent_syms) if !agent_syms.is_empty() => {
                let base_syms = ctx
                    .analyzer
                    .extract_symbols(file, base_content)
                    .unwrap_or_default();
                let trunk_syms = ctx
                    .analyzer
                    .extract_symbols(file, &ours)
                    .unwrap_or_default();
                let trunk_ops = ctx.analyzer.diff_symbols(&base_syms, &trunk_syms);
                let trunk_names: HashSet<String> = trunk_ops
                    .iter()
                    .filter_map(|op| op.symbol_name().map(String::from))
                    .collect();
                !agent_syms.iter().any(|s| trunk_names.contains(s.as_str()))
            }
            _ => false,
        };

        if symbols_disjoint {
            debug!(file = %file.display(), "symbol-disjoint — using text merge");
            if let MergeResult::Clean(content) =
                self.git.text_merge(base_content, &ours, theirs)?
            {
                return Ok(MergeFileOutcome::Merged {
                    content,
                    text_fallback: !ctx.analyzer.supports_language(file),
                });
            }
            debug!(
                file = %file.display(),
                "text merge conflict despite disjoint symbols, falling back to semantic merge"
            );
        }

        // Three-way semantic merge.
        let result = ctx
            .analyzer
            .three_way_merge(base_content, &ours, theirs, file)
            .map_err(|e| OrchestratorError::Semantic(e.to_string()))?;

        Ok(merge_result_to_outcome(result, file, ctx.analyzer))
    }

    /// Validate that a relative path does not escape the trunk directory.
    fn validate_path(&self, file: &Path, trunk_path: &Path) -> Result<(), OrchestratorError> {
        // Reject absolute paths — joining an absolute path replaces the base entirely
        if file.is_absolute() {
            return Err(OrchestratorError::MaterializationFailed(format!(
                "path must be relative, got absolute: {}",
                file.display()
            )));
        }

        // Reject paths with parent traversal components
        for component in file.components() {
            if matches!(component, std::path::Component::ParentDir) {
                return Err(OrchestratorError::MaterializationFailed(format!(
                    "path contains parent traversal (..): {}",
                    file.display()
                )));
            }
        }

        // Final check: the joined path must still start with trunk_path
        let joined = trunk_path.join(file);
        if !joined.starts_with(trunk_path) {
            return Err(OrchestratorError::MaterializationFailed(format!(
                "path escapes working tree: {}",
                file.display()
            )));
        }

        Ok(())
    }

    /// Build a commit from pre-created blob OIDs without touching the
    /// working tree.
    ///
    /// Memory-efficient counterpart to [`commit_from_content`]: blobs are
    /// already created, so this only builds the tree and commit objects.
    fn commit_from_oids(
        &self,
        file_oids: &[(PathBuf, git2::Oid)],
        parent_oid: &GitOid,
        message: &str,
        author: &str,
    ) -> Result<GitOid, OrchestratorError> {
        let repo = self.git.repo();
        let git2_parent_oid = git::git_oid_to_oid(parent_oid)?;
        let parent = repo.find_commit(git2_parent_oid)?;
        let base_tree = parent.tree()?;

        let new_tree_oid = git::build_tree_from_oids(repo, &base_tree, file_oids)?;
        let new_tree = repo.find_tree(new_tree_oid)?;

        let sig = git2::Signature::now(author, &format!("{author}@phantom"))?;
        let new_oid = repo.commit(Some("HEAD"), &sig, &sig, message, &new_tree, &[&parent])?;

        Ok(git::oid_to_git_oid(new_oid))
    }

    /// Append a `ChangesetMaterialized` event to the store.
    async fn append_materialized_event(
        &self,
        changeset: &Changeset,
        new_commit: &GitOid,
        event_store: &dyn EventStore,
    ) -> Result<(), OrchestratorError> {
        let event = Event {
            id: EventId(0), // assigned by store
            timestamp: Utc::now(),
            changeset_id: changeset.id.clone(),
            agent_id: changeset.agent_id.clone(),
            kind: EventKind::ChangesetMaterialized {
                new_commit: *new_commit,
            },
        };
        event_store
            .append(event)
            .await
            .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;
        Ok(())
    }

    /// Append a `ChangesetConflicted` event to the store.
    async fn append_conflicted_event(
        &self,
        changeset: &Changeset,
        conflicts: &[ConflictDetail],
        event_store: &dyn EventStore,
    ) -> Result<(), OrchestratorError> {
        let event = Event {
            id: EventId(0), // assigned by store
            timestamp: Utc::now(),
            changeset_id: changeset.id.clone(),
            agent_id: changeset.agent_id.clone(),
            kind: EventKind::ChangesetConflicted {
                conflicts: conflicts.to_vec(),
            },
        };
        event_store
            .append(event)
            .await
            .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;
        Ok(())
    }
}

use crate::ops::group_ops_by_file;

/// Outcome of merging a single file during the merge-apply path.
enum MergeFileOutcome {
    /// File merged cleanly with this content.
    Merged { content: Vec<u8>, text_fallback: bool },
    /// File produced conflicts.
    Conflicted(Vec<ConflictDetail>),
    /// File was skipped (deleted or whiteout).
    Skipped,
}

/// Convert a [`MergeResult`] into a [`MergeFileOutcome`], tracking text fallback.
fn merge_result_to_outcome(
    result: MergeResult,
    file: &Path,
    analyzer: &dyn SemanticAnalyzer,
) -> MergeFileOutcome {
    match result {
        MergeResult::Clean(content) => MergeFileOutcome::Merged {
            text_fallback: !analyzer.supports_language(file),
            content,
        },
        MergeResult::Conflict(conflicts) => MergeFileOutcome::Conflicted(conflicts),
    }
}

/// Bundled context for a merge-apply operation, avoiding excessive parameter counts.
struct MergeContext<'a> {
    upper_dir: &'a Path,
    trunk_path: &'a Path,
    head: &'a GitOid,
    message: &'a str,
    event_store: &'a dyn EventStore,
    analyzer: &'a dyn SemanticAnalyzer,
}

#[cfg(test)]
#[path = "materializer_tests.rs"]
mod tests;

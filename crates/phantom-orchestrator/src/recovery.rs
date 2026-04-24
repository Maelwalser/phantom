//! Crash recovery for the ghost-commit protocol.
//!
//! When [`crate::materializer`] emits a [`EventKind::ChangesetMaterializationStarted`]
//! fence event before touching git, a crash between the fence and the terminal
//! event (`ChangesetMaterialized` / `ChangesetConflicted` / `ChangesetDropped`)
//! leaves the intent recorded but the outcome ambiguous. This module walks the
//! orphan fences and reconciles each one against trunk HEAD:
//!
//! - **Commit landed** (HEAD contains a commit whose parent matches the fence's
//!   declared parent and whose author is the fence's agent): append the missing
//!   [`EventKind::ChangesetMaterialized`] so the event log matches reality.
//! - **Commit did not land**: append [`EventKind::ChangesetDropped`] with a
//!   reason that points back at the fence event.
//!
//! Recovery is idempotent by construction — after it runs, every fence has a
//! terminal event and a re-run finds no orphans.
//!
//! The reconciliation lookup is bounded (`RECONSTRUCT_SCAN_DEPTH` commits back
//! from HEAD) so a long-running trunk without `ph recover` for ages cannot
//! turn one stale fence into an unbounded git walk.

use chrono::Utc;
use tracing::{info, warn};

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::{OrphanFence, ReplayEngine, SqliteEventStore};

use crate::error::OrchestratorError;
use crate::git::{GitOps, oid_to_git_oid};

/// How far back along the first-parent chain `ph recover` will search for a
/// commit matching an orphan fence. Fences older than this are treated as
/// aborted even if a matching commit exists — users would have to run `git
/// log` themselves at that point anyway.
const RECONSTRUCT_SCAN_DEPTH: usize = 128;

/// Outcome of [`reconcile_orphan_fences`]: which fences were reconciled by
/// reconstructing the missing terminal, and which were marked dropped.
#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// Fences whose commits were found on trunk. The missing
    /// `ChangesetMaterialized` event has been appended for each.
    pub reconstructed: Vec<ReconstructedFence>,
    /// Fences whose commits were not found. A `ChangesetDropped` event has
    /// been appended for each.
    pub aborted: Vec<AbortedFence>,
}

impl RecoveryReport {
    /// Total number of fences that were reconciled.
    #[must_use]
    pub fn total(&self) -> usize {
        self.reconstructed.len() + self.aborted.len()
    }
}

/// A fence whose git commit was located on trunk, and whose missing
/// `ChangesetMaterialized` event has now been appended.
#[derive(Debug, Clone)]
pub struct ReconstructedFence {
    pub changeset_id: ChangesetId,
    pub fence_event_id: EventId,
    pub new_commit: GitOid,
}

/// A fence whose git commit was not on trunk, now terminated by a
/// `ChangesetDropped` event.
#[derive(Debug, Clone)]
pub struct AbortedFence {
    pub changeset_id: ChangesetId,
    pub fence_event_id: EventId,
    pub parent: GitOid,
}

/// Scan for orphan fences and reconcile each one.
///
/// Safe to call at any time — on a healthy repo the orphan list is empty and
/// this returns an empty report. Callers (typically `ph recover`) should
/// serialize with in-flight submits; concurrently reconciling a fence while
/// the materializer is actively finalizing it would double-append the
/// terminal event. The submit path's materialize lock already blocks new
/// submits during recovery as long as recovery runs outside the lock.
pub async fn reconcile_orphan_fences(
    git: &GitOps,
    event_store: &SqliteEventStore,
) -> Result<RecoveryReport, OrchestratorError> {
    let engine = ReplayEngine::new(event_store);
    let orphans = engine
        .orphan_materialization_fences()
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;

    let mut report = RecoveryReport::default();
    if orphans.is_empty() {
        return Ok(report);
    }

    let head = git.head_oid()?;

    for orphan in orphans {
        if let Some(new_commit) = locate_commit_for_fence(git, &orphan, &head) {
            append_reconstructed_materialized(event_store, &orphan, new_commit).await?;
            info!(
                changeset = %orphan.changeset_id,
                fence_event = %orphan.fence_event_id,
                new_commit = %new_commit,
                "reconstructed missing ChangesetMaterialized from git HEAD"
            );
            report.reconstructed.push(ReconstructedFence {
                changeset_id: orphan.changeset_id,
                fence_event_id: orphan.fence_event_id,
                new_commit,
            });
        } else {
            append_aborted_dropped(event_store, &orphan).await?;
            warn!(
                changeset = %orphan.changeset_id,
                fence_event = %orphan.fence_event_id,
                parent = %orphan.parent,
                "no matching commit on trunk; marking fence aborted"
            );
            report.aborted.push(AbortedFence {
                changeset_id: orphan.changeset_id,
                fence_event_id: orphan.fence_event_id,
                parent: orphan.parent,
            });
        }
    }

    Ok(report)
}

/// Walk HEAD's first-parent chain looking for a commit whose parent matches
/// the fence's declared parent *and* whose author matches the fence agent.
/// Returns `Some(oid)` on a match, `None` if no match is found within
/// [`RECONSTRUCT_SCAN_DEPTH`] commits.
fn locate_commit_for_fence(git: &GitOps, fence: &OrphanFence, head: &GitOid) -> Option<GitOid> {
    if *head == GitOid::zero() {
        // Unborn HEAD — no commits could possibly match.
        return None;
    }

    let mut current_oid = crate::git::git_oid_to_oid(head).ok()?;
    for _ in 0..RECONSTRUCT_SCAN_DEPTH {
        let Ok(commit) = git.repo().find_commit(current_oid) else {
            return None;
        };

        // A commit with no parents cannot match — a fence always records a
        // concrete `parent` that the new commit was supposed to sit on top
        // of.
        let first_parent_id = commit.parent_ids().next()?;
        let first_parent = oid_to_git_oid(first_parent_id);

        if first_parent == fence.parent && commit_authored_by(&commit, &fence.agent_id.0) {
            return Some(oid_to_git_oid(commit.id()));
        }

        current_oid = first_parent_id;
    }
    None
}

fn commit_authored_by(commit: &git2::Commit<'_>, agent_id: &str) -> bool {
    commit
        .author()
        .name()
        .is_some_and(|name| name == agent_id)
}

async fn append_reconstructed_materialized(
    event_store: &SqliteEventStore,
    fence: &OrphanFence,
    new_commit: GitOid,
) -> Result<(), OrchestratorError> {
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: fence.changeset_id.clone(),
        agent_id: fence.agent_id.clone(),
        causal_parent: Some(fence.fence_event_id),
        kind: EventKind::ChangesetMaterialized { new_commit },
    };
    event_store
        .append(event)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;
    Ok(())
}

async fn append_aborted_dropped(
    event_store: &SqliteEventStore,
    fence: &OrphanFence,
) -> Result<(), OrchestratorError> {
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: fence.changeset_id.clone(),
        agent_id: fence.agent_id.clone(),
        causal_parent: Some(fence.fence_event_id),
        kind: EventKind::ChangesetDropped {
            reason: format!(
                "MaterializationAborted: fence event {} had no matching trunk commit",
                fence.fence_event_id.0
            ),
        },
    };
    event_store
        .append(event)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use phantom_core::event::MaterializationPath;
    use phantom_core::id::AgentId;

    use crate::test_support::init_repo;

    async fn open_store() -> SqliteEventStore {
        SqliteEventStore::in_memory().await.unwrap()
    }

    async fn append_fence(
        store: &SqliteEventStore,
        changeset: &str,
        agent: &str,
        parent: GitOid,
        path: MaterializationPath,
    ) -> EventId {
        store
            .append(Event {
                id: EventId(0),
                timestamp: Utc::now(),
                changeset_id: ChangesetId(changeset.into()),
                agent_id: AgentId(agent.into()),
                causal_parent: None,
                kind: EventKind::ChangesetMaterializationStarted { parent, path },
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn empty_report_when_no_orphans() {
        let (_dir, git) = init_repo(&[("a.txt", b"hi")]);
        let store = open_store().await;
        let report = reconcile_orphan_fences(&git, &store).await.unwrap();
        assert_eq!(report.total(), 0);
    }

    #[tokio::test]
    async fn orphan_with_matching_trunk_commit_reconstructs() {
        // Set up: init a repo, record what HEAD was, then advance trunk via an
        // agent-authored commit. That mimics "commit landed, terminal event
        // was never written."
        let (_dir, git) = init_repo(&[("a.txt", b"hi")]);
        let fence_parent = git.head_oid().unwrap();

        // Commit an "agent" commit on top. advance_trunk uses a test author;
        // we need the commit author to match the fence agent for recovery to
        // accept it.
        let expected_commit = advance_trunk_as(&git, "agent-a", "a.txt", b"bye");

        let store = open_store().await;
        let fence_id = append_fence(
            &store,
            "cs-1",
            "agent-a",
            fence_parent,
            MaterializationPath::Direct,
        )
        .await;

        let report = reconcile_orphan_fences(&git, &store).await.unwrap();

        assert_eq!(report.reconstructed.len(), 1);
        assert_eq!(report.aborted.len(), 0);
        assert_eq!(report.reconstructed[0].fence_event_id, fence_id);
        assert_eq!(report.reconstructed[0].new_commit, expected_commit);

        // Idempotency: a second run should find no orphans.
        let second = reconcile_orphan_fences(&git, &store).await.unwrap();
        assert_eq!(second.total(), 0);
    }

    #[tokio::test]
    async fn orphan_without_matching_commit_is_marked_dropped() {
        // Trunk does NOT advance after the fence — the commit never landed.
        let (_dir, git) = init_repo(&[("a.txt", b"hi")]);
        let parent = git.head_oid().unwrap();

        let store = open_store().await;
        let fence_id = append_fence(
            &store,
            "cs-1",
            "agent-a",
            parent,
            MaterializationPath::Direct,
        )
        .await;

        let report = reconcile_orphan_fences(&git, &store).await.unwrap();

        assert_eq!(report.aborted.len(), 1);
        assert_eq!(report.reconstructed.len(), 0);
        assert_eq!(report.aborted[0].fence_event_id, fence_id);

        // Verify the ChangesetDropped event actually landed with the
        // expected reason format.
        let all = store.query_all().await.unwrap();
        let dropped = all
            .iter()
            .find(|e| matches!(e.kind, EventKind::ChangesetDropped { .. }))
            .expect("ChangesetDropped must be appended");
        let EventKind::ChangesetDropped { reason } = &dropped.kind else {
            unreachable!();
        };
        assert!(reason.contains("MaterializationAborted"));
        assert!(reason.contains(&fence_id.0.to_string()));
        assert_eq!(dropped.causal_parent, Some(fence_id));
    }

    #[tokio::test]
    async fn commit_by_different_agent_does_not_match_fence() {
        // Trunk advanced, but by a different agent — recovery must not
        // blindly attribute someone else's commit to our orphan fence.
        let (_dir, git) = init_repo(&[("a.txt", b"hi")]);
        let parent = git.head_oid().unwrap();
        let _other_commit = advance_trunk_as(&git, "agent-b", "a.txt", b"bye");

        let store = open_store().await;
        append_fence(
            &store,
            "cs-1",
            "agent-a",
            parent,
            MaterializationPath::Direct,
        )
        .await;

        let report = reconcile_orphan_fences(&git, &store).await.unwrap();
        assert_eq!(report.reconstructed.len(), 0);
        assert_eq!(report.aborted.len(), 1);
    }

    /// Commit a single file on top of HEAD with a given author name, and
    /// return the new commit's OID. Mirrors `advance_trunk` but lets the
    /// caller control the author so recovery tests can pin agent matching.
    fn advance_trunk_as(git: &GitOps, author: &str, path: &str, content: &[u8]) -> GitOid {
        use std::io::Write;
        let repo = git.repo();
        let workdir = repo.workdir().unwrap();
        let file = workdir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all(content).unwrap();

        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new(path))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let sig = git2::Signature::now(author, &format!("{author}@phantom.test")).unwrap();
        let parent_commit = repo
            .find_commit(repo.head().unwrap().target().unwrap())
            .unwrap();
        let new_oid = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "test advance",
                &tree,
                &[&parent_commit],
            )
            .unwrap();
        oid_to_git_oid(new_oid)
    }

}

//! Live rebase — auto-merge trunk changes into agent overlays.
//!
//! After a changeset materializes, [`rebase_agent`] performs a three-way merge
//! on each shadowed file in an agent's upper layer. Clean merges are written
//! atomically; conflicts are left untouched so the agent keeps its version.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use phantom_core::conflict::{ConflictDetail, ConflictKind, MergeResult};
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_core::traits::SemanticAnalyzer;

use crate::error::OrchestratorError;
use crate::git::{GitError, GitOps};

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
            Err(GitError::NotFound(_)) => None,
            Err(e) => return Err(e.into()),
        };

        // Read trunk's current version.
        let ours = match git.read_file_at_commit(new_head, file) {
            Ok(content) => content,
            Err(GitError::NotFound(_)) => {
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
            Err(e) => return Err(e.into()),
        };

        match base {
            None => {
                // File didn't exist at base — both trunk and agent added it.
                let result = analyzer
                    .three_way_merge(&[], &ours, &theirs, file)
                    .map_err(|e| OrchestratorError::Semantic(e.to_string()))?;
                match result.result {
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
                match result.result {
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
    if *base == GitOid::zero() {
        return Err(OrchestratorError::LiveRebase(format!(
            "refusing to persist null OID as current_base for {agent_id}: repository has no initial commit"
        )));
    }

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
mod tests {
    use super::*;

    use phantom_core::conflict::{ConflictKind, MergeResult};
    use std::path::PathBuf;
    use tempfile::TempDir;

    use crate::test_support::commit_file;

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    /// Create a git repo with an empty initial commit in the given directory.
    fn init_repo(dir: &Path) -> GitOps {
        let repo = git2::Repository::init(dir).unwrap();

        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        drop(tree);
        drop(repo);

        GitOps::open(dir).unwrap()
    }

    /// A test analyzer that delegates to the text-based merge in git.rs.
    struct TextMergeAnalyzer;

    impl SemanticAnalyzer for TextMergeAnalyzer {
        fn extract_symbols(
            &self,
            _path: &Path,
            _content: &[u8],
        ) -> Result<Vec<phantom_core::symbol::SymbolEntry>, phantom_core::CoreError> {
            Ok(vec![])
        }

        fn diff_symbols(
            &self,
            _base: &[phantom_core::symbol::SymbolEntry],
            _current: &[phantom_core::symbol::SymbolEntry],
        ) -> Vec<phantom_core::changeset::SemanticOperation> {
            vec![]
        }

        fn three_way_merge(
            &self,
            base: &[u8],
            ours: &[u8],
            theirs: &[u8],
            _path: &Path,
        ) -> Result<phantom_core::conflict::MergeReport, phantom_core::CoreError> {
            use phantom_core::conflict::{MergeReport, MergeStrategy};
            let base_s = String::from_utf8_lossy(base);
            let ours_s = String::from_utf8_lossy(ours);
            let theirs_s = String::from_utf8_lossy(theirs);
            let result = match diffy::merge(&base_s, &ours_s, &theirs_s) {
                Ok(merged) => MergeResult::Clean(merged.into_bytes()),
                Err(_) => MergeResult::Conflict(vec![phantom_core::conflict::ConflictDetail {
                    kind: phantom_core::conflict::ConflictKind::RawTextConflict,
                    file: PathBuf::new(),
                    symbol_id: None,
                    ours_changeset: phantom_core::id::ChangesetId("trunk".into()),
                    theirs_changeset: phantom_core::id::ChangesetId("agent".into()),
                    description: "text merge conflict".into(),
                    ours_span: None,
                    theirs_span: None,
                    base_span: None,
                }]),
            };
            Ok(MergeReport {
                result,
                strategy: MergeStrategy::TextFallbackUnsupported,
            })
        }
    }

    // ---------------------------------------------------------------------------
    // current_base persistence tests
    // ---------------------------------------------------------------------------

    #[test]
    fn current_base_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let agent = AgentId("agent-a".into());
        let oid = GitOid::from_bytes([0xAB; 20]);

        write_current_base(tmp.path(), &agent, &oid).unwrap();
        let read = read_current_base(tmp.path(), &agent).unwrap();
        assert_eq!(read, Some(oid));
    }

    #[test]
    fn current_base_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let agent = AgentId("ghost".into());
        let result = read_current_base(tmp.path(), &agent).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn write_current_base_rejects_null_oid() {
        let tmp = TempDir::new().unwrap();
        let agent = AgentId("unborn".into());

        let err = write_current_base(tmp.path(), &agent, &GitOid::zero()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("null OID") || msg.contains("no initial commit"),
            "unexpected error: {msg}"
        );

        assert!(
            !tmp.path()
                .join("overlays")
                .join("unborn")
                .join("current_base")
                .exists()
        );
    }

    #[test]
    fn current_base_invalid_hex_errors() {
        let tmp = TempDir::new().unwrap();
        let agent = AgentId("agent-bad".into());
        let dir = tmp.path().join("overlays").join("agent-bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("current_base"), "not-valid-hex").unwrap();

        let result = read_current_base(tmp.path(), &agent);
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------------------
    // atomic_write_upper tests
    // ---------------------------------------------------------------------------

    #[test]
    fn atomic_write_creates_file_and_leaves_no_tmp() {
        let tmp = TempDir::new().unwrap();
        let upper = tmp.path();
        let rel = PathBuf::from("src/merged.rs");

        atomic_write_upper(upper, &rel, b"merged content").unwrap();

        assert_eq!(
            std::fs::read_to_string(upper.join("src/merged.rs")).unwrap(),
            "merged content"
        );
        assert!(!upper.join("src/merged.phantom-rebase-tmp").exists());
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let tmp = TempDir::new().unwrap();
        let upper = tmp.path();
        let rel = PathBuf::from("file.rs");

        std::fs::write(upper.join("file.rs"), b"old content").unwrap();
        atomic_write_upper(upper, &rel, b"new content").unwrap();

        assert_eq!(
            std::fs::read_to_string(upper.join("file.rs")).unwrap(),
            "new content"
        );
    }

    // ---------------------------------------------------------------------------
    // rebase_agent tests (require a real git repo)
    // ---------------------------------------------------------------------------

    #[test]
    fn rebase_clean_merge_updates_upper() {
        let repo_dir = TempDir::new().unwrap();
        let upper_dir = TempDir::new().unwrap();
        let git = init_repo(repo_dir.path());
        let analyzer = TextMergeAnalyzer;
        let agent = AgentId("agent-b".into());

        let base = commit_file(
            &git,
            "src/shared.rs",
            b"// top\nfn alpha() {}\n// middle\nfn beta() {}\n// bottom\n",
            "base",
        );

        let head = commit_file(
            &git,
            "src/shared.rs",
            b"// top\nfn new_trunk() {}\nfn alpha() {}\n// middle\nfn beta() {}\n// bottom\n",
            "trunk adds fn",
        );

        std::fs::create_dir_all(upper_dir.path().join("src")).unwrap();
        std::fs::write(
            upper_dir.path().join("src/shared.rs"),
            b"// top\nfn alpha() {}\n// middle\nfn beta() {}\n// bottom\nfn agent_fn() {}\n",
        )
        .unwrap();

        let result = rebase_agent(
            &git,
            &analyzer,
            &agent,
            &base,
            &head,
            upper_dir.path(),
            &[PathBuf::from("src/shared.rs")],
        )
        .unwrap();

        assert_eq!(result.merged.len(), 1);
        assert!(result.conflicted.is_empty());

        let content = std::fs::read_to_string(upper_dir.path().join("src/shared.rs")).unwrap();
        assert!(content.contains("fn new_trunk()"));
        assert!(content.contains("fn agent_fn()"));
    }

    #[test]
    fn rebase_conflict_leaves_upper_unchanged() {
        let repo_dir = TempDir::new().unwrap();
        let upper_dir = TempDir::new().unwrap();
        let git = init_repo(repo_dir.path());
        let analyzer = TextMergeAnalyzer;
        let agent = AgentId("agent-b".into());

        let base = commit_file(&git, "src/shared.rs", b"original\n", "base");
        let head = commit_file(&git, "src/shared.rs", b"trunk version\n", "trunk edit");

        std::fs::create_dir_all(upper_dir.path().join("src")).unwrap();
        std::fs::write(upper_dir.path().join("src/shared.rs"), b"agent version\n").unwrap();

        let result = rebase_agent(
            &git,
            &analyzer,
            &agent,
            &base,
            &head,
            upper_dir.path(),
            &[PathBuf::from("src/shared.rs")],
        )
        .unwrap();

        assert!(result.merged.is_empty());
        assert_eq!(result.conflicted.len(), 1);

        let content = std::fs::read_to_string(upper_dir.path().join("src/shared.rs")).unwrap();
        assert_eq!(content, "agent version\n");
    }

    #[test]
    fn rebase_trunk_deleted_file_is_conflict() {
        let repo_dir = TempDir::new().unwrap();
        let upper_dir = TempDir::new().unwrap();
        let git = init_repo(repo_dir.path());
        let analyzer = TextMergeAnalyzer;
        let agent = AgentId("agent-b".into());

        let base = commit_file(&git, "src/gone.rs", b"content\n", "base");

        // Trunk: delete the file.
        let repo = git.repo();
        let workdir = repo.workdir().unwrap();
        std::fs::remove_file(workdir.join("src/gone.rs")).unwrap();
        let mut index = repo.index().unwrap();
        index.remove_path(Path::new("src/gone.rs")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        let sig = git2::Signature::now("test", "test@test.com").unwrap();
        let oid = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "delete file",
                &tree,
                &[&head_commit],
            )
            .unwrap();
        let mut head_bytes = [0u8; 20];
        head_bytes.copy_from_slice(oid.as_bytes());
        let head = GitOid::from_bytes(head_bytes);

        std::fs::create_dir_all(upper_dir.path().join("src")).unwrap();
        std::fs::write(upper_dir.path().join("src/gone.rs"), b"agent modified\n").unwrap();

        let result = rebase_agent(
            &git,
            &analyzer,
            &agent,
            &base,
            &head,
            upper_dir.path(),
            &[PathBuf::from("src/gone.rs")],
        )
        .unwrap();

        assert!(result.merged.is_empty());
        assert_eq!(result.conflicted.len(), 1);
        assert_eq!(
            result.conflicted[0].1[0].kind,
            ConflictKind::ModifyDeleteSymbol
        );
    }

    #[test]
    fn rebase_mixed_clean_and_conflict() {
        let repo_dir = TempDir::new().unwrap();
        let upper_dir = TempDir::new().unwrap();
        let git = init_repo(repo_dir.path());
        let analyzer = TextMergeAnalyzer;
        let agent = AgentId("agent-b".into());

        let _base1 = commit_file(
            &git,
            "src/clean.rs",
            b"// top\nfn alpha() {}\n// bottom\n",
            "base clean",
        );
        let base = commit_file(&git, "src/conflict.rs", b"original\n", "base conflict");

        let _mid = commit_file(
            &git,
            "src/clean.rs",
            b"// top\nfn trunk_fn() {}\nfn alpha() {}\n// bottom\n",
            "trunk clean",
        );
        let head = commit_file(
            &git,
            "src/conflict.rs",
            b"trunk version\n",
            "trunk conflict",
        );

        std::fs::create_dir_all(upper_dir.path().join("src")).unwrap();
        std::fs::write(
            upper_dir.path().join("src/clean.rs"),
            b"// top\nfn alpha() {}\n// bottom\nfn agent_fn() {}\n",
        )
        .unwrap();
        std::fs::write(upper_dir.path().join("src/conflict.rs"), b"agent version\n").unwrap();

        let result = rebase_agent(
            &git,
            &analyzer,
            &agent,
            &base,
            &head,
            upper_dir.path(),
            &[
                PathBuf::from("src/clean.rs"),
                PathBuf::from("src/conflict.rs"),
            ],
        )
        .unwrap();

        assert_eq!(result.merged.len(), 1);
        assert_eq!(result.conflicted.len(), 1);
        assert_eq!(result.merged[0], PathBuf::from("src/clean.rs"));
        assert_eq!(result.conflicted[0].0, PathBuf::from("src/conflict.rs"));
    }

    #[test]
    fn rebase_empty_shadowed_list() {
        let repo_dir = TempDir::new().unwrap();
        let upper_dir = TempDir::new().unwrap();
        let git = init_repo(repo_dir.path());
        let analyzer = TextMergeAnalyzer;
        let agent = AgentId("agent-b".into());
        let head = git.head_oid().unwrap();

        let result =
            rebase_agent(&git, &analyzer, &agent, &head, &head, upper_dir.path(), &[]).unwrap();

        assert!(result.merged.is_empty());
        assert!(result.conflicted.is_empty());
    }
}

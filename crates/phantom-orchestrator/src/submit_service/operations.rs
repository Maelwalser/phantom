//! Extract semantic operations from an agent's overlay contents.
//!
//! For each modified file, diff base → current at the symbol level (or emit
//! `AddSymbol` for a new file). Any files that semantic analysis cannot
//! classify fall back to a single `RawDiff` op. Deleted files are emitted as
//! `DeleteFile`.

use std::path::{Path, PathBuf};

use phantom_core::changeset::SemanticOperation;
use phantom_core::id::GitOid;
use phantom_core::traits::SemanticAnalyzer;

use crate::error::OrchestratorError;
use crate::git::GitOps;

/// Aggregate counts used for human-readable submission summaries.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct OpCounts {
    pub additions: u32,
    pub modifications: u32,
    pub deletions: u32,
}

impl OpCounts {
    fn tally(&mut self, op: &SemanticOperation) {
        match op {
            SemanticOperation::AddSymbol { .. } | SemanticOperation::AddFile { .. } => {
                self.additions += 1;
            }
            SemanticOperation::ModifySymbol { .. } | SemanticOperation::RawDiff { .. } => {
                self.modifications += 1;
            }
            SemanticOperation::DeleteSymbol { .. } | SemanticOperation::DeleteFile { .. } => {
                self.deletions += 1;
            }
        }
    }
}

/// Result of extracting semantic operations from an overlay.
pub(super) struct ExtractedOps {
    pub all_ops: Vec<SemanticOperation>,
    pub counts: OpCounts,
}

/// Extract semantic operations per modified file, append synthesized
/// `DeleteFile` operations for deleted paths, and fall back to `RawDiff`
/// when semantic analysis yielded nothing.
pub(super) fn extract_operations(
    git: &GitOps,
    analyzer: &dyn SemanticAnalyzer,
    base_commit: &GitOid,
    upper_dir: &Path,
    modified: &[PathBuf],
    deleted: &[PathBuf],
) -> Result<ExtractedOps, OrchestratorError> {
    let mut all_ops: Vec<SemanticOperation> = Vec::new();
    let mut counts = OpCounts::default();

    for file in modified {
        let agent_content = std::fs::read(upper_dir.join(file)).map_err(OrchestratorError::Io)?;
        let ops = diff_file(git, analyzer, base_commit, file, &agent_content);
        for op in &ops {
            counts.tally(op);
        }
        all_ops.extend(ops);
    }

    for file in deleted {
        all_ops.push(SemanticOperation::DeleteFile { path: file.clone() });
        counts.deletions += 1;
    }

    // If semantic analysis yielded no structured ops, record raw diffs.
    if all_ops.is_empty() && !modified.is_empty() {
        for file in modified {
            all_ops.push(SemanticOperation::RawDiff {
                path: file.clone(),
                patch: String::new(),
            });
            counts.modifications += 1;
        }
    }

    Ok(ExtractedOps { all_ops, counts })
}

/// Diff a single file at the symbol level against its base version.
fn diff_file(
    git: &GitOps,
    analyzer: &dyn SemanticAnalyzer,
    base_commit: &GitOid,
    file: &Path,
    agent_content: &[u8],
) -> Vec<SemanticOperation> {
    if let Ok(base) = git.read_file_at_commit(base_commit, file) {
        let base_symbols = analyzer
            .extract_symbols(file, &base)
            .inspect_err(|_| {
                tracing::debug!(?file, "no semantic analysis available for base");
            })
            .unwrap_or_default();
        let current_symbols = analyzer
            .extract_symbols(file, agent_content)
            .inspect_err(|_| {
                tracing::debug!(?file, "no semantic analysis available for current");
            })
            .unwrap_or_default();
        analyzer.diff_symbols(&base_symbols, &current_symbols)
    } else {
        // New file — every symbol is an addition.
        let symbols = analyzer
            .extract_symbols(file, agent_content)
            .inspect_err(|_| {
                tracing::debug!(?file, "no semantic analysis available for new file");
            })
            .unwrap_or_default();
        symbols
            .into_iter()
            .map(|sym| SemanticOperation::AddSymbol {
                file: file.to_path_buf(),
                symbol: sym,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use phantom_core::conflict::MergeResult;
    use phantom_core::error::CoreError;
    use phantom_core::id::{ContentHash, SymbolId};
    use phantom_core::symbol::{SymbolEntry, SymbolKind};

    use crate::test_support::{commit_file, init_repo};
    use tempfile::TempDir;

    /// An analyzer that chooses between `base` and `current` symbol sets
    /// based on a marker substring in the file contents. Each test chooses
    /// the marker so `base` is returned for base-commit reads and `current`
    /// is returned for overlay reads.
    struct StubAnalyzer {
        current_marker: &'static [u8],
        base: Vec<SymbolEntry>,
        current: Vec<SymbolEntry>,
    }

    impl SemanticAnalyzer for StubAnalyzer {
        fn extract_symbols(
            &self,
            _path: &Path,
            content: &[u8],
        ) -> Result<Vec<SymbolEntry>, CoreError> {
            if contains_slice(content, self.current_marker) {
                Ok(self.current.clone())
            } else {
                Ok(self.base.clone())
            }
        }

        fn diff_symbols(
            &self,
            base: &[SymbolEntry],
            current: &[SymbolEntry],
        ) -> Vec<SemanticOperation> {
            let base_names: std::collections::HashSet<&str> =
                base.iter().map(|s| s.name.as_str()).collect();
            current
                .iter()
                .filter(|s| !base_names.contains(s.name.as_str()))
                .map(|sym| SemanticOperation::AddSymbol {
                    file: sym.file.clone(),
                    symbol: sym.clone(),
                })
                .collect()
        }

        fn three_way_merge(
            &self,
            _base: &[u8],
            _ours: &[u8],
            theirs: &[u8],
            _path: &Path,
        ) -> Result<phantom_core::conflict::MergeReport, CoreError> {
            Ok(phantom_core::conflict::MergeReport::semantic(
                MergeResult::Clean(theirs.to_vec()),
            ))
        }
    }

    /// An analyzer that refuses to extract symbols — exercises the RawDiff fallback.
    struct BlindAnalyzer;

    impl SemanticAnalyzer for BlindAnalyzer {
        fn extract_symbols(
            &self,
            _path: &Path,
            _content: &[u8],
        ) -> Result<Vec<SymbolEntry>, CoreError> {
            Err(CoreError::Semantic("no grammar".into()))
        }

        fn diff_symbols(
            &self,
            _base: &[SymbolEntry],
            _current: &[SymbolEntry],
        ) -> Vec<SemanticOperation> {
            vec![]
        }

        fn three_way_merge(
            &self,
            _base: &[u8],
            _ours: &[u8],
            theirs: &[u8],
            _path: &Path,
        ) -> Result<phantom_core::conflict::MergeReport, CoreError> {
            Ok(phantom_core::conflict::MergeReport::semantic(
                MergeResult::Clean(theirs.to_vec()),
            ))
        }
    }

    fn contains_slice(haystack: &[u8], needle: &[u8]) -> bool {
        if needle.is_empty() {
            return true;
        }
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn sym(name: &str, file: &str) -> SymbolEntry {
        SymbolEntry {
            id: SymbolId(format!("crate::{name}::Function")),
            kind: SymbolKind::Function,
            name: name.to_string(),
            scope: "crate".to_string(),
            file: PathBuf::from(file),
            byte_range: 0..10,
            content_hash: ContentHash([0; 32]),
        }
    }

    #[test]
    fn new_file_emits_add_symbol_per_symbol() {
        let (_repo_dir, git) = init_repo(&[]);
        let base = git.head_oid().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("new.rs"), b"fn new_one() {}").unwrap();

        let analyzer = StubAnalyzer {
            current_marker: b"new_one",
            base: vec![],
            current: vec![sym("new_one", "new.rs")],
        };
        let extracted = extract_operations(
            &git,
            &analyzer,
            &base,
            upper.path(),
            &[PathBuf::from("new.rs")],
            &[],
        )
        .unwrap();
        assert_eq!(extracted.all_ops.len(), 1);
        assert!(matches!(
            &extracted.all_ops[0],
            SemanticOperation::AddSymbol { .. }
        ));
        assert_eq!(extracted.counts.additions, 1);
        assert_eq!(extracted.counts.modifications, 0);
        assert_eq!(extracted.counts.deletions, 0);
    }

    #[test]
    fn deleted_file_emits_delete_file_op() {
        let (_repo_dir, git) = init_repo(&[("x.rs", b"fn x() {}")]);
        let base = git.head_oid().unwrap();
        let upper = TempDir::new().unwrap();

        let analyzer = StubAnalyzer {
            current_marker: b"",
            base: vec![],
            current: vec![],
        };
        let extracted = extract_operations(
            &git,
            &analyzer,
            &base,
            upper.path(),
            &[],
            &[PathBuf::from("x.rs")],
        )
        .unwrap();

        assert_eq!(extracted.all_ops.len(), 1);
        assert!(matches!(
            &extracted.all_ops[0],
            SemanticOperation::DeleteFile { path } if path == Path::new("x.rs")
        ));
        assert_eq!(extracted.counts.deletions, 1);
    }

    #[test]
    fn empty_overlay_with_analyzer_that_returns_nothing_falls_back_to_raw_diff() {
        let (_repo_dir, git) = init_repo(&[("a.rs", b"fn a() {}")]);
        let _bumped = commit_file(&git, "a.rs", b"fn a() { /* v2 */ }", "bump");
        let base = git.head_oid().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("a.rs"), b"fn a() { /* v3 */ }").unwrap();

        let extracted = extract_operations(
            &git,
            &BlindAnalyzer,
            &base,
            upper.path(),
            &[PathBuf::from("a.rs")],
            &[],
        )
        .unwrap();
        assert_eq!(extracted.all_ops.len(), 1);
        assert!(matches!(
            &extracted.all_ops[0],
            SemanticOperation::RawDiff { path, .. } if path == Path::new("a.rs")
        ));
        assert_eq!(extracted.counts.modifications, 1);
    }

    #[test]
    fn modified_and_deleted_counts_are_combined() {
        let (_repo_dir, git) = init_repo(&[("keep.rs", b"fn keep() {}")]);
        let base = git.head_oid().unwrap();
        let upper = TempDir::new().unwrap();
        std::fs::write(upper.path().join("keep.rs"), b"fn keep() { new_sym(); }").unwrap();

        let analyzer = StubAnalyzer {
            current_marker: b"new_sym",
            base: vec![],
            current: vec![sym("new_sym", "keep.rs")],
        };
        let extracted = extract_operations(
            &git,
            &analyzer,
            &base,
            upper.path(),
            &[PathBuf::from("keep.rs")],
            &[PathBuf::from("gone.rs")],
        )
        .unwrap();

        assert_eq!(extracted.counts.additions, 1, "new_sym counts as addition");
        assert_eq!(extracted.counts.deletions, 1, "gone.rs counts as deletion");
        assert_eq!(extracted.all_ops.len(), 2);
    }
}

//! Build the subject line of an auto-generated commit message.

use std::path::{Path, PathBuf};

use phantom_core::changeset::SemanticOperation;
use phantom_core::id::AgentId;

/// Symbol and file lists bucketed by operation kind, used to render the subject.
pub(super) struct OpBins<'a> {
    pub added: Vec<&'a str>,
    pub modified: Vec<&'a str>,
    pub deleted: Vec<&'a str>,
    pub new_files: Vec<&'a Path>,
    pub deleted_files: Vec<&'a Path>,
    pub raw_files: Vec<&'a Path>,
}

impl<'a> OpBins<'a> {
    pub(super) fn from_ops(ops: &'a [SemanticOperation]) -> Self {
        let mut bins = Self {
            added: Vec::new(),
            modified: Vec::new(),
            deleted: Vec::new(),
            new_files: Vec::new(),
            deleted_files: Vec::new(),
            raw_files: Vec::new(),
        };
        for op in ops {
            match op {
                SemanticOperation::AddSymbol { symbol, .. } => bins.added.push(&symbol.name),
                SemanticOperation::ModifySymbol { new_entry, .. } => {
                    bins.modified.push(&new_entry.name);
                }
                SemanticOperation::DeleteSymbol { id, .. } => {
                    // SymbolId is "scope::name::kind"; extract the name.
                    let name = id.0.split("::").nth(1).unwrap_or(&id.0);
                    bins.deleted.push(name);
                }
                SemanticOperation::AddFile { path } => bins.new_files.push(path),
                SemanticOperation::DeleteFile { path } => bins.deleted_files.push(path),
                SemanticOperation::RawDiff { path, .. } => bins.raw_files.push(path),
            }
        }
        bins
    }
}

/// Build a concise subject line from bucketed operations.
pub(super) fn build_subject(
    agent_id: &AgentId,
    bins: &OpBins<'_>,
    modified_files: &[PathBuf],
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if !bins.modified.is_empty() {
        parts.push(format!("modify {}", symbol_summary(&bins.modified, 4)));
    }
    if !bins.added.is_empty() {
        parts.push(format!("add {}", symbol_summary(&bins.added, 4)));
    }
    if !bins.deleted.is_empty() {
        parts.push(format!("remove {}", symbol_summary(&bins.deleted, 4)));
    }
    if !bins.new_files.is_empty() {
        parts.push(format!("create {}", file_summary(&bins.new_files, 3)));
    }
    if !bins.deleted_files.is_empty() {
        parts.push(format!("delete {}", file_summary(&bins.deleted_files, 3)));
    }
    if !bins.raw_files.is_empty() && parts.is_empty() {
        parts.push(format!("update {}", file_summary(&bins.raw_files, 3)));
    }

    if parts.is_empty() {
        return format!(
            "phantom({agent_id}): update {} file(s)",
            modified_files.len()
        );
    }

    let subject = format!("phantom({agent_id}): {}", parts.join(", "));
    if subject.len() > 120 {
        format!("{}...", &subject[..117])
    } else {
        subject
    }
}

/// Summarize a list of symbol names, showing up to `max` names.
fn symbol_summary(names: &[&str], max: usize) -> String {
    if names.len() <= max {
        names.join(", ")
    } else {
        let shown: Vec<&str> = names[..max].to_vec();
        format!("{} (+{} more)", shown.join(", "), names.len() - max)
    }
}

/// Summarize a list of file paths, showing filenames only.
fn file_summary(paths: &[&Path], max: usize) -> String {
    let names: Vec<&str> = paths
        .iter()
        .map(|p| p.file_name().and_then(|n| n.to_str()).unwrap_or("?"))
        .collect();
    symbol_summary(&names, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::changeset::SemanticOperation;
    use phantom_core::id::{ContentHash, SymbolId};
    use phantom_core::symbol::{SymbolEntry, SymbolKind};

    fn sym(name: &str) -> SymbolEntry {
        SymbolEntry {
            id: SymbolId(format!("crate::{name}::Function")),
            kind: SymbolKind::Function,
            name: name.to_string(),
            scope: "crate".to_string(),
            file: PathBuf::from("src/lib.rs"),
            byte_range: 0..10,
            content_hash: ContentHash([0; 32]),
        }
    }

    #[test]
    fn subject_reports_modified_added_deleted() {
        let ops = vec![
            SemanticOperation::ModifySymbol {
                file: PathBuf::from("src/lib.rs"),
                old_hash: ContentHash([0; 32]),
                new_entry: sym("foo"),
            },
            SemanticOperation::AddSymbol {
                file: PathBuf::from("src/lib.rs"),
                symbol: sym("bar"),
            },
            SemanticOperation::DeleteSymbol {
                file: PathBuf::from("src/lib.rs"),
                id: SymbolId("crate::baz::Function".into()),
            },
        ];
        let bins = OpBins::from_ops(&ops);
        let subj = build_subject(&AgentId("agent-x".into()), &bins, &[]);
        assert!(subj.contains("phantom(agent-x)"));
        assert!(subj.contains("modify foo"));
        assert!(subj.contains("add bar"));
        assert!(subj.contains("remove baz"));
    }

    #[test]
    fn subject_falls_back_to_file_count_when_empty() {
        let bins = OpBins::from_ops(&[]);
        let files = vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")];
        let subj = build_subject(&AgentId("a1".into()), &bins, &files);
        assert_eq!(subj, "phantom(a1): update 2 file(s)");
    }

    #[test]
    fn subject_truncates_when_too_long() {
        let names: Vec<String> = (0..30)
            .map(|i| format!("very_long_symbol_name_{i}"))
            .collect();
        let ops: Vec<SemanticOperation> = names
            .iter()
            .map(|n| SemanticOperation::AddSymbol {
                file: PathBuf::from("src/lib.rs"),
                symbol: sym(n),
            })
            .collect();
        let bins = OpBins::from_ops(&ops);
        let subj = build_subject(&AgentId("a".into()), &bins, &[]);
        assert!(subj.ends_with("..."));
        assert_eq!(subj.len(), 120);
    }

    #[test]
    fn symbol_summary_truncates_with_plus_more() {
        let names = vec!["a", "b", "c", "d", "e", "f"];
        let out = symbol_summary(&names, 3);
        assert_eq!(out, "a, b, c (+3 more)");
    }

    #[test]
    fn symbol_summary_no_truncation_when_within_limit() {
        let names = vec!["a", "b"];
        let out = symbol_summary(&names, 4);
        assert_eq!(out, "a, b");
    }
}

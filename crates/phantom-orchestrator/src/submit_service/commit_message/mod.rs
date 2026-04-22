//! Generate a descriptive commit message from a list of semantic operations.
//!
//! The subject line summarizes what changed (added / modified / deleted
//! symbols and files); the optional body lists individual operations grouped
//! by file. Both halves are rendered as deterministic, stable text so they
//! round-trip cleanly through git.

use std::path::PathBuf;

use phantom_core::changeset::SemanticOperation;
use phantom_core::id::AgentId;

mod body;
mod subject;

/// Generate a commit message for a submission by an agent.
pub(crate) fn generate_commit_message(
    agent_id: &AgentId,
    ops: &[SemanticOperation],
    modified_files: &[PathBuf],
) -> String {
    let bins = subject::OpBins::from_ops(ops);
    let subject = subject::build_subject(agent_id, &bins, modified_files);
    let body = body::build_body(ops);
    if body.is_empty() {
        subject
    } else {
        format!("{subject}{body}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
            signature_hash: ContentHash([0; 32]),
        }
    }

    #[test]
    fn generate_combines_subject_and_body() {
        let ops = vec![SemanticOperation::AddSymbol {
            file: PathBuf::from("src/lib.rs"),
            symbol: sym("hello"),
        }];
        let msg = generate_commit_message(&AgentId("a".into()), &ops, &[]);
        let (subject, body) = msg.split_once('\n').expect("subject + body separator");
        assert!(subject.contains("add hello"));
        assert!(body.contains("src/lib.rs:"));
        assert!(body.contains("+ hello"));
    }

    #[test]
    fn generate_subject_only_when_no_ops() {
        let msg = generate_commit_message(&AgentId("a".into()), &[], &[PathBuf::from("f.rs")]);
        assert_eq!(msg, "phantom(a): update 1 file(s)");
        assert!(!msg.contains('\n'));
    }
}

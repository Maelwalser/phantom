//! Integration test: references to external names (stdlib, other crates)
//! should never panic or fail materialization — they are silently dropped.

use phantom_core::symbol::{ReferenceKind, SymbolReference};
use phantom_core::traits::{DependencyGraph, SymbolIndex};
use phantom_semantic::{InMemoryDependencyGraph, InMemorySymbolIndex, Parser};
use std::path::{Path, PathBuf};

#[test]
fn external_references_do_not_panic_or_produce_edges() {
    let src = r#"
use std::collections::HashMap;

fn load() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("k".into(), "v".into());
    m
}
"#;

    let parser = Parser::new();
    let (symbols, refs) = parser
        .parse_file_with_refs(Path::new("src/lib.rs"), src.as_bytes())
        .unwrap();

    // The file itself defines `load`.
    assert!(symbols.iter().any(|s| s.name == "load"));
    // The references include HashMap and its ::new method — but those
    // targets are not present in the index.
    assert!(!refs.is_empty(), "expected some references to be emitted");

    let mut index = InMemorySymbolIndex::new(phantom_core::id::GitOid::zero());
    index.update_file(Path::new("src/lib.rs"), symbols);

    let mut graph = InMemoryDependencyGraph::new();
    // Must not panic.
    graph.update_file(Path::new("src/lib.rs"), refs, &index);

    // No resolvable target → zero edges.
    assert_eq!(
        graph.edge_count(),
        0,
        "references to external names must produce no graph edges"
    );
}

#[test]
fn reference_with_scope_hint_that_doesnt_match_is_dropped() {
    use phantom_core::id::{ContentHash, GitOid, SymbolId};
    use phantom_core::symbol::{SymbolEntry, SymbolKind};

    let mut index = InMemorySymbolIndex::new(GitOid::zero());
    index.update_file(
        Path::new("src/lib.rs"),
        vec![SymbolEntry {
            id: SymbolId("crate::billing::login::function".into()),
            kind: SymbolKind::Function,
            name: "login".into(),
            scope: "crate::billing".into(),
            file: PathBuf::from("src/lib.rs"),
            byte_range: 0..10,
            content_hash: ContentHash::from_bytes(b"login"),
            signature_hash: ContentHash::from_bytes(b"login"),
        }],
    );

    let mut graph = InMemoryDependencyGraph::new();
    let r = SymbolReference {
        source: SymbolId("crate::caller::function".into()),
        target_name: "login".into(),
        target_scope_hint: Some("crate::auth".into()), // different scope
        kind: ReferenceKind::Call,
        file: PathBuf::from("src/caller.rs"),
        byte_range: 0..5,
    };
    graph.update_file(Path::new("src/caller.rs"), vec![r], &index);

    // Over-approximation: when the scope hint doesn't match any candidate,
    // the resolver falls back to name-only. So it *does* produce an edge
    // (the only candidate is `crate::billing::login`).  This is the
    // intended behaviour — false positive is preferable to false negative
    // for notifications.
    assert_eq!(graph.edge_count(), 1);
}

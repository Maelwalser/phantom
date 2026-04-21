//! In-memory semantic dependency graph.
//!
//! Tracks which symbols reference which others (function calls, type uses,
//! imports, trait impls). Built from the same tree-sitter ASTs as the
//! symbol index. Used at ripple time to answer: *"If trunk symbol X just
//! changed, which of agent A's working-set symbols depend on X?"*
//!
//! The graph is **rebuilt on every materialization** rather than persisted:
//! this mirrors the lifecycle of [`InMemorySymbolIndex`](crate::InMemorySymbolIndex)
//! and avoids a second source of truth that could diverge. A SQLite cache
//! for large repos is planned once benchmarks show it's needed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use phantom_core::id::SymbolId;
use phantom_core::symbol::SymbolReference;
use phantom_core::traits::{DependencyEdge, DependencyGraph, SymbolIndex};

use crate::error::SemanticError;
use crate::parser::Parser;

/// In-memory dependency graph backed by forward + reverse adjacency maps.
///
/// Resolution strategy (name + scope heuristic):
/// 1. If the reference has a `target_scope_hint`, prefer symbols whose
///    `scope` ends with that hint *and* match `name`.
/// 2. Otherwise, fall back to name-only matching.
/// 3. When more than one candidate matches, the graph over-approximates and
///    adds an edge to **every** candidate. This yields false-positive
///    notifications but never false negatives — the right trade for a
///    ripple system whose job is to keep agents informed.
/// 4. When zero candidates match (reference to an external crate, stdlib,
///    etc.), the reference is silently dropped.
pub struct InMemoryDependencyGraph {
    /// source symbol → edges originating from that symbol.
    forward: HashMap<SymbolId, Vec<DependencyEdge>>,
    /// target symbol → edges pointing at that symbol (reverse index).
    reverse: HashMap<SymbolId, Vec<DependencyEdge>>,
    /// Per-file (source, target) pairs — enables O(edges_in_file) removal.
    per_file: HashMap<PathBuf, Vec<(SymbolId, SymbolId)>>,
}

impl InMemoryDependencyGraph {
    /// Create an empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self {
            forward: HashMap::new(),
            reverse: HashMap::new(),
            per_file: HashMap::new(),
        }
    }

    /// Build a graph by walking a directory and parsing all supported files.
    ///
    /// Caller is responsible for providing a matching [`SymbolIndex`] that
    /// has already been populated from the same trunk state. Any reference
    /// that cannot be resolved against `index` is dropped silently.
    pub fn build_from_directory(
        root: &Path,
        parser: &Parser,
        index: &dyn SymbolIndex,
    ) -> Result<Self, SemanticError> {
        let mut graph = Self::new();
        walk_dir(root, root, parser, index, &mut graph)?;
        Ok(graph)
    }
}

impl Default for InMemoryDependencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl DependencyGraph for InMemoryDependencyGraph {
    fn dependents_of(&self, target: &SymbolId) -> Vec<DependencyEdge> {
        self.reverse.get(target).cloned().unwrap_or_default()
    }

    fn dependencies_of(&self, source: &SymbolId) -> Vec<DependencyEdge> {
        self.forward.get(source).cloned().unwrap_or_default()
    }

    fn update_file(&mut self, path: &Path, refs: Vec<SymbolReference>, index: &dyn SymbolIndex) {
        self.remove_file(path);

        let mut pairs: Vec<(SymbolId, SymbolId)> = Vec::new();
        for reference in refs {
            for target in resolve_targets(&reference, index) {
                let edge = DependencyEdge {
                    source: reference.source.clone(),
                    target: target.clone(),
                    kind: reference.kind,
                    file: reference.file.clone(),
                    byte_range: reference.byte_range.clone(),
                };
                self.forward
                    .entry(edge.source.clone())
                    .or_default()
                    .push(edge.clone());
                self.reverse
                    .entry(edge.target.clone())
                    .or_default()
                    .push(edge);
                pairs.push((reference.source.clone(), target));
            }
        }
        if !pairs.is_empty() {
            self.per_file.insert(path.to_path_buf(), pairs);
        }
    }

    fn remove_file(&mut self, path: &Path) {
        let Some(pairs) = self.per_file.remove(path) else {
            return;
        };
        for (source, target) in pairs {
            if let Some(edges) = self.forward.get_mut(&source) {
                edges.retain(|e| !(e.target == target && e.file == path));
                if edges.is_empty() {
                    self.forward.remove(&source);
                }
            }
            if let Some(edges) = self.reverse.get_mut(&target) {
                edges.retain(|e| !(e.source == source && e.file == path));
                if edges.is_empty() {
                    self.reverse.remove(&target);
                }
            }
        }
    }

    fn edge_count(&self) -> usize {
        self.forward.values().map(Vec::len).sum()
    }
}

/// Resolve a single [`SymbolReference`] to zero or more target [`SymbolId`]s
/// using the name + scope heuristic.
fn resolve_targets(reference: &SymbolReference, index: &dyn SymbolIndex) -> Vec<SymbolId> {
    let candidates = index.lookup_by_name(&reference.target_name);
    if candidates.is_empty() {
        return Vec::new();
    }

    // Prefer scope-matching candidates when a hint is present.
    if let Some(hint) = &reference.target_scope_hint {
        let scoped: Vec<SymbolId> = candidates
            .iter()
            .filter(|c| scope_matches(&c.scope, hint))
            .map(|c| c.id.clone())
            .collect();
        if !scoped.is_empty() {
            return scoped;
        }
    }

    // No scope hint, or hint didn't match — fall back to all candidates.
    // This is the over-approximation step.
    candidates.into_iter().map(|c| c.id).collect()
}

/// Check whether a candidate's fully-qualified `scope` is compatible with a
/// reference's `scope_hint`.
///
/// Accepts an exact match or an end-match so that `crate::auth` matches a
/// candidate whose scope is `my_crate::auth` (external crate references).
fn scope_matches(candidate_scope: &str, hint: &str) -> bool {
    if candidate_scope == hint {
        return true;
    }
    if candidate_scope.ends_with(hint) {
        // Must break on a `::` boundary to avoid `crate::auth` matching
        // `crate::authenticator`.
        let boundary = candidate_scope.len() - hint.len();
        return boundary == 0 || candidate_scope.as_bytes()[boundary - 1] == b':';
    }
    false
}

/// Recursively walk a directory, parse supported files, and register their
/// references with `graph`.
fn walk_dir(
    dir: &Path,
    root: &Path,
    parser: &Parser,
    index: &dyn SymbolIndex,
    graph: &mut InMemoryDependencyGraph,
) -> Result<(), SemanticError> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                walk_dir(&path, root, parser, index, graph)?;
            }
        } else if parser.supports_language(&path) {
            let content = std::fs::read(&path)?;
            let relative = path.strip_prefix(root).unwrap_or(&path);
            match parser.parse_file_with_refs(relative, &content) {
                Ok((_, refs)) => {
                    graph.update_file(relative, refs, index);
                }
                Err(SemanticError::ParseError { .. }) => {
                    tracing::warn!(?path, "skipping file that failed to parse");
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::id::{ContentHash, GitOid};
    use phantom_core::symbol::{ReferenceKind, SymbolEntry, SymbolKind};

    use crate::InMemorySymbolIndex;

    fn sym(name: &str, scope: &str, kind: SymbolKind, file: &str) -> SymbolEntry {
        let kind_str = format!("{kind:?}").to_lowercase();
        SymbolEntry {
            id: SymbolId(format!("{scope}::{name}::{kind_str}")),
            kind,
            name: name.into(),
            scope: scope.into(),
            file: PathBuf::from(file),
            byte_range: 0..10,
            content_hash: ContentHash::from_bytes(name.as_bytes()),
            signature_hash: ContentHash::from_bytes(name.as_bytes()),
        }
    }

    fn sref(
        source: &str,
        target: &str,
        scope_hint: Option<&str>,
        kind: ReferenceKind,
        file: &str,
    ) -> SymbolReference {
        SymbolReference {
            source: SymbolId(source.into()),
            target_name: target.into(),
            target_scope_hint: scope_hint.map(str::to_string),
            kind,
            file: PathBuf::from(file),
            byte_range: 0..5,
        }
    }

    #[test]
    fn resolves_unique_name() {
        let mut index = InMemorySymbolIndex::new(GitOid::zero());
        let login = sym("login", "crate::auth", SymbolKind::Function, "auth.rs");
        index.update_file(Path::new("auth.rs"), vec![login.clone()]);

        let mut graph = InMemoryDependencyGraph::new();
        graph.update_file(
            Path::new("caller.rs"),
            vec![sref(
                "crate::caller::function",
                "login",
                None,
                ReferenceKind::Call,
                "caller.rs",
            )],
            &index,
        );

        let deps = graph.dependents_of(&login.id);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].source.0, "crate::caller::function");
        assert_eq!(deps[0].target, login.id);
        assert_eq!(deps[0].kind, ReferenceKind::Call);
    }

    #[test]
    fn ambiguous_name_attaches_to_all_candidates() {
        let mut index = InMemorySymbolIndex::new(GitOid::zero());
        let new_a = sym("new", "crate::A", SymbolKind::Method, "a.rs");
        let new_b = sym("new", "crate::B", SymbolKind::Method, "b.rs");
        index.update_file(Path::new("a.rs"), vec![new_a.clone()]);
        index.update_file(Path::new("b.rs"), vec![new_b.clone()]);

        let mut graph = InMemoryDependencyGraph::new();
        graph.update_file(
            Path::new("caller.rs"),
            vec![sref(
                "crate::caller::function",
                "new",
                None,
                ReferenceKind::Call,
                "caller.rs",
            )],
            &index,
        );

        // Over-approximation: both candidates are dependents.
        assert_eq!(graph.dependents_of(&new_a.id).len(), 1);
        assert_eq!(graph.dependents_of(&new_b.id).len(), 1);
    }

    #[test]
    fn scope_hint_disambiguates() {
        let mut index = InMemorySymbolIndex::new(GitOid::zero());
        let new_a = sym("new", "crate::A", SymbolKind::Method, "a.rs");
        let new_b = sym("new", "crate::B", SymbolKind::Method, "b.rs");
        index.update_file(Path::new("a.rs"), vec![new_a.clone()]);
        index.update_file(Path::new("b.rs"), vec![new_b.clone()]);

        let mut graph = InMemoryDependencyGraph::new();
        graph.update_file(
            Path::new("caller.rs"),
            vec![sref(
                "crate::caller::function",
                "new",
                Some("crate::A"),
                ReferenceKind::Call,
                "caller.rs",
            )],
            &index,
        );

        assert_eq!(graph.dependents_of(&new_a.id).len(), 1);
        assert!(graph.dependents_of(&new_b.id).is_empty());
    }

    #[test]
    fn unresolved_name_drops_silently() {
        let index = InMemorySymbolIndex::new(GitOid::zero());

        let mut graph = InMemoryDependencyGraph::new();
        graph.update_file(
            Path::new("caller.rs"),
            vec![sref(
                "crate::caller::function",
                "external_crate_fn",
                None,
                ReferenceKind::Call,
                "caller.rs",
            )],
            &index,
        );

        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn remove_file_purges_its_edges() {
        let mut index = InMemorySymbolIndex::new(GitOid::zero());
        let login = sym("login", "crate::auth", SymbolKind::Function, "auth.rs");
        index.update_file(Path::new("auth.rs"), vec![login.clone()]);

        let mut graph = InMemoryDependencyGraph::new();
        graph.update_file(
            Path::new("caller.rs"),
            vec![sref(
                "crate::caller::function",
                "login",
                None,
                ReferenceKind::Call,
                "caller.rs",
            )],
            &index,
        );
        assert_eq!(graph.edge_count(), 1);

        graph.remove_file(Path::new("caller.rs"));
        assert_eq!(graph.edge_count(), 0);
        assert!(graph.dependents_of(&login.id).is_empty());
    }

    #[test]
    fn update_file_idempotent() {
        let mut index = InMemorySymbolIndex::new(GitOid::zero());
        let login = sym("login", "crate::auth", SymbolKind::Function, "auth.rs");
        index.update_file(Path::new("auth.rs"), vec![login]);

        let mut graph = InMemoryDependencyGraph::new();
        let r = sref(
            "crate::caller::function",
            "login",
            None,
            ReferenceKind::Call,
            "caller.rs",
        );
        graph.update_file(Path::new("caller.rs"), vec![r.clone()], &index);
        graph.update_file(Path::new("caller.rs"), vec![r], &index);

        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn forward_and_reverse_are_consistent() {
        let mut index = InMemorySymbolIndex::new(GitOid::zero());
        let login = sym("login", "crate::auth", SymbolKind::Function, "auth.rs");
        index.update_file(Path::new("auth.rs"), vec![login.clone()]);

        let mut graph = InMemoryDependencyGraph::new();
        let caller_id = SymbolId("crate::caller::function".into());
        graph.update_file(
            Path::new("caller.rs"),
            vec![sref(
                &caller_id.0,
                "login",
                None,
                ReferenceKind::Call,
                "caller.rs",
            )],
            &index,
        );

        let forward = graph.dependencies_of(&caller_id);
        let reverse = graph.dependents_of(&login.id);
        assert_eq!(forward.len(), 1);
        assert_eq!(reverse.len(), 1);
        assert_eq!(forward[0].target, reverse[0].target);
        assert_eq!(forward[0].source, reverse[0].source);
    }

    #[test]
    fn scope_matches_is_boundary_aware() {
        // Must not match mid-identifier.
        assert!(!scope_matches("crate::authenticator", "crate::auth"));
        // Exact match.
        assert!(scope_matches("crate::auth", "crate::auth"));
        // Suffix match on `::` boundary.
        assert!(scope_matches("my_crate::auth", "auth"));
    }
}

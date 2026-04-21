//! Structured three-way merge for config files (TOML / YAML / JSON).
//!
//! These formats are hierarchical key/value documents. The symbol-based merger
//! in [`crate::merge`] treats each top-level table/object as one opaque symbol,
//! so two agents making disjoint additive edits inside the same table (e.g.,
//! adding different entries to `[project].dependencies` in `pyproject.toml`)
//! are reported as a spurious conflict.
//!
//! This module parses the three document versions into a common [`Node`] tree,
//! runs a key-level three-way merge, and emits merged bytes (or a structured
//! conflict) without ever touching the symbol-based path.

use std::path::Path;

use phantom_core::conflict::{ConflictDetail, ConflictKind, ConflictSpan, MergeResult};
use phantom_core::id::{ChangesetId, SymbolId};

use crate::error::SemanticError;

pub(crate) mod json;
pub(crate) mod toml;
pub(crate) mod yaml;

/// Format-neutral AST used by the structured merger.
///
/// Order is significant: `Mapping` preserves insertion order so repeated
/// re-serialization produces stable bytes and new keys are appended at the
/// boundary the user expects.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    Null,
    Bool(bool),
    /// Canonicalized scalar — numbers and strings are all stored as their
    /// textual form so equality comparisons match the source representation.
    Scalar(String),
    Array(Vec<Node>),
    /// Insertion-ordered key/value pairs. `Vec` rather than `HashMap` so we
    /// can preserve the author's ordering when round-tripping.
    Mapping(Vec<(String, Node)>),
}

impl Node {
    pub(crate) fn as_mapping(&self) -> Option<&[(String, Node)]> {
        match self {
            Self::Mapping(entries) => Some(entries),
            _ => None,
        }
    }
}

/// Pluggable per-format merger.
pub(crate) trait ConfigMerger: Send + Sync {
    fn extensions(&self) -> &'static [&'static str];
    fn merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        path: &Path,
    ) -> Result<MergeResult, SemanticError>;
}

/// Look up a merger by file extension. Returns `None` for files that should
/// continue to flow through the symbol-based merge path.
pub(crate) fn merger_for(path: &Path) -> Option<Box<dyn ConfigMerger>> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    all_mergers()
        .into_iter()
        .find(|m| m.extensions().iter().any(|e| *e == ext))
}

fn all_mergers() -> Vec<Box<dyn ConfigMerger>> {
    vec![
        Box::new(toml::TomlConfigMerger),
        Box::new(json::JsonConfigMerger),
        Box::new(yaml::YamlConfigMerger),
    ]
}

/// Record of a single unresolvable disagreement within the tree.
///
/// `path` is a JSON-pointer-like dotted key trail into the document, e.g.
/// `project.version` or `dependencies[0]`. It's only used to build the
/// `symbol_id` and human description on the emitted [`ConflictDetail`].
#[derive(Debug, Clone)]
pub(crate) struct NodeConflict {
    pub(crate) path: Vec<String>,
    pub(crate) description: String,
}

impl NodeConflict {
    fn dotted(&self) -> String {
        self.path.join("::")
    }
}

/// Three-way merge at the [`Node`] level. Returns the merged tree or a list
/// of conflicts capturing every irreconcilable disagreement.
///
/// Semantics:
///
/// - Identical on both sides → pass through.
/// - Only one side changed → take that side.
/// - Both changed to the same value → deduplicate.
/// - Mappings: recurse per key; set-union for keys only one side added.
/// - Arrays of scalars: union-by-value, preserving base order then ours' then theirs' additions.
/// - Arrays containing any non-scalar element: positional recursion; length mismatch with overlapping edits conflicts.
/// - Scalar / type-mismatch divergence → conflict.
pub(crate) fn merge_tree(
    base: &Node,
    ours: &Node,
    theirs: &Node,
) -> Result<Node, Vec<NodeConflict>> {
    let mut conflicts = Vec::new();
    let merged = merge_node(base, ours, theirs, &mut Vec::new(), &mut conflicts);
    if conflicts.is_empty() {
        Ok(merged)
    } else {
        Err(conflicts)
    }
}

fn merge_node(
    base: &Node,
    ours: &Node,
    theirs: &Node,
    path: &mut Vec<String>,
    conflicts: &mut Vec<NodeConflict>,
) -> Node {
    if ours == theirs {
        return ours.clone();
    }
    if ours == base {
        return theirs.clone();
    }
    if theirs == base {
        return ours.clone();
    }

    match (ours, theirs) {
        (Node::Mapping(o), Node::Mapping(t)) => {
            let base_map = base.as_mapping();
            merge_mappings(base_map, o, t, path, conflicts)
        }
        (Node::Array(o), Node::Array(t)) => {
            let base_arr = match base {
                Node::Array(b) => Some(b.as_slice()),
                _ => None,
            };
            merge_arrays(base_arr, o, t, path, conflicts)
        }
        // Type mismatch or genuine scalar divergence.
        _ => {
            conflicts.push(NodeConflict {
                path: path.clone(),
                description: format!(
                    "values diverge at {}: ours={ours:?}, theirs={theirs:?}",
                    path_display(path)
                ),
            });
            ours.clone()
        }
    }
}

fn merge_mappings(
    base: Option<&[(String, Node)]>,
    ours: &[(String, Node)],
    theirs: &[(String, Node)],
    path: &mut Vec<String>,
    conflicts: &mut Vec<NodeConflict>,
) -> Node {
    let base_lookup: Vec<(&String, &Node)> = base
        .map(|b| b.iter().map(|(k, v)| (k, v)).collect())
        .unwrap_or_default();
    let find = |haystack: &[(String, Node)], key: &str| -> Option<Node> {
        haystack
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let base_find = |key: &str| -> Option<Node> {
        base_lookup
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map(|(_, v)| (*v).clone())
    };

    // Iteration order: base order first (stable), then ours' new keys, then
    // theirs' new keys. Preserves document layout for the common case.
    let mut key_order: Vec<String> = Vec::new();
    fn push_unique(order: &mut Vec<String>, key: &str) {
        if !order.iter().any(|k| k == key) {
            order.push(key.to_string());
        }
    }
    if let Some(b) = base {
        for (k, _) in b {
            push_unique(&mut key_order, k);
        }
    }
    for (k, _) in ours {
        push_unique(&mut key_order, k);
    }
    for (k, _) in theirs {
        push_unique(&mut key_order, k);
    }

    let mut merged: Vec<(String, Node)> = Vec::with_capacity(key_order.len());
    for key in key_order {
        let b_v = base_find(&key);
        let o_v = find(ours, &key);
        let t_v = find(theirs, &key);

        match (o_v, t_v) {
            (Some(o), Some(t)) => {
                path.push(key.clone());
                let b = b_v.unwrap_or(Node::Null);
                let child = merge_node(&b, &o, &t, path, conflicts);
                path.pop();
                merged.push((key, child));
            }
            (Some(o), None) => {
                // Theirs doesn't have the key. Either:
                //   - theirs deleted it (present in base, unchanged in ours) → drop
                //   - theirs never saw it because ours added it → keep ours
                //   - ours modified and theirs deleted → modify/delete conflict
                match b_v {
                    Some(b) if b == o => { /* theirs deleted an unmodified key — drop */ }
                    Some(_) => {
                        path.push(key.clone());
                        conflicts.push(NodeConflict {
                            path: path.clone(),
                            description: format!(
                                "ours modified {} but theirs deleted it",
                                path_display(path)
                            ),
                        });
                        path.pop();
                        merged.push((key, o));
                    }
                    None => merged.push((key, o)),
                }
            }
            (None, Some(t)) => match b_v {
                Some(b) if b == t => { /* ours deleted an unmodified key — drop */ }
                Some(_) => {
                    path.push(key.clone());
                    conflicts.push(NodeConflict {
                        path: path.clone(),
                        description: format!(
                            "theirs modified {} but ours deleted it",
                            path_display(path)
                        ),
                    });
                    path.pop();
                    merged.push((key, t));
                }
                None => merged.push((key, t)),
            },
            (None, None) => { /* both deleted — drop */ }
        }
    }
    Node::Mapping(merged)
}

fn merge_arrays(
    base: Option<&[Node]>,
    ours: &[Node],
    theirs: &[Node],
    path: &mut Vec<String>,
    conflicts: &mut Vec<NodeConflict>,
) -> Node {
    let all_scalar = |arr: &[Node]| arr.iter().all(is_leaf_scalar);
    if all_scalar(ours) && all_scalar(theirs) && base.is_none_or(all_scalar) {
        return Node::Array(union_scalar_arrays(base, ours, theirs));
    }

    // Positional recursion. If lengths diverge and both sides changed at the
    // same index, that's a real conflict.
    let base_vec: Vec<Node> = base.map(<[Node]>::to_vec).unwrap_or_default();
    let max_len = ours.len().max(theirs.len()).max(base_vec.len());
    let mut out = Vec::with_capacity(max_len);
    for i in 0..max_len {
        let b = base_vec.get(i).cloned().unwrap_or(Node::Null);
        let o = ours.get(i).cloned();
        let t = theirs.get(i).cloned();
        path.push(format!("[{i}]"));
        match (o, t) {
            (Some(o), Some(t)) => {
                out.push(merge_node(&b, &o, &t, path, conflicts));
            }
            (Some(o), None) => {
                if base_vec.get(i) == Some(&o) {
                    // theirs truncated; ours unchanged — drop
                } else {
                    out.push(o);
                }
            }
            (None, Some(t)) => {
                if base_vec.get(i) == Some(&t) {
                    // ours truncated; theirs unchanged — drop
                } else {
                    out.push(t);
                }
            }
            (None, None) => {}
        }
        path.pop();
    }
    Node::Array(out)
}

fn is_leaf_scalar(n: &Node) -> bool {
    matches!(n, Node::Null | Node::Bool(_) | Node::Scalar(_))
}

fn union_scalar_arrays(base: Option<&[Node]>, ours: &[Node], theirs: &[Node]) -> Vec<Node> {
    let base = base.unwrap_or(&[]);
    let mut out: Vec<Node> = Vec::new();
    let push_unique = |out: &mut Vec<Node>, n: &Node| {
        if !out.iter().any(|existing| existing == n) {
            out.push(n.clone());
        }
    };

    // Keep base elements that survived in at least one side.
    for b in base {
        let in_ours = ours.iter().any(|n| n == b);
        let in_theirs = theirs.iter().any(|n| n == b);
        if in_ours && in_theirs {
            push_unique(&mut out, b);
        } else if in_ours ^ in_theirs {
            // One side deleted, the other kept → deletion wins only if the
            // deleting side was the one that changed something else; we take
            // the conservative stance and drop (matches git's default).
        }
    }
    // Append ours' additions (not in base), preserving order.
    for o in ours {
        if !base.iter().any(|b| b == o) {
            push_unique(&mut out, o);
        }
    }
    // Then theirs' additions.
    for t in theirs {
        if !base.iter().any(|b| b == t) && !ours.iter().any(|o| o == t) {
            push_unique(&mut out, t);
        }
    }
    out
}

fn path_display(path: &[String]) -> String {
    if path.is_empty() {
        "<root>".into()
    } else {
        path.join(".")
    }
}

/// Convert a list of [`NodeConflict`]s into [`ConflictDetail`]s with byte-range
/// spans resolved against the provided source bytes. Spans default to the full
/// file when the conflict path cannot be located in bytes.
pub(crate) fn conflicts_to_details(
    path: &Path,
    ours_src: &[u8],
    theirs_src: &[u8],
    base_src: &[u8],
    conflicts: &[NodeConflict],
) -> Vec<ConflictDetail> {
    let placeholder = ChangesetId("unknown".into());
    let file_span = |src: &[u8]| ConflictSpan::from_byte_range(src, 0..src.len());
    conflicts
        .iter()
        .map(|c| {
            let dotted = c.dotted();
            ConflictDetail {
                kind: ConflictKind::BothModifiedSymbol,
                file: path.to_path_buf(),
                symbol_id: Some(SymbolId(format!(
                    "{}::{}",
                    path.display(),
                    if dotted.is_empty() {
                        "<root>".into()
                    } else {
                        dotted
                    }
                ))),
                ours_changeset: placeholder.clone(),
                theirs_changeset: placeholder.clone(),
                description: c.description.clone(),
                ours_span: Some(file_span(ours_src)),
                theirs_span: Some(file_span(theirs_src)),
                base_span: Some(file_span(base_src)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(s: &str) -> Node {
        Node::Scalar(s.into())
    }
    fn m(entries: &[(&str, Node)]) -> Node {
        Node::Mapping(
            entries
                .iter()
                .map(|(k, v)| ((*k).into(), v.clone()))
                .collect(),
        )
    }
    fn a(items: &[Node]) -> Node {
        Node::Array(items.to_vec())
    }

    #[test]
    fn identical_edits_dedupe() {
        let base = m(&[("x", scalar("1"))]);
        let edited = m(&[("x", scalar("2"))]);
        let merged = merge_tree(&base, &edited, &edited).unwrap();
        assert_eq!(merged, edited);
    }

    #[test]
    fn only_ours_changed_takes_ours() {
        let base = m(&[("x", scalar("1"))]);
        let ours = m(&[("x", scalar("2"))]);
        let merged = merge_tree(&base, &ours, &base).unwrap();
        assert_eq!(merged, ours);
    }

    #[test]
    fn disjoint_additive_keys_union() {
        let base = m(&[("a", scalar("1"))]);
        let ours = m(&[("a", scalar("1")), ("b", scalar("2"))]);
        let theirs = m(&[("a", scalar("1")), ("c", scalar("3"))]);
        let merged = merge_tree(&base, &ours, &theirs).unwrap();
        let Node::Mapping(entries) = merged else {
            panic!("expected mapping");
        };
        assert_eq!(entries.len(), 3);
        let keys: Vec<_> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn both_modified_same_scalar_conflicts() {
        let base = m(&[("v", scalar("1.0"))]);
        let ours = m(&[("v", scalar("1.1"))]);
        let theirs = m(&[("v", scalar("1.2"))]);
        let err = merge_tree(&base, &ours, &theirs).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].path, vec!["v"]);
    }

    #[test]
    fn scalar_array_union_dedupes() {
        let base = a(&[scalar("a"), scalar("b")]);
        let ours = a(&[scalar("a"), scalar("b"), scalar("c")]);
        let theirs = a(&[scalar("a"), scalar("b"), scalar("d")]);
        let merged = merge_tree(&base, &ours, &theirs).unwrap();
        let Node::Array(items) = merged else {
            panic!("expected array");
        };
        let texts: Vec<_> = items
            .iter()
            .map(|n| match n {
                Node::Scalar(s) => s.as_str(),
                _ => panic!("not scalar"),
            })
            .collect();
        assert_eq!(texts, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn scalar_array_both_add_same_value_dedupes() {
        let base = a(&[scalar("a")]);
        let both = a(&[scalar("a"), scalar("b")]);
        let merged = merge_tree(&base, &both, &both).unwrap();
        let Node::Array(items) = merged else { panic!() };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn modify_delete_mapping_key_conflicts() {
        let base = m(&[("k", scalar("1"))]);
        let ours = m(&[("k", scalar("2"))]);
        let theirs = m(&[]);
        let err = merge_tree(&base, &ours, &theirs).unwrap_err();
        assert!(err[0].description.contains("deleted"));
    }

    #[test]
    fn both_deleted_key_drops() {
        let base = m(&[("k", scalar("1"))]);
        let deleted = m(&[]);
        let merged = merge_tree(&base, &deleted, &deleted).unwrap();
        assert_eq!(merged, m(&[]));
    }

    #[test]
    fn nested_disjoint_edits_under_same_table() {
        // Mirrors the pyproject.toml repro: both sides add distinct entries
        // under the same nested mapping path.
        let base = m(&[(
            "project",
            m(&[
                ("name", scalar("\"x\"")),
                ("dependencies", a(&[scalar("\"fastapi\"")])),
            ]),
        )]);
        let ours = m(&[(
            "project",
            m(&[
                ("name", scalar("\"x\"")),
                (
                    "dependencies",
                    a(&[scalar("\"fastapi\""), scalar("\"pydantic\"")]),
                ),
            ]),
        )]);
        let theirs = m(&[(
            "project",
            m(&[
                ("name", scalar("\"x\"")),
                (
                    "dependencies",
                    a(&[scalar("\"fastapi\""), scalar("\"rich\"")]),
                ),
            ]),
        )]);
        let merged = merge_tree(&base, &ours, &theirs).unwrap();
        let Node::Mapping(root) = merged else {
            panic!()
        };
        let Some((_, Node::Mapping(project))) = root.iter().find(|(k, _)| k == "project") else {
            panic!()
        };
        let Some((_, Node::Array(deps))) = project.iter().find(|(k, _)| k == "dependencies") else {
            panic!()
        };
        let texts: Vec<_> = deps
            .iter()
            .map(|n| match n {
                Node::Scalar(s) => s.as_str(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(texts, vec!["\"fastapi\"", "\"pydantic\"", "\"rich\""]);
    }
}

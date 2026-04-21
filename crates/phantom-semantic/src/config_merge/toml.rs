//! TOML-aware structured merger.
//!
//! Parses all three versions with `toml_edit` (format-preserving), walks them
//! into the shared [`Node`] tree, runs [`merge_tree`], and re-materializes the
//! merged output back into a `toml_edit::DocumentMut` rooted on ours' document
//! so whitespace, comments, and key order from ours are preserved.

use std::path::Path;
use std::path::PathBuf;

use phantom_core::conflict::MergeResult;
use toml_edit::{Array, Formatted, Item, Table, Value};

use crate::error::SemanticError;

use super::{ConfigMerger, Node, conflicts_to_details, merge_tree};

pub(crate) struct TomlConfigMerger;

impl ConfigMerger for TomlConfigMerger {
    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
    }

    fn merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        path: &Path,
    ) -> Result<MergeResult, SemanticError> {
        let base_doc = parse_doc(path, base)?;
        let ours_doc = parse_doc(path, ours)?;
        let theirs_doc = parse_doc(path, theirs)?;

        let base_node = doc_to_node(&base_doc);
        let ours_node = doc_to_node(&ours_doc);
        let theirs_node = doc_to_node(&theirs_doc);

        match merge_tree(&base_node, &ours_node, &theirs_node) {
            Ok(merged) => {
                let mut out_doc = ours_doc.clone();
                apply_merged(&mut out_doc, &merged);
                Ok(MergeResult::Clean(out_doc.to_string().into_bytes()))
            }
            Err(conflicts) => Ok(MergeResult::Conflict(conflicts_to_details(
                path, ours, theirs, base, &conflicts,
            ))),
        }
    }
}

fn parse_doc(path: &Path, bytes: &[u8]) -> Result<toml_edit::DocumentMut, SemanticError> {
    let text = std::str::from_utf8(bytes).map_err(|e| SemanticError::ParseError {
        path: PathBuf::from(path),
        detail: format!("toml not valid utf-8: {e}"),
    })?;
    text.parse::<toml_edit::DocumentMut>()
        .map_err(|e| SemanticError::ParseError {
            path: PathBuf::from(path),
            detail: format!("toml parse error: {e}"),
        })
}

fn doc_to_node(doc: &toml_edit::DocumentMut) -> Node {
    table_to_node(doc.as_table())
}

fn table_to_node(tbl: &Table) -> Node {
    let mut entries = Vec::with_capacity(tbl.len());
    for (k, item) in tbl {
        entries.push((k.to_string(), item_to_node(item)));
    }
    Node::Mapping(entries)
}

fn item_to_node(item: &Item) -> Node {
    match item {
        Item::None => Node::Null,
        Item::Value(v) => value_to_node(v),
        Item::Table(t) => table_to_node(t),
        Item::ArrayOfTables(arr) => {
            Node::Array(arr.iter().map(table_to_node).collect())
        }
    }
}

fn value_to_node(value: &Value) -> Node {
    match value {
        Value::String(s) => Node::Scalar(format!("{:?}", s.value())),
        Value::Integer(i) => Node::Scalar(i.value().to_string()),
        Value::Float(f) => Node::Scalar(f.value().to_string()),
        Value::Boolean(b) => Node::Bool(*b.value()),
        Value::Datetime(dt) => Node::Scalar(dt.value().to_string()),
        Value::Array(arr) => Node::Array(arr.iter().map(value_to_node).collect()),
        Value::InlineTable(it) => {
            let mut entries = Vec::with_capacity(it.len());
            for (k, v) in it {
                entries.push((k.to_string(), value_to_node(v)));
            }
            Node::Mapping(entries)
        }
    }
}

/// Overwrite `doc`'s tree to match `merged`. Keys present in both keep ours'
/// formatting (toml_edit preserves per-item decor), new keys from theirs are
/// appended, and keys missing from `merged` are removed.
fn apply_merged(doc: &mut toml_edit::DocumentMut, merged: &Node) {
    if let Node::Mapping(entries) = merged {
        let tbl = doc.as_table_mut();
        apply_mapping(tbl, entries);
    }
}

fn apply_mapping(tbl: &mut Table, entries: &[(String, Node)]) {
    // Remove keys no longer present.
    let desired_keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
    let existing_keys: Vec<String> = tbl.iter().map(|(k, _)| k.to_string()).collect();
    for k in existing_keys {
        if !desired_keys.contains(&k.as_str()) {
            tbl.remove(&k);
        }
    }

    // Upsert desired keys in order.
    for (k, v) in entries {
        let preserve_existing = tbl.contains_key(k) && node_matches_existing(tbl.get(k), v);
        if preserve_existing {
            // Keep original decor / typing when the value is unchanged.
            continue;
        }
        if let Some(existing) = tbl.get_mut(k) {
            *existing = node_to_item(v, Some(existing));
        } else {
            tbl.insert(k, node_to_item(v, None));
        }
    }
}

/// Whether the existing item already represents the same logical value as
/// `node`. Avoids churning formatting when the merge picked ours unchanged.
fn node_matches_existing(item: Option<&Item>, node: &Node) -> bool {
    match item {
        Some(i) => item_to_node(i) == *node,
        None => false,
    }
}

fn node_to_item(node: &Node, existing: Option<&mut Item>) -> Item {
    // If the existing item is a Table, try to mutate in place so decor is kept.
    if let (Some(Item::Table(t)), Node::Mapping(entries)) = (existing, node) {
        apply_mapping(t, entries);
        return Item::Table(t.clone());
    }
    match node {
        Node::Null => Item::None,
        Node::Bool(b) => Item::Value(Value::Boolean(Formatted::new(*b))),
        Node::Scalar(s) => Item::Value(scalar_to_value(s)),
        Node::Array(items) => {
            let mut arr = Array::new();
            for n in items {
                arr.push(value_from_node(n));
            }
            Item::Value(Value::Array(arr))
        }
        Node::Mapping(entries) => {
            // Top-level plain tables are serialized as `[section]`; nested
            // mappings use inline tables. We default to a proper Table when
            // we're replacing an Item at table-level.
            let mut tbl = Table::new();
            apply_mapping(&mut tbl, entries);
            Item::Table(tbl)
        }
    }
}

fn value_from_node(node: &Node) -> Value {
    match node {
        Node::Null => Value::String(Formatted::new(String::new())),
        Node::Bool(b) => Value::Boolean(Formatted::new(*b)),
        Node::Scalar(s) => scalar_to_value(s),
        Node::Array(items) => {
            let mut arr = Array::new();
            for n in items {
                arr.push(value_from_node(n));
            }
            Value::Array(arr)
        }
        Node::Mapping(entries) => {
            let mut it = toml_edit::InlineTable::new();
            for (k, v) in entries {
                it.insert(k, value_from_node(v));
            }
            Value::InlineTable(it)
        }
    }
}

/// Re-parse a serialized scalar to recover its original type.
///
/// The `Node::Scalar` strings we emit were produced by [`value_to_node`] —
/// integers as decimal literals, floats as their `f64` repr, strings quoted.
/// We parse them back through `toml_edit` so the output has the right TOML
/// type rather than quoting everything as a string.
fn scalar_to_value(s: &str) -> Value {
    let probe = format!("_x_ = {s}");
    if let Ok(doc) = probe.parse::<toml_edit::DocumentMut>()
        && let Some(Item::Value(v)) = doc.get("_x_")
    {
        return v.clone();
    }
    // Fall back to a plain string.
    Value::String(Formatted::new(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn merge(base: &str, ours: &str, theirs: &str) -> MergeResult {
        TomlConfigMerger
            .merge(
                base.as_bytes(),
                ours.as_bytes(),
                theirs.as_bytes(),
                Path::new("pyproject.toml"),
            )
            .unwrap()
    }

    #[test]
    fn pep621_dep_additions_merge() {
        let base = r#"[project]
name = "x"
dependencies = ["fastapi"]
"#;
        let ours = r#"[project]
name = "x"
dependencies = ["fastapi", "pydantic"]
"#;
        let theirs = r#"[project]
name = "x"
dependencies = ["fastapi", "rich"]
"#;
        let res = merge(base, ours, theirs);
        let MergeResult::Clean(bytes) = res else {
            panic!("expected clean merge, got {res:?}");
        };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("fastapi"));
        assert!(text.contains("pydantic"));
        assert!(text.contains("rich"));
    }

    #[test]
    fn disjoint_tables_merge() {
        let base = r#"[a]
x = 1
"#;
        let ours = r#"[a]
x = 1

[b]
y = 2
"#;
        let theirs = r#"[a]
x = 1

[c]
z = 3
"#;
        let MergeResult::Clean(bytes) = merge(base, ours, theirs) else {
            panic!("expected clean");
        };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("[a]"));
        assert!(text.contains("[b]"));
        assert!(text.contains("[c]"));
    }

    #[test]
    fn version_bump_conflict() {
        let base = r#"[project]
version = "1.0"
"#;
        let ours = r#"[project]
version = "1.1"
"#;
        let theirs = r#"[project]
version = "2.0"
"#;
        let res = merge(base, ours, theirs);
        let MergeResult::Conflict(details) = res else {
            panic!("expected conflict");
        };
        assert_eq!(details.len(), 1);
        assert!(
            details[0]
                .description
                .to_lowercase()
                .contains("version")
        );
    }

    #[test]
    fn identical_edits_dedupe() {
        let base = r#"x = 1
"#;
        let same = r#"x = 2
"#;
        let res = merge(base, same, same);
        let MergeResult::Clean(bytes) = res else {
            panic!("expected clean");
        };
        assert!(String::from_utf8(bytes).unwrap().contains("x = 2"));
    }

    #[test]
    fn nested_dep_addition_in_table_form() {
        // `[project.dependencies]` as a table (not inline array).
        let base = r#"[project]
name = "x"

[project.dependencies]
serde = "1"
"#;
        let ours = r#"[project]
name = "x"

[project.dependencies]
serde = "1"
anyhow = "1"
"#;
        let theirs = r#"[project]
name = "x"

[project.dependencies]
serde = "1"
tokio = "1"
"#;
        let MergeResult::Clean(bytes) = merge(base, ours, theirs) else {
            panic!("expected clean");
        };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("serde"));
        assert!(text.contains("anyhow"));
        assert!(text.contains("tokio"));
    }
}

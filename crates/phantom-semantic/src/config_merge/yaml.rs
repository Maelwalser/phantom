//! YAML-aware structured merger.
//!
//! Uses `serde_yaml_ng`'s `Mapping` (which preserves insertion order) as a
//! convenient structured representation. Comments are not round-tripped
//! (serde_yaml_ng doesn't preserve them), but structure / key order / string
//! quoting style are preserved.

use std::path::Path;
use std::path::PathBuf;

use phantom_core::conflict::MergeResult;
use serde_yaml_ng::Value;

use crate::error::SemanticError;

use super::{ConfigMerger, Node, conflicts_to_details, merge_tree};

pub(crate) struct YamlConfigMerger;

impl ConfigMerger for YamlConfigMerger {
    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
    }

    fn merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        path: &Path,
    ) -> Result<MergeResult, SemanticError> {
        let base_node = parse(path, base)?;
        let ours_node = parse(path, ours)?;
        let theirs_node = parse(path, theirs)?;

        match merge_tree(&base_node, &ours_node, &theirs_node) {
            Ok(merged) => {
                let value = node_to_value(&merged);
                let text = serde_yaml_ng::to_string(&value).map_err(|e| {
                    SemanticError::MergeError(format!("yaml serialize failed: {e}"))
                })?;
                Ok(MergeResult::Clean(text.into_bytes()))
            }
            Err(conflicts) => Ok(MergeResult::Conflict(conflicts_to_details(
                path, ours, theirs, base, &conflicts,
            ))),
        }
    }
}

fn parse(path: &Path, bytes: &[u8]) -> Result<Node, SemanticError> {
    let text = std::str::from_utf8(bytes).map_err(|e| SemanticError::ParseError {
        path: PathBuf::from(path),
        detail: format!("yaml not valid utf-8: {e}"),
    })?;
    if text.trim().is_empty() {
        return Ok(Node::Null);
    }
    let value: Value = serde_yaml_ng::from_str(text).map_err(|e| {
        SemanticError::ParseError {
            path: PathBuf::from(path),
            detail: format!("yaml parse error: {e}"),
        }
    })?;
    Ok(value_to_node(&value))
}

fn value_to_node(v: &Value) -> Node {
    match v {
        Value::Null => Node::Null,
        Value::Bool(b) => Node::Bool(*b),
        Value::Number(n) => Node::Scalar(n.to_string()),
        Value::String(s) => Node::Scalar(s.clone()),
        Value::Sequence(seq) => Node::Array(seq.iter().map(value_to_node).collect()),
        Value::Mapping(map) => {
            let mut entries = Vec::with_capacity(map.len());
            for (k, v) in map {
                let key = match k {
                    Value::String(s) => s.clone(),
                    // Non-string keys are rare in config-style YAML; coerce.
                    other => serde_yaml_ng::to_string(other)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                };
                entries.push((key, value_to_node(v)));
            }
            Node::Mapping(entries)
        }
        Value::Tagged(t) => value_to_node(&t.value),
    }
}

fn node_to_value(node: &Node) -> Value {
    match node {
        Node::Null => Value::Null,
        Node::Bool(b) => Value::Bool(*b),
        Node::Scalar(s) => {
            // Try to round-trip as the best typed value.
            if let Ok(parsed) = serde_yaml_ng::from_str::<Value>(s) {
                // Never coerce a non-string YAML document that would evaluate
                // to a mapping or sequence — those must stay literal strings.
                match parsed {
                    Value::Null | Value::Bool(_) | Value::Number(_) => return parsed,
                    _ => {}
                }
            }
            Value::String(s.clone())
        }
        Node::Array(items) => Value::Sequence(items.iter().map(node_to_value).collect()),
        Node::Mapping(entries) => {
            let mut map = serde_yaml_ng::Mapping::with_capacity(entries.len());
            for (k, v) in entries {
                map.insert(Value::String(k.clone()), node_to_value(v));
            }
            Value::Mapping(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn merge(base: &str, ours: &str, theirs: &str) -> MergeResult {
        YamlConfigMerger
            .merge(
                base.as_bytes(),
                ours.as_bytes(),
                theirs.as_bytes(),
                Path::new("docker-compose.yml"),
            )
            .unwrap()
    }

    #[test]
    fn disjoint_services_merge() {
        let base = r"services:
  web:
    image: nginx
";
        let ours = r"services:
  web:
    image: nginx
  api:
    image: api:1.0
";
        let theirs = r"services:
  web:
    image: nginx
  worker:
    image: worker:1.0
";
        let MergeResult::Clean(bytes) = merge(base, ours, theirs) else {
            panic!("expected clean");
        };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("web"));
        assert!(text.contains("api"));
        assert!(text.contains("worker"));
    }

    #[test]
    fn real_scalar_conflict() {
        let base = "version: \"1.0\"\n";
        let ours = "version: \"1.1\"\n";
        let theirs = "version: \"2.0\"\n";
        let res = merge(base, ours, theirs);
        let MergeResult::Conflict(details) = res else {
            panic!("expected conflict");
        };
        assert_eq!(details.len(), 1);
    }

    #[test]
    fn additive_list_entries_union() {
        let base = "deps: [a, b]\n";
        let ours = "deps: [a, b, c]\n";
        let theirs = "deps: [a, b, d]\n";
        let MergeResult::Clean(bytes) = merge(base, ours, theirs) else {
            panic!("expected clean");
        };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains('c'));
        assert!(text.contains('d'));
    }
}

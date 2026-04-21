//! JSON-aware structured merger.
//!
//! Uses `serde_json::Value` with a key-ordered intermediate representation so
//! the merged output preserves the insertion order readers expect (package.json
//! lists scripts / deps / devDeps in a conventional order).

use std::path::Path;
use std::path::PathBuf;

use phantom_core::conflict::MergeResult;

use crate::error::SemanticError;

use super::{ConfigMerger, Node, conflicts_to_details, merge_tree};

pub(crate) struct JsonConfigMerger;

impl ConfigMerger for JsonConfigMerger {
    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
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
            Ok(merged) => Ok(MergeResult::Clean(serialize(&merged, ours))),
            Err(conflicts) => Ok(MergeResult::Conflict(conflicts_to_details(
                path, ours, theirs, base, &conflicts,
            ))),
        }
    }
}

fn parse(path: &Path, bytes: &[u8]) -> Result<Node, SemanticError> {
    let text = std::str::from_utf8(bytes).map_err(|e| SemanticError::ParseError {
        path: PathBuf::from(path),
        detail: format!("json not valid utf-8: {e}"),
    })?;
    if text.trim().is_empty() {
        return Ok(Node::Null);
    }
    // serde_json::Value is a BTreeMap by default which loses key order. We
    // parse via raw tokens instead to keep insertion order.
    let raw: serde_json::Value =
        serde_json::from_str(text).map_err(|e| SemanticError::ParseError {
            path: PathBuf::from(path),
            detail: format!("json parse error: {e}"),
        })?;
    Ok(value_to_node(&raw, text))
}

/// Convert `serde_json::Value` into our `Node` tree. We re-walk the raw text
/// to preserve key ordering since `serde_json::Map` (without the
/// `preserve_order` feature) reorders keys alphabetically.
fn value_to_node(v: &serde_json::Value, raw_text: &str) -> Node {
    ordered_parse(raw_text).unwrap_or_else(|_| fallback_node(v))
}

fn fallback_node(v: &serde_json::Value) -> Node {
    match v {
        serde_json::Value::Null => Node::Null,
        serde_json::Value::Bool(b) => Node::Bool(*b),
        serde_json::Value::Number(n) => Node::Scalar(n.to_string()),
        serde_json::Value::String(s) => Node::Scalar(format!("{s:?}")),
        serde_json::Value::Array(items) => Node::Array(items.iter().map(fallback_node).collect()),
        serde_json::Value::Object(map) => Node::Mapping(
            map.iter()
                .map(|(k, v)| (k.clone(), fallback_node(v)))
                .collect(),
        ),
    }
}

/// Tokenize the JSON text manually just enough to preserve key ordering in
/// objects. For arrays and scalars we defer to `serde_json` by round-tripping
/// the surrounding text.
fn ordered_parse(text: &str) -> Result<Node, SemanticError> {
    let mut parser = JsonOrderedParser::new(text);
    parser.skip_ws();
    let node = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos < parser.src.len() {
        return Err(SemanticError::MergeError(
            "trailing content after JSON value".into(),
        ));
    }
    Ok(node)
}

struct JsonOrderedParser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> JsonOrderedParser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            src: text.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.src.len()
            && matches!(self.src[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn parse_value(&mut self) -> Result<Node, SemanticError> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => {
                let start = self.pos;
                self.consume_string()?;
                Ok(Node::Scalar(
                    std::str::from_utf8(&self.src[start..self.pos])
                        .map_err(|e| SemanticError::MergeError(e.to_string()))?
                        .to_string(),
                ))
            }
            Some(b't' | b'f') => {
                let start = self.pos;
                while self.pos < self.src.len()
                    && (self.src[self.pos] as char).is_ascii_alphabetic()
                {
                    self.pos += 1;
                }
                let word = &self.src[start..self.pos];
                match word {
                    b"true" => Ok(Node::Bool(true)),
                    b"false" => Ok(Node::Bool(false)),
                    other => Err(SemanticError::MergeError(format!(
                        "unexpected token: {:?}",
                        std::str::from_utf8(other).unwrap_or("?")
                    ))),
                }
            }
            Some(b'n') => {
                if self.src[self.pos..].starts_with(b"null") {
                    self.pos += 4;
                    Ok(Node::Null)
                } else {
                    Err(SemanticError::MergeError("expected null".into()))
                }
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                let start = self.pos;
                while self.pos < self.src.len()
                    && !matches!(
                        self.src[self.pos],
                        b',' | b'}' | b']' | b' ' | b'\n' | b'\t' | b'\r'
                    )
                {
                    self.pos += 1;
                }
                Ok(Node::Scalar(
                    std::str::from_utf8(&self.src[start..self.pos])
                        .map_err(|e| SemanticError::MergeError(e.to_string()))?
                        .to_string(),
                ))
            }
            _ => Err(SemanticError::MergeError("unexpected token".into())),
        }
    }

    fn parse_object(&mut self) -> Result<Node, SemanticError> {
        self.expect(b'{')?;
        self.skip_ws();
        let mut entries: Vec<(String, Node)> = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Node::Mapping(entries));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string_content()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Node::Mapping(entries));
                }
                _ => {
                    return Err(SemanticError::MergeError(
                        "expected ',' or '}' in object".into(),
                    ));
                }
            }
        }
    }

    fn parse_array(&mut self) -> Result<Node, SemanticError> {
        self.expect(b'[')?;
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Node::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Node::Array(items));
                }
                _ => {
                    return Err(SemanticError::MergeError(
                        "expected ',' or ']' in array".into(),
                    ));
                }
            }
        }
    }

    fn expect(&mut self, b: u8) -> Result<(), SemanticError> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(SemanticError::MergeError(format!(
                "expected '{}'",
                b as char
            )))
        }
    }

    fn consume_string(&mut self) -> Result<(), SemanticError> {
        if self.peek() != Some(b'"') {
            return Err(SemanticError::MergeError("expected string".into()));
        }
        self.pos += 1;
        while self.pos < self.src.len() {
            match self.src[self.pos] {
                b'"' => {
                    self.pos += 1;
                    return Ok(());
                }
                b'\\' => self.pos += 2,
                _ => self.pos += 1,
            }
        }
        Err(SemanticError::MergeError("unterminated string".into()))
    }

    fn parse_string_content(&mut self) -> Result<String, SemanticError> {
        let start = self.pos;
        self.consume_string()?;
        let raw = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|e| SemanticError::MergeError(format!("invalid utf-8 in key: {e}")))?;
        let value: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| SemanticError::MergeError(format!("invalid json string: {e}")))?;
        match value {
            serde_json::Value::String(s) => Ok(s),
            _ => Err(SemanticError::MergeError(
                "object key must be string".into(),
            )),
        }
    }
}

/// Serialize a [`Node`] as JSON, matching ours' indentation style when
/// possible. Falls back to a standard 2-space indent.
fn serialize(node: &Node, ours: &[u8]) -> Vec<u8> {
    let indent = detect_indent(ours).unwrap_or_else(|| "  ".to_string());
    let trailing_newline = ours.last() == Some(&b'\n');
    let mut out = String::new();
    write_node(&mut out, node, 0, &indent);
    if trailing_newline && !out.ends_with('\n') {
        out.push('\n');
    }
    out.into_bytes()
}

fn detect_indent(src: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(src).ok()?;
    for line in text.lines().skip(1) {
        let leading: String = line.chars().take_while(|c| *c == ' ').collect();
        if !leading.is_empty() {
            return Some(leading);
        }
    }
    None
}

fn write_node(out: &mut String, node: &Node, depth: usize, indent: &str) {
    match node {
        Node::Null => out.push_str("null"),
        Node::Bool(true) => out.push_str("true"),
        Node::Bool(false) => out.push_str("false"),
        Node::Scalar(s) => out.push_str(s),
        Node::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            out.push('\n');
            for (i, item) in items.iter().enumerate() {
                push_indent(out, depth + 1, indent);
                write_node(out, item, depth + 1, indent);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, depth, indent);
            out.push(']');
        }
        Node::Mapping(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            out.push('\n');
            for (i, (k, v)) in entries.iter().enumerate() {
                push_indent(out, depth + 1, indent);
                out.push('"');
                for c in k.chars() {
                    match c {
                        '\\' => out.push_str("\\\\"),
                        '"' => out.push_str("\\\""),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        other => out.push(other),
                    }
                }
                out.push_str("\": ");
                write_node(out, v, depth + 1, indent);
                if i + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            push_indent(out, depth, indent);
            out.push('}');
        }
    }
}

fn push_indent(out: &mut String, depth: usize, indent: &str) {
    for _ in 0..depth {
        out.push_str(indent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn pyproject_like_additive_array_merges() {
        let base = br#"{"deps": ["a", "b"]}"#;
        let ours = br#"{"deps": ["a", "b", "c"]}"#;
        let theirs = br#"{"deps": ["a", "b", "d"]}"#;
        let res = JsonConfigMerger
            .merge(base, ours, theirs, Path::new("package.json"))
            .unwrap();
        let MergeResult::Clean(bytes) = res else {
            panic!("expected clean");
        };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"a\""));
        assert!(text.contains("\"b\""));
        assert!(text.contains("\"c\""));
        assert!(text.contains("\"d\""));
    }

    #[test]
    fn disjoint_new_top_level_keys_merge() {
        let base = br#"{"name": "x"}"#;
        let ours = br#"{"name": "x", "version": "1.0"}"#;
        let theirs = br#"{"name": "x", "author": "y"}"#;
        let res = JsonConfigMerger
            .merge(base, ours, theirs, Path::new("package.json"))
            .unwrap();
        let MergeResult::Clean(bytes) = res else {
            panic!("expected clean, got {res:?}");
        };
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"version\""));
        assert!(text.contains("\"author\""));
    }

    #[test]
    fn real_value_conflict_returns_conflict() {
        let base = br#"{"version": "1.0"}"#;
        let ours = br#"{"version": "1.1"}"#;
        let theirs = br#"{"version": "2.0"}"#;
        let res = JsonConfigMerger
            .merge(base, ours, theirs, Path::new("package.json"))
            .unwrap();
        let MergeResult::Conflict(details) = res else {
            panic!("expected conflict");
        };
        assert_eq!(details.len(), 1);
        assert!(details[0].description.contains("version"));
    }

    #[test]
    fn preserves_key_order_and_indent() {
        let base = b"{\n  \"a\": 1,\n  \"b\": 2\n}\n";
        let ours = b"{\n  \"a\": 1,\n  \"b\": 2,\n  \"c\": 3\n}\n";
        let res = JsonConfigMerger
            .merge(base, ours, base, Path::new("p.json"))
            .unwrap();
        let MergeResult::Clean(bytes) = res else {
            panic!()
        };
        let text = String::from_utf8(bytes).unwrap();
        // Key order should be a, b, c.
        let a = text.find("\"a\"").unwrap();
        let b = text.find("\"b\"").unwrap();
        let c = text.find("\"c\"").unwrap();
        assert!(a < b && b < c, "got: {text}");
    }
}

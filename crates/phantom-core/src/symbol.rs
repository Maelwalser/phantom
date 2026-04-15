//! Symbol types extracted from source files via tree-sitter.
//!
//! A [`SymbolEntry`] represents a single named declaration (function, struct,
//! import, etc.) together with its location and a content hash for fast change
//! detection.

use std::ops::Range;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{ContentHash, SymbolId};

/// Find the smallest symbol whose `byte_range` fully contains `target`.
///
/// When multiple symbols enclose the range (e.g., a method inside an impl
/// block), returns the tightest (smallest span) enclosing symbol.
pub fn find_enclosing_symbol<'a>(
    symbols: &'a [SymbolEntry],
    target: &Range<usize>,
) -> Option<&'a SymbolEntry> {
    symbols
        .iter()
        .filter(|s| s.byte_range.start <= target.start && s.byte_range.end >= target.end)
        .min_by_key(|s| s.byte_range.end - s.byte_range.start)
}

/// The syntactic category of a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    /// A standalone function or free function.
    Function,
    /// A struct definition.
    Struct,
    /// An enum definition.
    Enum,
    /// A trait definition (Rust).
    Trait,
    /// An impl block (Rust).
    Impl,
    /// An import / use statement.
    Import,
    /// A constant binding.
    Const,
    /// A type alias.
    TypeAlias,
    /// A module declaration.
    Module,
    /// A test function.
    Test,
    /// A class definition (TypeScript / Python / Go).
    Class,
    /// An interface definition (TypeScript / Go).
    Interface,
    /// A method within a class or impl block.
    Method,
    /// A config section (YAML top-level key, TOML table, JSON object key, HCL block, CSS rule).
    Section,
    /// A standalone directive (Dockerfile instruction, shell command, CSS @-rule).
    Directive,
    /// A variable assignment (shell export, Makefile variable).
    Variable,
}

/// A single symbol extracted from a source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolEntry {
    /// Unique identity in `"scope::name::kind"` format.
    pub id: SymbolId,
    /// Syntactic category.
    pub kind: SymbolKind,
    /// Short name of the symbol (e.g. `"handle_login"`).
    pub name: String,
    /// Fully-qualified scope (e.g. `"crate::handlers"`).
    pub scope: String,
    /// Path of the source file relative to the repository root.
    pub file: PathBuf,
    /// Byte range within the file that this symbol spans.
    pub byte_range: Range<usize>,
    /// BLAKE3 hash of the symbol's source text.
    pub content_hash: ContentHash,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> SymbolEntry {
        SymbolEntry {
            id: SymbolId("crate::handlers::login::Function".into()),
            kind: SymbolKind::Function,
            name: "login".into(),
            scope: "crate::handlers".into(),
            file: PathBuf::from("src/handlers.rs"),
            byte_range: 100..250,
            content_hash: ContentHash::from_bytes(b"fn login() {}"),
        }
    }

    #[test]
    fn serde_symbol_kind_roundtrip() {
        let kind = SymbolKind::Trait;
        let json = serde_json::to_string(&kind).unwrap();
        let back: SymbolKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }

    #[test]
    fn serde_symbol_entry_roundtrip() {
        let entry = sample_entry();
        let json = serde_json::to_string(&entry).unwrap();
        let back: SymbolEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }

    fn make_symbol(name: &str, kind: SymbolKind, byte_range: Range<usize>) -> SymbolEntry {
        SymbolEntry {
            id: SymbolId(format!("test::{name}::{kind:?}")),
            kind,
            name: name.into(),
            scope: "test".into(),
            file: PathBuf::from("test.rs"),
            byte_range,
            content_hash: ContentHash::from_bytes(name.as_bytes()),
        }
    }

    #[test]
    fn find_enclosing_symbol_returns_tightest() {
        let symbols = vec![
            make_symbol("MyImpl", SymbolKind::Impl, 0..500),
            make_symbol("inner_method", SymbolKind::Method, 50..200),
        ];
        let target = 100..150;
        let result = find_enclosing_symbol(&symbols, &target);
        assert_eq!(result.unwrap().name, "inner_method");
    }

    #[test]
    fn find_enclosing_symbol_exact_match() {
        let symbols = vec![make_symbol("foo", SymbolKind::Function, 10..50)];
        let target = 10..50;
        let result = find_enclosing_symbol(&symbols, &target);
        assert_eq!(result.unwrap().name, "foo");
    }

    #[test]
    fn find_enclosing_symbol_none_when_no_match() {
        let symbols = vec![make_symbol("foo", SymbolKind::Function, 10..50)];
        let target = 60..80;
        assert!(find_enclosing_symbol(&symbols, &target).is_none());
    }
}

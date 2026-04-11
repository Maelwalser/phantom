//! Symbol types extracted from source files via tree-sitter.
//!
//! A [`SymbolEntry`] represents a single named declaration (function, struct,
//! import, etc.) together with its location and a content hash for fast change
//! detection.

use std::ops::Range;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{ContentHash, SymbolId};

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
}

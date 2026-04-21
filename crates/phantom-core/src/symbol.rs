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

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Function => "fn",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Impl => "impl",
            Self::Import => "import",
            Self::Const => "const",
            Self::TypeAlias => "type",
            Self::Module => "mod",
            Self::Test => "test",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Method => "method",
            Self::Section => "section",
            Self::Directive => "directive",
            Self::Variable => "var",
        };
        f.write_str(s)
    }
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
    /// BLAKE3 hash of the symbol's *declaration* (signature, not body).
    ///
    /// For a function, this covers `fn name(args) -> ret` but not the body.
    /// For a struct, the name and field declarations. When this hash differs
    /// between two versions of the same symbol, the change is likely
    /// API-breaking for dependents; when only `content_hash` differs, the
    /// change is body-only and typically safe for dependents.
    ///
    /// Defaults to the all-zeros sentinel when an extractor does not compute
    /// a signature (unsupported kind) or when deserializing older payloads
    /// that predate this field. Consumers must treat a zero hash as "unknown"
    /// and fall back to `content_hash` comparison.
    #[serde(default = "ContentHash::zero")]
    pub signature_hash: ContentHash,
}

/// The kind of reference a symbol makes to another symbol.
///
/// Used to build the semantic dependency graph. Each captured reference
/// becomes an edge `source → target` in the graph with one of these kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReferenceKind {
    /// Function call or method invocation (`foo()`, `x.bar()`).
    Call,
    /// Type appears in a type position (`let x: MyType`, `fn f() -> Result<T>`).
    TypeUse,
    /// Import / use / `from X import Y`.
    Import,
    /// Struct field access or enum variant reference.
    FieldAccess,
    /// `impl Trait for Type` — target is the trait.
    TraitImpl,
}

impl std::fmt::Display for ReferenceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Call => "call",
            Self::TypeUse => "type",
            Self::Import => "import",
            Self::FieldAccess => "field",
            Self::TraitImpl => "impl",
        };
        f.write_str(s)
    }
}

/// An unresolved reference from one symbol to another.
///
/// Emitted by language extractors during tree-sitter traversal. Resolution
/// to a concrete `SymbolId` target happens later in `DependencyGraph::update_file`
/// using a [`SymbolIndex`](crate::traits::SymbolIndex) and a name+scope heuristic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolReference {
    /// The symbol containing the reference (the "caller").
    ///
    /// For references at module scope (imports, top-level type aliases) that
    /// don't live inside any enclosing symbol, extractors emit a synthetic
    /// file-level source with `kind = SymbolKind::Module` and a `__file__`
    /// name component.
    pub source: SymbolId,
    /// The short name of the referenced symbol (e.g. `"login"`).
    pub target_name: String,
    /// An optional scope hint parsed from a qualified path
    /// (e.g. `crate::auth::login` → `Some("crate::auth")`).
    ///
    /// When `Some`, the resolver prefers candidates whose scope matches.
    /// When `None`, the resolver falls back to name-only matching.
    pub target_scope_hint: Option<String>,
    /// The kind of reference (call / type-use / import / field / impl).
    pub kind: ReferenceKind,
    /// Path of the source file (where the reference appears).
    pub file: PathBuf,
    /// Byte range of the reference node in the source file.
    pub byte_range: Range<usize>,
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
            signature_hash: ContentHash::from_bytes(b"fn login()"),
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
            signature_hash: ContentHash::from_bytes(name.as_bytes()),
        }
    }

    #[test]
    fn serde_symbol_entry_decodes_without_signature_hash() {
        // Older events written before signature_hash was introduced should
        // deserialize with the zero sentinel rather than erroring out.
        let legacy_json = r#"{
            "id": "crate::foo::Function",
            "kind": "Function",
            "name": "foo",
            "scope": "crate",
            "file": "src/foo.rs",
            "byte_range": {"start": 0, "end": 10},
            "content_hash": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]
        }"#;
        let entry: SymbolEntry = serde_json::from_str(legacy_json).unwrap();
        assert!(entry.signature_hash.is_zero());
    }

    #[test]
    fn serde_symbol_reference_roundtrip() {
        let r = SymbolReference {
            source: SymbolId("crate::caller::Function".into()),
            target_name: "login".into(),
            target_scope_hint: Some("crate::auth".into()),
            kind: ReferenceKind::Call,
            file: PathBuf::from("src/caller.rs"),
            byte_range: 42..47,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: SymbolReference = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn reference_kind_display() {
        assert_eq!(ReferenceKind::Call.to_string(), "call");
        assert_eq!(ReferenceKind::TypeUse.to_string(), "type");
        assert_eq!(ReferenceKind::Import.to_string(), "import");
        assert_eq!(ReferenceKind::FieldAccess.to_string(), "field");
        assert_eq!(ReferenceKind::TraitImpl.to_string(), "impl");
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

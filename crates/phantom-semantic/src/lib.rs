//! `phantom-semantic` — semantic index, tree-sitter parsing, and merge engine.
//!
//! Implements [`phantom_core::SymbolIndex`] and [`phantom_core::SemanticAnalyzer`]
//! using tree-sitter grammars for Rust, TypeScript, Python, and Go.

pub(crate) mod config_merge;
pub(crate) mod diff;
pub mod error;
pub(crate) mod graph;
pub(crate) mod index;
pub(crate) mod languages;
pub(crate) mod merge;
pub(crate) mod parser;

pub use error::SemanticError;
pub use graph::InMemoryDependencyGraph;
pub use index::InMemorySymbolIndex;
pub use languages::LanguageExtractor;
pub use merge::SemanticMerger;
pub use parser::Parser;

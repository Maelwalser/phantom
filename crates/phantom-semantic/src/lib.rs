//! `phantom-semantic` — semantic index, tree-sitter parsing, and merge engine.
//!
//! Implements [`phantom_core::SymbolIndex`] and [`phantom_core::SemanticAnalyzer`]
//! using tree-sitter grammars for Rust, TypeScript, Python, and Go.

pub mod diff;
pub mod error;
pub mod index;
pub mod languages;
pub mod merge;
pub mod parser;

pub use error::SemanticError;
pub use index::InMemorySymbolIndex;
pub use merge::SemanticMerger;
pub use parser::Parser;

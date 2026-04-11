//! `phantom-semantic` — semantic index, tree-sitter parsing, and merge engine.
//!
//! Implements [`phantom_core::SymbolIndex`] and [`phantom_core::SemanticAnalyzer`]
//! using tree-sitter grammars for Rust, TypeScript, Python, and Go.

pub mod diff;
pub mod index;
pub mod languages;
pub mod merge;
pub mod parser;

//! Three-way semantic merge engine.
//!
//! Implements [`phantom_core::traits::SemanticAnalyzer`] using tree-sitter
//! parsing and Weave-style entity matching.

use std::collections::HashMap;
use std::path::Path;

use phantom_core::changeset::SemanticOperation;
use phantom_core::conflict::{ConflictDetail, ConflictKind, ConflictSpan};
use phantom_core::error::CoreError;
use phantom_core::id::ChangesetId;
use phantom_core::is_binary_or_non_utf8;
use phantom_core::symbol::SymbolEntry;
use phantom_core::traits::{MergeResult, SemanticAnalyzer};

use crate::diff::{self, EntityKey, entity_key};
use crate::parser::Parser;

/// Semantic merge engine backed by tree-sitter.
pub struct SemanticMerger {
    parser: Parser,
}

impl SemanticMerger {
    /// Create a new merger with the default parser.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
        }
    }
}

impl Default for SemanticMerger {
    fn default() -> Self {
        Self::new()
    }
}

impl phantom_core::traits::SemanticAnalyzer for SemanticMerger {
    fn extract_symbols(&self, path: &Path, content: &[u8]) -> Result<Vec<SymbolEntry>, CoreError> {
        self.parser
            .parse_file(path, content)
            .map_err(|e| CoreError::Semantic(e.to_string()))
    }

    fn diff_symbols(
        &self,
        base: &[SymbolEntry],
        current: &[SymbolEntry],
    ) -> Vec<SemanticOperation> {
        let file = base
            .first()
            .or(current.first())
            .map(|e| e.file.as_path())
            .unwrap_or(Path::new("unknown"));
        diff::diff_symbols(base, current, file)
    }

    fn three_way_merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        path: &Path,
    ) -> Result<MergeResult, CoreError> {
        // If content is identical, short-circuit
        if ours == theirs {
            return Ok(MergeResult::Clean(ours.to_vec()));
        }
        if ours == base {
            return Ok(MergeResult::Clean(theirs.to_vec()));
        }
        if theirs == base {
            return Ok(MergeResult::Clean(ours.to_vec()));
        }

        // Try semantic merge if language is supported
        if self.parser.supports_language(path) {
            match self.semantic_merge(base, ours, theirs, path) {
                Ok(result) => return Ok(result),
                Err(_) => {
                    // Fall through to text-based merge
                    tracing::warn!(?path, "semantic merge failed, falling back to text merge");
                }
            }
        }

        // Fallback: line-based three-way merge
        text_merge(base, ours, theirs, path)
    }
}

impl SemanticMerger {
    /// Attempt a semantic three-way merge using symbol-level analysis.
    fn semantic_merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        path: &Path,
    ) -> Result<MergeResult, CoreError> {
        let base_symbols = self.extract_symbols(path, base)?;
        let ours_symbols = self.extract_symbols(path, ours)?;
        let theirs_symbols = self.extract_symbols(path, theirs)?;

        let base_map: HashMap<EntityKey, &SymbolEntry> =
            base_symbols.iter().map(|e| (entity_key(e), e)).collect();
        let ours_map: HashMap<EntityKey, &SymbolEntry> =
            ours_symbols.iter().map(|e| (entity_key(e), e)).collect();
        let theirs_map: HashMap<EntityKey, &SymbolEntry> =
            theirs_symbols.iter().map(|e| (entity_key(e), e)).collect();

        let mut conflicts = Vec::new();
        let placeholder_cs = ChangesetId("unknown".into());

        // Check for conflicts
        for (key, ours_entry) in &ours_map {
            if let Some(theirs_entry) = theirs_map.get(key) {
                if let Some(base_entry) = base_map.get(key) {
                    let ours_changed = ours_entry.content_hash != base_entry.content_hash;
                    let theirs_changed = theirs_entry.content_hash != base_entry.content_hash;
                    if ours_changed && theirs_changed {
                        // Both modified same symbol
                        if ours_entry.content_hash != theirs_entry.content_hash {
                            conflicts.push(ConflictDetail {
                                kind: ConflictKind::BothModifiedSymbol,
                                file: path.to_path_buf(),
                                symbol_id: Some(ours_entry.id.clone()),
                                ours_changeset: placeholder_cs.clone(),
                                theirs_changeset: placeholder_cs.clone(),
                                description: format!(
                                    "both sides modified {}::{}",
                                    ours_entry.scope, ours_entry.name
                                ),
                                ours_span: Some(ConflictSpan::from_byte_range(
                                    ours,
                                    ours_entry.byte_range.clone(),
                                )),
                                theirs_span: Some(ConflictSpan::from_byte_range(
                                    theirs,
                                    theirs_entry.byte_range.clone(),
                                )),
                                base_span: Some(ConflictSpan::from_byte_range(
                                    base,
                                    base_entry.byte_range.clone(),
                                )),
                            });
                        }
                        // If both changed to same content, no conflict (deduplicate)
                    }
                } else {
                    // Both added same-named symbol (not in base)
                    if ours_entry.content_hash != theirs_entry.content_hash {
                        conflicts.push(ConflictDetail {
                            kind: ConflictKind::BothModifiedSymbol,
                            file: path.to_path_buf(),
                            symbol_id: Some(ours_entry.id.clone()),
                            ours_changeset: placeholder_cs.clone(),
                            theirs_changeset: placeholder_cs.clone(),
                            description: format!(
                                "both sides added {}::{} with different content",
                                ours_entry.scope, ours_entry.name
                            ),
                            ours_span: Some(ConflictSpan::from_byte_range(
                                ours,
                                ours_entry.byte_range.clone(),
                            )),
                            theirs_span: Some(ConflictSpan::from_byte_range(
                                theirs,
                                theirs_entry.byte_range.clone(),
                            )),
                            base_span: None,
                        });
                    }
                    // Same content → deduplicate, no conflict
                }
            }
        }

        // Modify-delete conflicts
        for (key, base_entry) in &base_map {
            let in_ours = ours_map.contains_key(key);
            let in_theirs = theirs_map.contains_key(key);

            if in_ours && !in_theirs {
                let ours_entry = ours_map[key];
                if ours_entry.content_hash != base_entry.content_hash {
                    conflicts.push(ConflictDetail {
                        kind: ConflictKind::ModifyDeleteSymbol,
                        file: path.to_path_buf(),
                        symbol_id: Some(base_entry.id.clone()),
                        ours_changeset: placeholder_cs.clone(),
                        theirs_changeset: placeholder_cs.clone(),
                        description: format!(
                            "ours modified {}::{} but theirs deleted it",
                            base_entry.scope, base_entry.name
                        ),
                        ours_span: Some(ConflictSpan::from_byte_range(
                            ours,
                            ours_entry.byte_range.clone(),
                        )),
                        theirs_span: None,
                        base_span: Some(ConflictSpan::from_byte_range(
                            base,
                            base_entry.byte_range.clone(),
                        )),
                    });
                }
            } else if !in_ours && in_theirs {
                let theirs_entry = theirs_map[key];
                if theirs_entry.content_hash != base_entry.content_hash {
                    conflicts.push(ConflictDetail {
                        kind: ConflictKind::ModifyDeleteSymbol,
                        file: path.to_path_buf(),
                        symbol_id: Some(base_entry.id.clone()),
                        ours_changeset: placeholder_cs.clone(),
                        theirs_changeset: placeholder_cs.clone(),
                        description: format!(
                            "theirs modified {}::{} but ours deleted it",
                            base_entry.scope, base_entry.name
                        ),
                        ours_span: None,
                        theirs_span: Some(ConflictSpan::from_byte_range(
                            theirs,
                            theirs_entry.byte_range.clone(),
                        )),
                        base_span: Some(ConflictSpan::from_byte_range(
                            base,
                            base_entry.byte_range.clone(),
                        )),
                    });
                }
            }
        }

        if !conflicts.is_empty() {
            return Ok(MergeResult::Conflict(conflicts));
        }

        // No conflicts — reconstruct the merged file
        let merged = reconstruct_merged_file(
            base,
            ours,
            theirs,
            &base_symbols,
            &ours_symbols,
            &theirs_symbols,
        );

        // Safety net: re-parse the merged output and fall back to text merge
        // if the byte-range splicing produced broken syntax. Tree-sitter
        // grammars can handle trailing whitespace, commas, and docstrings
        // inconsistently, so the reconstructed file may not be syntactically
        // valid even though the merge was logically clean.
        if self.parser.has_syntax_errors(path, &merged) {
            tracing::warn!(
                ?path,
                "semantic merge produced invalid syntax, falling back to text merge"
            );
            return text_merge(base, ours, theirs, path);
        }

        Ok(MergeResult::Clean(merged))
    }
}

/// Ensure `buf` ends with exactly one newline before appending a new symbol.
fn ensure_newline(buf: &mut Vec<u8>) {
    if buf.last() != Some(&b'\n') {
        buf.push(b'\n');
    }
}

/// Reconstruct a merged file from base, ours, and theirs using symbol regions.
///
/// Strategy:
/// 1. Build a map of base symbol regions (byte ranges).
/// 2. Walk through base, replacing symbol regions with the appropriate version.
/// 3. Append symbols that were added by either side.
fn reconstruct_merged_file(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    base_symbols: &[SymbolEntry],
    ours_symbols: &[SymbolEntry],
    theirs_symbols: &[SymbolEntry],
) -> Vec<u8> {
    let base_map: HashMap<EntityKey, &SymbolEntry> =
        base_symbols.iter().map(|e| (entity_key(e), e)).collect();
    let ours_map: HashMap<EntityKey, &SymbolEntry> =
        ours_symbols.iter().map(|e| (entity_key(e), e)).collect();
    let theirs_map: HashMap<EntityKey, &SymbolEntry> =
        theirs_symbols.iter().map(|e| (entity_key(e), e)).collect();

    // Sort base symbols by byte position
    let mut sorted_base: Vec<&SymbolEntry> = base_symbols.iter().collect();
    sorted_base.sort_by_key(|s| s.byte_range.start);

    let mut result = Vec::new();
    let mut cursor = 0;

    for base_sym in &sorted_base {
        let key = entity_key(base_sym);
        let range = &base_sym.byte_range;

        // Copy interstitial bytes (between symbols) from base
        if range.start > cursor {
            result.extend_from_slice(&base[cursor..range.start]);
        }

        let in_ours = ours_map.get(&key);
        let in_theirs = theirs_map.get(&key);

        match (in_ours, in_theirs) {
            (Some(o), Some(t)) => {
                let ours_changed = o.content_hash != base_sym.content_hash;
                let theirs_changed = t.content_hash != base_sym.content_hash;
                if ours_changed && !theirs_changed {
                    result.extend_from_slice(&ours[o.byte_range.clone()]);
                } else if !ours_changed && theirs_changed {
                    result.extend_from_slice(&theirs[t.byte_range.clone()]);
                } else {
                    // Both changed to same thing, or neither changed — use ours
                    result.extend_from_slice(&ours[o.byte_range.clone()]);
                }
            }
            (Some(o), None) => {
                // Theirs deleted it, ours still has it (unchanged, since conflicts are already caught)
                // If ours didn't modify, skip (honor deletion). If ours modified → conflict (caught above).
                if o.content_hash == base_sym.content_hash {
                    // Ours unchanged, theirs deleted → honor deletion (skip)
                } else {
                    // Should not reach here — conflict was caught
                    result.extend_from_slice(&ours[o.byte_range.clone()]);
                }
            }
            (None, Some(t)) => {
                if t.content_hash == base_sym.content_hash {
                    // Theirs unchanged, ours deleted → honor deletion (skip)
                } else {
                    result.extend_from_slice(&theirs[t.byte_range.clone()]);
                }
            }
            (None, None) => {
                // Both deleted — skip
            }
        }

        cursor = range.end;
    }

    // Copy trailing bytes from base
    if cursor < base.len() {
        result.extend_from_slice(&base[cursor..]);
    }

    // Append symbols that were added by ours (not in base)
    for ours_sym in ours_symbols {
        let key = entity_key(ours_sym);
        if !base_map.contains_key(&key) {
            // Check if theirs also added the same symbol — if so, only add once
            if let Some(theirs_sym) = theirs_map.get(&key) {
                if theirs_sym.content_hash == ours_sym.content_hash {
                    // Identical — add from ours
                    ensure_newline(&mut result);
                    result.extend_from_slice(&ours[ours_sym.byte_range.clone()]);
                }
                // Different content is a conflict, already caught
            } else {
                ensure_newline(&mut result);
                result.extend_from_slice(&ours[ours_sym.byte_range.clone()]);
            }
        }
    }

    // Append symbols that were added only by theirs (not in base and not in ours)
    for theirs_sym in theirs_symbols {
        let key = entity_key(theirs_sym);
        if !base_map.contains_key(&key) && !ours_map.contains_key(&key) {
            ensure_newline(&mut result);
            result.extend_from_slice(&theirs[theirs_sym.byte_range.clone()]);
        }
    }

    result
}

/// LCS-based three-way text merge fallback using `diffy`.
///
/// Correctly handles insertions, deletions, and modifications at arbitrary
/// positions. Falls back to conflict when both sides change the same region.
fn text_merge(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    path: &Path,
) -> Result<MergeResult, CoreError> {
    // Reject binary or non-UTF-8 content to prevent silent data corruption.
    if is_binary_or_non_utf8(base)
        || is_binary_or_non_utf8(ours)
        || is_binary_or_non_utf8(theirs)
    {
        return Ok(MergeResult::Conflict(vec![ConflictDetail {
            kind: ConflictKind::BinaryFile,
            file: path.to_path_buf(),
            symbol_id: None,
            ours_changeset: ChangesetId("unknown".into()),
            theirs_changeset: ChangesetId("unknown".into()),
            description: "file is binary or not valid UTF-8; cannot text-merge".into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }]));
    }

    // Safe: all three buffers validated as UTF-8 above.
    let base_str = std::str::from_utf8(base).unwrap();
    let ours_str = std::str::from_utf8(ours).unwrap();
    let theirs_str = std::str::from_utf8(theirs).unwrap();

    match diffy::merge(base_str, ours_str, theirs_str) {
        Ok(merged) => Ok(MergeResult::Clean(merged.into_bytes())),
        Err(_conflict_text) => Ok(MergeResult::Conflict(vec![ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: path.to_path_buf(),
            symbol_id: None,
            ours_changeset: ChangesetId("unknown".into()),
            theirs_changeset: ChangesetId("unknown".into()),
            description: "line-level text conflict".into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::traits::SemanticAnalyzer;

    fn merger() -> SemanticMerger {
        SemanticMerger::new()
    }

    #[test]
    fn both_add_different_functions_merges_cleanly() {
        let base = b"fn existing() {}\n";
        let ours = b"fn existing() {}\nfn added_by_ours() {}\n";
        let theirs = b"fn existing() {}\nfn added_by_theirs() {}\n";

        let result = merger()
            .three_way_merge(base, ours, theirs, Path::new("test.rs"))
            .unwrap();

        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8_lossy(&merged);
                assert!(text.contains("existing"), "should keep existing function");
                assert!(
                    text.contains("added_by_ours"),
                    "should include ours' addition"
                );
                assert!(
                    text.contains("added_by_theirs"),
                    "should include theirs' addition"
                );
            }
            MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
        }
    }

    #[test]
    fn both_modify_same_function_conflicts() {
        let base = b"fn shared() { 1 }\n";
        let ours = b"fn shared() { 2 }\n";
        let theirs = b"fn shared() { 3 }\n";

        let result = merger()
            .three_way_merge(base, ours, theirs, Path::new("test.rs"))
            .unwrap();

        match result {
            MergeResult::Conflict(conflicts) => {
                assert!(!conflicts.is_empty());
                assert!(matches!(
                    conflicts[0].kind,
                    ConflictKind::BothModifiedSymbol
                ));
            }
            MergeResult::Clean(_) => panic!("expected conflict"),
        }
    }

    #[test]
    fn one_adds_other_modifies_different_function() {
        let base = b"fn original() { 1 }\n";
        let ours = b"fn original() { 2 }\n";
        let theirs = b"fn original() { 1 }\nfn new_fn() {}\n";

        let result = merger()
            .three_way_merge(base, ours, theirs, Path::new("test.rs"))
            .unwrap();

        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8_lossy(&merged);
                assert!(text.contains("original"), "should keep modified original");
                assert!(text.contains("new_fn"), "should include new function");
                assert!(text.contains("{ 2 }"), "should use ours' modification");
            }
            MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
        }
    }

    #[test]
    fn delete_and_modify_same_symbol_conflicts() {
        let base = b"fn shared() { 1 }\nfn other() {}\n";
        let ours = b"fn shared() { 2 }\nfn other() {}\n"; // modified
        let theirs = b"fn other() {}\n"; // deleted

        let result = merger()
            .three_way_merge(base, ours, theirs, Path::new("test.rs"))
            .unwrap();

        match result {
            MergeResult::Conflict(conflicts) => {
                assert!(!conflicts.is_empty());
                assert!(
                    conflicts
                        .iter()
                        .any(|c| matches!(c.kind, ConflictKind::ModifyDeleteSymbol))
                );
            }
            MergeResult::Clean(_) => panic!("expected conflict"),
        }
    }

    #[test]
    fn both_add_identical_function_deduplicates() {
        let base = b"fn existing() {}\n";
        let ours = b"fn existing() {}\nfn same_new() { 42 }\n";
        let theirs = b"fn existing() {}\nfn same_new() { 42 }\n";

        let result = merger()
            .three_way_merge(base, ours, theirs, Path::new("test.rs"))
            .unwrap();

        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8_lossy(&merged);
                assert!(text.contains("same_new"));
                // Should not duplicate
                let count = text.matches("same_new").count();
                assert_eq!(count, 1, "identical function should appear only once");
            }
            MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
        }
    }

    #[test]
    fn both_add_same_import_deduplicates() {
        let base = b"fn existing() {}\n";
        let ours = b"use std::io;\nfn existing() {}\n";
        let theirs = b"use std::io;\nfn existing() {}\n";

        let result = merger()
            .three_way_merge(base, ours, theirs, Path::new("test.rs"))
            .unwrap();

        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8_lossy(&merged);
                assert!(text.contains("std::io"));
            }
            MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
        }
    }

    #[test]
    fn disjoint_changes_merge_cleanly() {
        let base = b"fn a() { 1 }\nfn b() { 2 }\n";
        let ours = b"fn a() { 10 }\nfn b() { 2 }\n"; // modified a
        let theirs = b"fn a() { 1 }\nfn b() { 20 }\n"; // modified b

        let result = merger()
            .three_way_merge(base, ours, theirs, Path::new("test.rs"))
            .unwrap();

        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8_lossy(&merged);
                assert!(text.contains("{ 10 }"), "should have ours' change to a");
                assert!(text.contains("{ 20 }"), "should have theirs' change to b");
            }
            MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
        }
    }

    #[test]
    fn unsupported_file_falls_back_to_text_merge() {
        // Ours modifies line2, theirs adds line4 — disjoint edits, clean merge.
        let base = b"line1\nline2\nline3\n";
        let ours = b"line1\nline2_modified\nline3\n";
        let theirs = b"line1\nline2\nline3\nline4\n";

        let result = merger()
            .three_way_merge(base, ours, theirs, Path::new("config.toml"))
            .unwrap();

        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8_lossy(&merged);
                assert!(text.contains("line2_modified"), "should have ours' change");
                assert!(text.contains("line4"), "should have theirs' addition");
            }
            MergeResult::Conflict(_) => panic!("expected clean text merge"),
        }
    }

    #[test]
    fn identical_ours_and_theirs_returns_clean() {
        let base = b"fn old() {}\n";
        let same = b"fn new_version() {}\n";

        let result = merger()
            .three_way_merge(base, same, same, Path::new("test.rs"))
            .unwrap();

        assert!(matches!(result, MergeResult::Clean(_)));
    }

    #[test]
    fn binary_file_with_null_bytes_returns_conflict() {
        let base = b"line1\nline2\n";
        let ours = b"line1\x00binary\nline2\n";
        let theirs = b"line1\nline2\nline3\n";

        let result = text_merge(base, ours, theirs, Path::new("data.bin")).unwrap();

        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
            }
            MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
        }
    }

    #[test]
    fn non_utf8_bytes_returns_conflict() {
        let base = b"valid utf8\n";
        let ours = b"\xff\xfe invalid utf8\n";
        let theirs = b"also valid\n";

        let result = text_merge(base, ours, theirs, Path::new("encoded.txt")).unwrap();

        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
            }
            MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
        }
    }

    #[test]
    fn valid_utf8_text_merges_normally() {
        let base = b"line1\nline2\nline3\n";
        let ours = b"line1\nmodified\nline3\n";
        let theirs = b"line1\nline2\nline3\nline4\n";

        let result = text_merge(base, ours, theirs, Path::new("notes.txt")).unwrap();

        match result {
            MergeResult::Clean(merged) => {
                let text = std::str::from_utf8(&merged).unwrap();
                assert!(text.contains("modified"));
                assert!(text.contains("line4"));
            }
            MergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }

    #[test]
    fn appended_symbols_no_double_newline() {
        // When base already ends with \n, appending should not produce \n\n
        let base = b"fn existing() {}\n";
        let ours = b"fn existing() {}\nfn from_ours() {}\n";
        let theirs = b"fn existing() {}\n";

        let result = merger()
            .three_way_merge(base, ours, theirs, Path::new("test.rs"))
            .unwrap();

        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8_lossy(&merged);
                assert!(
                    !text.contains("\n\n\n"),
                    "should not have triple newlines, got: {text:?}"
                );
                assert!(text.contains("from_ours"));
            }
            MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
        }
    }

    #[test]
    fn syntax_validation_catches_broken_merge() {
        // Verify that has_syntax_errors works on known-bad Rust code
        let parser = crate::parser::Parser::new();
        let valid = b"fn valid() { 42 }\n";
        let broken = b"fn broken( { 42 }\n"; // missing closing paren

        assert!(
            !parser.has_syntax_errors(Path::new("test.rs"), valid),
            "valid code should not have errors"
        );
        assert!(
            parser.has_syntax_errors(Path::new("test.rs"), broken),
            "broken code should have errors"
        );
    }

    #[test]
    fn syntax_validation_ignores_unsupported_languages() {
        let parser = crate::parser::Parser::new();
        let content = b"this is { definitely not valid { code";

        assert!(
            !parser.has_syntax_errors(Path::new("config.toml"), content),
            "unsupported languages should return false (no grammar to check)"
        );
    }
}

//! Build the per-file body of an auto-generated commit message.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use phantom_core::changeset::SemanticOperation;

/// Render a per-file breakdown of semantic operations, sorted by file path.
///
/// Returns an empty string when there are no operations to report; otherwise a
/// leading newline followed by one section per file.
pub(super) fn build_body(ops: &[SemanticOperation]) -> String {
    let mut file_ops: BTreeMap<&Path, Vec<String>> = BTreeMap::new();

    for op in ops {
        match op {
            SemanticOperation::AddSymbol { file, symbol, .. } => {
                file_ops
                    .entry(file)
                    .or_default()
                    .push(format!("  + {} ({})", symbol.name, symbol.kind));
            }
            SemanticOperation::ModifySymbol {
                file, new_entry, ..
            } => {
                file_ops
                    .entry(file)
                    .or_default()
                    .push(format!("  ~ {} ({})", new_entry.name, new_entry.kind));
            }
            SemanticOperation::DeleteSymbol { file, id, .. } => {
                let name = id.0.split("::").nth(1).unwrap_or(&id.0);
                file_ops
                    .entry(file)
                    .or_default()
                    .push(format!("  - {name}"));
            }
            SemanticOperation::AddFile { path } => {
                file_ops
                    .entry(path)
                    .or_default()
                    .push("  (new file)".to_string());
            }
            SemanticOperation::DeleteFile { path } => {
                file_ops
                    .entry(path)
                    .or_default()
                    .push("  (deleted)".to_string());
            }
            SemanticOperation::RawDiff { path, .. } => {
                file_ops
                    .entry(path)
                    .or_default()
                    .push("  (raw diff)".to_string());
            }
        }
    }

    if file_ops.is_empty() {
        return String::new();
    }

    let mut body = String::new();
    let _ = writeln!(body);
    for (file, lines) in &file_ops {
        let _ = writeln!(body, "{}:", file.display());
        for line in lines {
            let _ = writeln!(body, "{line}");
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use phantom_core::changeset::SemanticOperation;
    use phantom_core::id::{ContentHash, SymbolId};
    use phantom_core::symbol::{SymbolEntry, SymbolKind};

    fn sym(name: &str) -> SymbolEntry {
        SymbolEntry {
            id: SymbolId(format!("crate::{name}::Function")),
            kind: SymbolKind::Function,
            name: name.to_string(),
            scope: "crate".to_string(),
            file: PathBuf::from("src/lib.rs"),
            byte_range: 0..10,
            content_hash: ContentHash([0; 32]),
            signature_hash: ContentHash([0; 32]),
        }
    }

    #[test]
    fn body_empty_when_no_ops() {
        assert_eq!(build_body(&[]), "");
    }

    #[test]
    fn body_groups_by_file_and_sorts() {
        let ops = vec![
            SemanticOperation::AddSymbol {
                file: PathBuf::from("src/z.rs"),
                symbol: sym("last"),
            },
            SemanticOperation::AddSymbol {
                file: PathBuf::from("src/a.rs"),
                symbol: sym("first"),
            },
        ];
        let body = build_body(&ops);
        let a_pos = body.find("src/a.rs:").expect("a.rs section");
        let z_pos = body.find("src/z.rs:").expect("z.rs section");
        assert!(a_pos < z_pos, "sections must be sorted alphabetically");
        assert!(body.contains("+ first"));
        assert!(body.contains("+ last"));
    }

    #[test]
    fn body_handles_all_variants() {
        let ops = vec![
            SemanticOperation::AddSymbol {
                file: PathBuf::from("f.rs"),
                symbol: sym("a"),
            },
            SemanticOperation::ModifySymbol {
                file: PathBuf::from("f.rs"),
                old_hash: ContentHash([0; 32]),
                old_signature_hash: ContentHash([0; 32]),
                new_entry: sym("b"),
            },
            SemanticOperation::DeleteSymbol {
                file: PathBuf::from("f.rs"),
                id: SymbolId("crate::c::Function".into()),
            },
            SemanticOperation::AddFile {
                path: PathBuf::from("new.rs"),
            },
            SemanticOperation::DeleteFile {
                path: PathBuf::from("old.rs"),
            },
            SemanticOperation::RawDiff {
                path: PathBuf::from("raw.rs"),
                patch: String::new(),
            },
        ];
        let body = build_body(&ops);
        assert!(body.contains("+ a"));
        assert!(body.contains("~ b"));
        assert!(body.contains("- c"));
        assert!(body.contains("(new file)"));
        assert!(body.contains("(deleted)"));
        assert!(body.contains("(raw diff)"));
    }
}

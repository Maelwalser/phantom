//! Shared helpers for semantic operation analysis.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use phantom_core::changeset::SemanticOperation;

/// Group semantic operations by file path, collecting the symbol names
/// modified in each file. Used by the materializer and submit service
/// for symbol-level overlap checks.
pub(crate) fn group_ops_by_file(
    operations: &[SemanticOperation],
) -> HashMap<PathBuf, HashSet<String>> {
    let mut map: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for op in operations {
        if let Some(name) = op.symbol_name() {
            map.entry(op.file_path().to_path_buf())
                .or_default()
                .insert(name.to_string());
        }
    }
    map
}

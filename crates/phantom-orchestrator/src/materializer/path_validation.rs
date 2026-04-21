//! Validate changeset file paths before they are written to the working tree.
//!
//! Rejects absolute paths, paths that escape the trunk directory via
//! parent-directory (`..`) components, and paths reserved for the user's
//! VCS or Phantom's own state (`.git/`, `.phantom/`, `.whiteouts.json`).
//!
//! This is the last gate before the tree builder turns a semantic op list
//! into a commit.  Earlier gates (overlay `modified_files`, semantic-op
//! extraction) also filter reserved paths; re-checking here is defense in
//! depth.  A malformed op list that reached this point MUST stop the
//! materialization dead — silently skipping reserved entries would leave
//! the op list in a half-applied state where the audit trail no longer
//! describes what landed on trunk.

use std::path::Path;

use phantom_core::reserved::is_reserved_path;

use crate::error::OrchestratorError;

/// Ensure `file` is a relative path that stays within `trunk_path` and does
/// not target a reserved Phantom/VCS path.
pub(super) fn validate_path(file: &Path, trunk_path: &Path) -> Result<(), OrchestratorError> {
    if file.is_absolute() {
        return Err(OrchestratorError::MaterializationFailed(format!(
            "path must be relative, got absolute: {}",
            file.display()
        )));
    }

    for component in file.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(OrchestratorError::MaterializationFailed(format!(
                "path contains parent traversal (..): {}",
                file.display()
            )));
        }
    }

    let joined = trunk_path.join(file);
    if !joined.starts_with(trunk_path) {
        return Err(OrchestratorError::MaterializationFailed(format!(
            "path escapes working tree: {}",
            file.display()
        )));
    }

    if let Some(kind) = is_reserved_path(file) {
        return Err(OrchestratorError::MaterializationFailed(format!(
            "refusing to materialize reserved path ({kind:?}): {}",
            file.display()
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn rejects_absolute_path() {
        let err = validate_path(Path::new("/etc/passwd"), Path::new("/repo")).unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    #[test]
    fn rejects_parent_traversal() {
        let err = validate_path(Path::new("../outside.txt"), Path::new("/repo")).unwrap_err();
        assert!(err.to_string().contains("parent traversal"), "{err}");
    }

    #[test]
    fn rejects_deep_parent_traversal() {
        let err = validate_path(Path::new("src/../../etc/passwd"), Path::new("/repo")).unwrap_err();
        assert!(err.to_string().contains("parent traversal"), "{err}");
    }

    #[test]
    fn accepts_relative_path() {
        validate_path(&PathBuf::from("src/main.rs"), Path::new("/repo")).unwrap();
    }

    #[test]
    fn rejects_dotgit_path() {
        for p in [
            ".git/HEAD",
            ".git/config",
            ".git/refs/heads/main",
            "foo/.git/HEAD",
        ] {
            let err = validate_path(Path::new(p), Path::new("/repo")).unwrap_err();
            assert!(
                err.to_string().contains("reserved"),
                "expected reserved-path error for {p}, got: {err}"
            );
        }
    }

    #[test]
    fn rejects_dotphantom_path() {
        let err =
            validate_path(Path::new(".phantom/events.db"), Path::new("/repo")).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err}");
    }

    #[test]
    fn rejects_whiteouts_json_at_any_depth() {
        for p in [".whiteouts.json", "a/b/.whiteouts.json"] {
            let err = validate_path(Path::new(p), Path::new("/repo")).unwrap_err();
            assert!(
                err.to_string().contains("reserved"),
                "expected reserved-path error for {p}, got: {err}"
            );
        }
    }
}

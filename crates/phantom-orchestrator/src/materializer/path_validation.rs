//! Validate changeset file paths before they are written to the working tree.
//!
//! Rejects absolute paths and paths that escape the trunk directory via
//! parent-directory (`..`) components.

use std::path::Path;

use crate::error::OrchestratorError;

/// Ensure `file` is a relative path that stays within `trunk_path`.
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
        let err =
            validate_path(Path::new("../outside.txt"), Path::new("/repo")).unwrap_err();
        assert!(err.to_string().contains("parent traversal"), "{err}");
    }

    #[test]
    fn rejects_deep_parent_traversal() {
        let err = validate_path(
            Path::new("src/../../etc/passwd"),
            Path::new("/repo"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("parent traversal"), "{err}");
    }

    #[test]
    fn accepts_relative_path() {
        validate_path(&PathBuf::from("src/main.rs"), Path::new("/repo")).unwrap();
    }
}

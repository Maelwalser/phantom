//! `.phantom-task.md` generation and cleanup for agent overlays.
//!
//! The context file provides agents with metadata about their session:
//! agent ID, changeset ID, base commit, and available commands.
//!
//! Submodules:
//! - [`task`]: basic context file writing
//! - [`resolve`]: conflict resolution context and rules
//! - [`plan`]: plan domain instruction files

use std::path::Path;

use tracing::warn;

mod plan;
mod resolve;
mod task;

pub use plan::write_plan_domain_instructions;
pub use resolve::{ResolveConflictContext, write_resolve_context_file, write_resolve_rules_file};
pub use task::{append_context_update, write_context_file};

/// Name of the generated context file placed in the overlay.
pub const CONTEXT_FILE: &str = ".phantom-task.md";

/// Name of the static resolution rules file injected via system prompt.
pub const RESOLVE_RULES_FILE: &str = "resolve-rules.md";

/// Detect language from file extension for code fence annotations.
pub(crate) fn lang_from_path(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("ts" | "tsx") => "typescript",
        Some("js" | "jsx") => "javascript",
        Some("py") => "python",
        Some("go") => "go",
        Some("toml") => "toml",
        Some("json") => "json",
        Some("yaml" | "yml") => "yaml",
        Some("md") => "markdown",
        Some("css") => "css",
        Some("html") => "html",
        _ => "",
    }
}

/// Remove the generated context file from the overlay.
pub fn cleanup_context_file(upper_dir: &Path) {
    let path = upper_dir.join(CONTEXT_FILE);
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!(path = %path.display(), error = %e, "failed to clean up context file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_from_path_maps_correctly() {
        assert_eq!(lang_from_path(Path::new("foo.rs")), "rust");
        assert_eq!(lang_from_path(Path::new("bar.ts")), "typescript");
        assert_eq!(lang_from_path(Path::new("baz.py")), "python");
        assert_eq!(lang_from_path(Path::new("qux.go")), "go");
        assert_eq!(lang_from_path(Path::new("unknown.txt")), "");
    }
}

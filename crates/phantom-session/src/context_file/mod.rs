//! `.phantom-task.md` generation and cleanup for agent overlays.
//!
//! The context file provides agents with metadata about their session:
//! agent ID, changeset ID, base commit, and available commands.
//!
//! Submodules:
//! - `task`: basic context file writing
//! - `resolve`: conflict resolution context and rules
//! - `plan`: plan domain instruction files

use std::path::Path;

use tracing::warn;

mod category_rules;
mod plan;
mod resolve;
mod task;

pub use category_rules::{
    RULES_DIR, ensure_category_rules_dir, rules_body, rules_path, write_category_rules_file,
};
pub use plan::{write_plan_domain_instructions, write_plan_domain_instructions_with_toolchain};
pub use resolve::{ResolveConflictContext, write_resolve_context_file, write_resolve_rules_file};
pub use task::{append_context_update, write_context_file, write_context_file_with_toolchain};

/// Name of the generated context file placed in the overlay.
pub const CONTEXT_FILE: &str = ".phantom-task.md";

/// Name of the static resolution rules file injected via system prompt.
pub const RESOLVE_RULES_FILE: &str = "resolve-rules.md";

/// Escape user-controlled content before embedding it into markdown that
/// will be read by an LLM agent.
///
/// Markdown headings (`#` at start of line) and horizontal rules (`---`)
/// are structural tokens. If a task description contains
/// `## Commands\n- rm -rf /` it can visually override the real Commands
/// section shown to the downstream agent. We neutralize these by prefixing
/// a zero-width Unicode char (U+200B) to leading `#` and `-` runs so they
/// render as ordinary text. Backticks and HTML-like `<…>` tags are left
/// intact because they are commonly part of legitimate descriptions;
/// callers that want to fully neutralize a field should additionally wrap
/// it in a fenced block.
pub fn sanitize_markdown(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.split_inclusive('\n') {
        // Strip leading whitespace to inspect the first non-space byte.
        let trimmed = line.trim_start_matches([' ', '\t']);
        let needs_escape = trimmed.starts_with('#')
            || trimmed.starts_with("---")
            || trimmed.starts_with("***")
            || trimmed.starts_with("===");
        if needs_escape {
            // Preserve leading whitespace and insert zero-width space so
            // the markdown parser (and the LLM's structural sense) does
            // not interpret the line as a heading or rule.
            let indent_len = line.len() - trimmed.len();
            out.push_str(&line[..indent_len]);
            out.push('\u{200B}');
            out.push_str(trimmed);
        } else {
            out.push_str(line);
        }
    }
    out
}

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

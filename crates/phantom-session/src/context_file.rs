//! `.phantom-task.md` generation and cleanup for agent overlays.
//!
//! The context file provides agents with metadata about their session:
//! agent ID, changeset ID, base commit, and available commands.

use std::path::Path;

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_core::symbol::find_enclosing_symbol;
use tracing::warn;

/// Approximate byte budget for whole-file display in conflict context.
/// ~8192 tokens × 4 bytes/token = 32,768 bytes.
const WHOLE_FILE_BYTE_BUDGET: usize = 32_768;
const BYTES_PER_TOKEN_ESTIMATE: usize = 4;

/// Name of the generated context file placed in the overlay.
pub const CONTEXT_FILE: &str = ".phantom-task.md";

/// Name of the static resolution rules file injected via system prompt.
pub const RESOLVE_RULES_FILE: &str = "resolve-rules.md";

/// Write a context file into the overlay with agent metadata and optional task.
pub fn write_context_file(
    upper_dir: &Path,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    task: Option<&str>,
) -> anyhow::Result<()> {
    let base_hex = base_commit.to_hex();
    let base_short = &base_hex[..12.min(base_hex.len())];

    let task_section = match task {
        Some(t) if !t.is_empty() => format!("\n## Task\n{t}\n"),
        _ => String::new(),
    };

    let content = format!(
        r#"# Phantom Agent Session

You are working inside a Phantom overlay. Your changes are isolated from
trunk and other agents.

## Commands
- `phantom submit {agent_id}` -- submit your changes
- `phantom materialize {changeset_id}` -- merge to trunk
- `phantom status` -- view all agents and changesets
{task_section}
## Agent Info
- Agent: {agent_id}
- Changeset: {changeset_id}
- Base commit: {base_short}
"#
    );

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write context file to {}", path.display()))?;

    Ok(())
}

/// Context for a single conflict, with the three-way file content.
pub struct ResolveConflictContext {
    /// The conflict detail from the event log.
    pub detail: phantom_core::ConflictDetail,
    /// Content of the file at the changeset's base commit (common ancestor).
    pub base_content: Option<String>,
    /// Content of the file at the current trunk HEAD.
    pub ours_content: Option<String>,
    /// Content of the file in the agent's overlay (upper layer).
    pub theirs_content: Option<String>,
}

/// Write the static resolution rules to a file for system prompt injection.
///
/// This file is passed via `--append-system-prompt-file` so its content becomes
/// part of the cached system prompt prefix. Because the content is 100% static,
/// it maximises prompt cache hit rates across all conflict resolution sessions.
pub fn write_resolve_rules_file(path: &Path) -> anyhow::Result<()> {
    let content = "\
# Phantom Conflict Resolution Rules

## Resolution Rules

1. For symbol-level conflicts you are shown the BASE version as full source,
   then OURS and THEIRS as unified diffs showing what each side changed
   relative to BASE. Your working directory has the THEIRS version —
   integrate the OURS changes. For other conflict types you see three full
   code blocks (BASE, OURS, THEIRS).
2. Your goal: produce a merged version that preserves the intent of BOTH sides.
3. NEVER silently drop code from either side unless one side explicitly deleted it.
4. For BothModifiedSymbol conflicts: merge both sets of changes into the symbol.
   If they modify different parts, combine them. If they make contradictory
   changes to the same lines, prefer the more complete version and leave a
   comment explaining the choice.
5. For ModifyDeleteSymbol conflicts: keep the modification unless the deletion
   was clearly intentional (e.g., functionality moved elsewhere).
6. For dependency version conflicts: pick the higher version unless there is a
   compatibility constraint.
7. Edit ONLY the files listed in the conflict context file. Do not modify unrelated files.
8. After editing, verify the file still parses correctly.
9. If you cannot resolve a conflict with confidence, leave a comment:
   `// PHANTOM_UNRESOLVED: <reason>`

## After Resolution
Your changes will be automatically submitted and materialized when you finish.
";

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    std::fs::write(path, content)
        .with_context(|| format!("failed to write resolve rules to {}", path.display()))?;

    Ok(())
}

/// Write a conflict-resolution context file into the overlay.
///
/// Generates a `.phantom-task.md` with three-version diffs for a background
/// Claude Code agent. Static resolution rules are injected separately via
/// `--append-system-prompt-file` (see [`write_resolve_rules_file`]).
pub fn write_resolve_context_file(
    upper_dir: &Path,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    conflicts: &[ResolveConflictContext],
) -> anyhow::Result<()> {
    use std::fmt::Write;

    let base_hex = base_commit.to_hex();
    let base_short = &base_hex[..12.min(base_hex.len())];

    let mut content = String::new();
    writeln!(content, "# Phantom Conflict Resolution").unwrap();
    writeln!(content).unwrap();
    writeln!(
        content,
        "You are resolving merge conflicts in a Phantom overlay. Your changes are"
    )
    .unwrap();
    writeln!(content, "isolated from trunk and other agents.").unwrap();
    writeln!(content).unwrap();
    writeln!(content, "## Agent Info").unwrap();
    writeln!(content, "- Agent: {agent_id}").unwrap();
    writeln!(content, "- Changeset: {changeset_id}").unwrap();
    writeln!(content, "- Base commit: {base_short}").unwrap();
    writeln!(content).unwrap();
    writeln!(content, "## Conflicts").unwrap();

    for (i, conflict) in conflicts.iter().enumerate() {
        writeln!(content).unwrap();
        let kind_label = format_conflict_kind(conflict.detail.kind);
        writeln!(
            content,
            "### Conflict {}: {} [{}]",
            i + 1,
            conflict.detail.file.display(),
            kind_label
        )
        .unwrap();
        writeln!(content, "{}", conflict.detail.description).unwrap();
        writeln!(content).unwrap();

        let lang = lang_from_path(&conflict.detail.file);
        let file_path = &conflict.detail.file;

        // Try compact diff format for symbol-level conflicts.
        let is_symbol_conflict = matches!(
            conflict.detail.kind,
            phantom_core::ConflictKind::BothModifiedSymbol
                | phantom_core::ConflictKind::ModifyDeleteSymbol
        );

        let used_compact = is_symbol_conflict
            && write_compact_conflict(&mut content, lang, conflict, base_short);

        if !used_compact {
            // Fallback: three full code blocks.
            writeln!(content, "#### BASE (common ancestor at {base_short})").unwrap();
            write_code_block(
                &mut content,
                lang,
                conflict.base_content.as_deref(),
                conflict.detail.base_span.as_ref(),
                file_path,
            );

            writeln!(content, "#### OURS (current trunk)").unwrap();
            write_code_block(
                &mut content,
                lang,
                conflict.ours_content.as_deref(),
                conflict.detail.ours_span.as_ref(),
                file_path,
            );

            writeln!(
                content,
                "#### THEIRS (agent's version \u{2014} in your working directory)"
            )
            .unwrap();
            write_code_block(
                &mut content,
                lang,
                conflict.theirs_content.as_deref(),
                conflict.detail.theirs_span.as_ref(),
                file_path,
            );
        }

        writeln!(
            content,
            "Edit `{}` in your working directory to merge both changes.",
            conflict.detail.file.display()
        )
        .unwrap();
        writeln!(content).unwrap();
        writeln!(content, "---").unwrap();
    }

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write resolve context file to {}", path.display()))?;

    Ok(())
}

/// Extract the enclosing symbol's source text and start line from file content.
///
/// Returns `(symbol_text, start_line)` on success, or `None` if the language
/// is unsupported, parsing fails, or no symbol encloses the span.
fn extract_symbol_text(
    content: &str,
    span: &phantom_core::conflict::ConflictSpan,
    file_path: &Path,
) -> Option<(String, usize)> {
    let parser = phantom_semantic::Parser::new();
    if !parser.supports_language(file_path) {
        return None;
    }
    let symbols = parser.parse_file(file_path, content.as_bytes()).ok()?;
    let enclosing = find_enclosing_symbol(&symbols, &span.byte_range)?;
    let start = enclosing.byte_range.start;
    let end = enclosing.byte_range.end.min(content.len());
    let text = content[start..end].to_string();
    let start_line = content[..start].matches('\n').count() + 1;
    Some((text, start_line))
}

/// Attempt to write a conflict in compact diff format.
///
/// Shows BASE symbol text once, then OURS and THEIRS as unified diffs
/// against BASE. Returns `true` if the compact format was written,
/// `false` if the caller should fall back to the three-block format.
fn write_compact_conflict(
    out: &mut String,
    lang: &str,
    conflict: &ResolveConflictContext,
    base_short: &str,
) -> bool {
    use std::fmt::Write;

    let file_path = &conflict.detail.file;

    // Require all three contents and a base span.
    let (base_content, ours_content, theirs_content) = match (
        conflict.base_content.as_deref(),
        conflict.ours_content.as_deref(),
        conflict.theirs_content.as_deref(),
    ) {
        (Some(b), Some(o), Some(t)) => (b, o, t),
        _ => return false,
    };

    let base_span = match conflict.detail.base_span.as_ref() {
        Some(s) => s,
        None => return false,
    };

    // Extract symbol text from BASE.
    let (base_symbol, base_start_line) =
        match extract_symbol_text(base_content, base_span, file_path) {
            Some(pair) => pair,
            None => return false,
        };

    // Extract symbol text from OURS and THEIRS (None = side deleted the symbol).
    let ours_symbol = conflict
        .detail
        .ours_span
        .as_ref()
        .and_then(|s| extract_symbol_text(ours_content, s, file_path))
        .map(|(text, _)| text);

    let theirs_symbol = conflict
        .detail
        .theirs_span
        .as_ref()
        .and_then(|s| extract_symbol_text(theirs_content, s, file_path))
        .map(|(text, _)| text);

    // Write BASE once.
    let end_line = base_start_line + base_symbol.lines().count().saturating_sub(1);
    writeln!(out, "#### BASE (common ancestor at {base_short})").unwrap();
    writeln!(
        out,
        "Lines {base_start_line}-{end_line} in `{}`",
        file_path.display()
    )
    .unwrap();
    writeln!(out, "```{lang}").unwrap();
    writeln!(out, "{base_symbol}").unwrap();
    writeln!(out, "```").unwrap();
    writeln!(out).unwrap();

    // Write OURS diff.
    write_diff_section(out, "OURS", "trunk applied these changes", &base_symbol, ours_symbol.as_deref());

    // Write THEIRS diff.
    write_diff_section(
        out,
        "THEIRS",
        "agent applied these changes \u{2014} in your working directory",
        &base_symbol,
        theirs_symbol.as_deref(),
    );

    true
}

/// Write a single diff section (OURS or THEIRS) relative to BASE.
fn write_diff_section(
    out: &mut String,
    label: &str,
    desc: &str,
    base_symbol: &str,
    modified: Option<&str>,
) {
    use std::fmt::Write;

    writeln!(out, "#### {label} ({desc})").unwrap();
    match modified {
        Some(text) if text == base_symbol => {
            writeln!(out, "*(identical to BASE)*").unwrap();
        }
        Some(text) => {
            let patch = diffy::create_patch(base_symbol, text);
            writeln!(out, "```diff").unwrap();
            write!(out, "{patch}").unwrap();
            writeln!(out, "```").unwrap();
        }
        None => {
            writeln!(out, "*(symbol deleted)*").unwrap();
        }
    }
    writeln!(out).unwrap();
}

/// Extract the enclosing AST node for a conflict span, falling back to ±10
/// line padding for unsupported languages or when no symbol encloses the span.
fn extract_span_context(
    content: &str,
    span: &phantom_core::conflict::ConflictSpan,
    file_path: &Path,
) -> String {
    let parser = phantom_semantic::Parser::new();
    if parser.supports_language(file_path) {
        if let Ok(symbols) = parser.parse_file(file_path, content.as_bytes()) {
            if let Some(enclosing) = find_enclosing_symbol(&symbols, &span.byte_range) {
                let start = enclosing.byte_range.start;
                let end = enclosing.byte_range.end.min(content.len());
                return content[start..end].to_string();
            }
        }
    }
    extract_span_lines_fallback(content, span)
}

/// Fallback: extract lines around a conflict span with ±10 line padding.
fn extract_span_lines_fallback(
    content: &str,
    span: &phantom_core::conflict::ConflictSpan,
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = span.start_line.saturating_sub(10).max(1) - 1; // zero-indexed
    let end = (span.end_line + 10).min(lines.len());
    lines[start..end].join("\n")
}

/// Truncate content to fit within the token budget, breaking at the last
/// complete line before the byte limit.
fn truncate_to_token_budget(text: &str) -> String {
    if text.len() <= WHOLE_FILE_BYTE_BUDGET {
        return text.to_string();
    }
    let cut = text[..WHOLE_FILE_BYTE_BUDGET]
        .rfind('\n')
        .unwrap_or(WHOLE_FILE_BYTE_BUDGET);
    let remaining_tokens = (text.len() - cut) / BYTES_PER_TOKEN_ESTIMATE;
    format!(
        "{}\n// ... truncated (~{remaining_tokens} more tokens)",
        &text[..cut]
    )
}

/// Write a fenced code block, trimming to span if available.
fn write_code_block(
    out: &mut String,
    lang: &str,
    content: Option<&str>,
    span: Option<&phantom_core::conflict::ConflictSpan>,
    file_path: &Path,
) {
    use std::fmt::Write;

    match content {
        Some(text) => {
            let display = match span {
                Some(s) => extract_span_context(text, s, file_path),
                None => truncate_to_token_budget(text),
            };
            writeln!(out, "```{lang}").unwrap();
            writeln!(out, "{display}").unwrap();
            writeln!(out, "```").unwrap();
        }
        None => {
            writeln!(out, "*(file not found at this version)*").unwrap();
        }
    }
    writeln!(out).unwrap();
}

/// Map a conflict kind to a human-readable label.
fn format_conflict_kind(kind: phantom_core::ConflictKind) -> &'static str {
    match kind {
        phantom_core::ConflictKind::BothModifiedSymbol => "BothModifiedSymbol",
        phantom_core::ConflictKind::ModifyDeleteSymbol => "ModifyDeleteSymbol",
        phantom_core::ConflictKind::BothModifiedDependencyVersion => {
            "BothModifiedDependencyVersion"
        }
        phantom_core::ConflictKind::RawTextConflict => "RawTextConflict",
        phantom_core::ConflictKind::BinaryFile => "BinaryFile",
    }
}

/// Detect language from file extension for code fence annotations.
fn lang_from_path(path: &std::path::Path) -> &'static str {
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

/// Write a domain-specific instruction file for a `phantom plan` agent.
///
/// The generated markdown provides the agent with its task scope, requirements
/// checklist, verification commands, and awareness of other parallel domains.
/// This file is passed to Claude Code via `--append-system-prompt-file`.
pub fn write_plan_domain_instructions(
    instructions_path: &Path,
    domain: &phantom_core::plan::PlanDomain,
    plan: &phantom_core::plan::Plan,
    cross_domain_signatures: Option<&str>,
) -> anyhow::Result<()> {
    use std::fmt::Write as _;

    let mut content = String::new();

    // Writing to a String is infallible — no need for unwrap/error handling.
    let _ = writeln!(content, "# Phantom Plan Domain: {}", domain.name);
    let _ = writeln!(content);
    let _ = writeln!(
        content,
        "You are an autonomous agent working on one domain of a larger plan."
    );
    let _ = writeln!(
        content,
        "Your changes will be automatically submitted and materialized when you finish."
    );
    let _ = writeln!(content);

    // Task
    let _ = writeln!(content, "## Your Task");
    let _ = writeln!(content, "{}", domain.description);
    let _ = writeln!(content);

    // Requirements
    if !domain.requirements.is_empty() {
        let _ = writeln!(content, "## Requirements");
        for req in &domain.requirements {
            let _ = writeln!(content, "- [ ] {req}");
        }
        let _ = writeln!(content);
    }

    // Scope
    let _ = writeln!(content, "## Scope");
    let _ = writeln!(content);
    if !domain.files_to_modify.is_empty() {
        let _ = writeln!(content, "### Files you SHOULD modify");
        for file in &domain.files_to_modify {
            let _ = writeln!(content, "- `{}`", file.display());
        }
        let _ = writeln!(content);
    }
    if !domain.files_not_to_modify.is_empty() {
        let _ = writeln!(content, "### Files you MUST NOT modify");
        for pattern in &domain.files_not_to_modify {
            let _ = writeln!(content, "- `{pattern}` (owned by another domain)");
        }
        let _ = writeln!(content);
    }

    // Parallel work awareness
    let other_domains: Vec<_> = plan
        .domains
        .iter()
        .filter(|d| d.name != domain.name)
        .collect();
    if !other_domains.is_empty() {
        let _ = writeln!(content, "## Parallel Work Awareness");
        let _ = writeln!(
            content,
            "Other agents are working on these domains simultaneously:"
        );
        for other in &other_domains {
            let _ = writeln!(content, "- **{}**: {}", other.name, other.description);
        }
        let _ = writeln!(content);
        let _ = writeln!(
            content,
            "Do NOT modify files owned by other domains. Phantom's semantic merge \
             will compose all domains' work automatically."
        );
        let _ = writeln!(content);
    }

    // Cross-domain signatures (pre-extracted for token efficiency).
    if let Some(sigs) = cross_domain_signatures
        && !sigs.is_empty()
    {
        let _ = write!(content, "{sigs}");
    }

    // Dependencies
    if !domain.depends_on.is_empty() {
        let _ = writeln!(content, "## Dependencies");
        let _ = writeln!(
            content,
            "This domain depends on work from these other domains:"
        );
        for dep in &domain.depends_on {
            let _ = writeln!(content, "- **{dep}**");
        }
        let _ = writeln!(
            content,
            "Their changes may or may not be on trunk yet. Code defensively."
        );
        let _ = writeln!(content);
    }

    // Verification
    if !domain.verification.is_empty() {
        let _ = writeln!(content, "## Verification");
        let _ = writeln!(content, "Run these commands before finishing:");
        for cmd in &domain.verification {
            let _ = writeln!(content, "```");
            let _ = writeln!(content, "{cmd}");
            let _ = writeln!(content, "```");
        }
        let _ = writeln!(content);
    }

    // Completion
    let _ = writeln!(content, "## Completion");
    let _ = writeln!(
        content,
        "When all requirements are met and verification passes, your work will \
         be automatically submitted and materialized. Do not run `phantom submit` \
         or `phantom materialize` manually."
    );

    // Ensure parent directory exists.
    if let Some(parent) = instructions_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create instructions directory {}",
                parent.display()
            )
        })?;
    }

    std::fs::write(instructions_path, content).with_context(|| {
        format!(
            "failed to write instructions to {}",
            instructions_path.display()
        )
    })?;

    Ok(())
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
    fn truncate_to_token_budget_under_budget_is_identity() {
        let text = "fn main() {}\n";
        assert_eq!(truncate_to_token_budget(text), text);
    }

    #[test]
    fn truncate_to_token_budget_over_budget_cuts_at_line() {
        // Build a string larger than WHOLE_FILE_BYTE_BUDGET.
        let line = "x".repeat(100) + "\n";
        let count = (WHOLE_FILE_BYTE_BUDGET / line.len()) + 10;
        let text: String = line.repeat(count);
        assert!(text.len() > WHOLE_FILE_BYTE_BUDGET);

        let result = truncate_to_token_budget(&text);
        assert!(result.len() < text.len());
        assert!(result.contains("// ... truncated (~"));
        assert!(result.contains("more tokens)"));
        // The cut should be at a newline boundary — no partial lines.
        let before_comment = result.split("// ... truncated").next().unwrap();
        assert!(before_comment.ends_with('\n'));
    }

    #[test]
    fn extract_span_context_uses_semantic_for_rust() {
        let src = "struct Foo {}\n\nfn target() {\n    let x = 1;\n    let y = 2;\n}\n\nfn other() {}\n";
        let span = phantom_core::conflict::ConflictSpan {
            byte_range: 28..39, // inside "fn target()"
            start_line: 4,
            end_line: 4,
        };
        let result = extract_span_context(src, &span, Path::new("test.rs"));
        // Should return the entire fn target() body, not just ±10 lines.
        assert!(result.contains("fn target()"));
        assert!(result.contains("let x = 1;"));
        assert!(result.contains("let y = 2;"));
        // Should NOT include unrelated symbols.
        assert!(!result.contains("struct Foo"));
        assert!(!result.contains("fn other"));
    }

    #[test]
    fn extract_span_context_falls_back_for_unsupported_lang() {
        let src = "line1\nline2\nline3\nline4\nline5\n";
        let span = phantom_core::conflict::ConflictSpan {
            byte_range: 6..11,
            start_line: 2,
            end_line: 2,
        };
        let result = extract_span_context(src, &span, Path::new("config.toml"));
        // Fallback path: should include surrounding lines.
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
    }

    fn make_both_modified_conflict(
        base_src: &str,
        ours_src: &str,
        theirs_src: &str,
    ) -> ResolveConflictContext {
        use phantom_core::conflict::{ConflictDetail, ConflictKind, ConflictSpan};
        use phantom_core::id::{ChangesetId, SymbolId};

        // Compute a span inside the first function body so that
        // `find_enclosing_symbol` can locate it. We point at a byte
        // range strictly within the function, not at the trailing newline.
        let span_of = |src: &str| {
            let start = src.find("fn ").unwrap_or(0);
            let end = src.rfind('}').map(|p| p + 1).unwrap_or(src.len());
            ConflictSpan::from_byte_range(src.as_bytes(), start..end)
        };

        ResolveConflictContext {
            detail: ConflictDetail {
                kind: ConflictKind::BothModifiedSymbol,
                file: std::path::PathBuf::from("src/handler.rs"),
                symbol_id: Some(SymbolId("crate::handler::target::function".into())),
                ours_changeset: ChangesetId("cs-1".into()),
                theirs_changeset: ChangesetId("cs-2".into()),
                description: "both sides modified handler::target".into(),
                base_span: Some(span_of(base_src)),
                ours_span: Some(span_of(ours_src)),
                theirs_span: Some(span_of(theirs_src)),
            },
            base_content: Some(base_src.to_string()),
            ours_content: Some(ours_src.to_string()),
            theirs_content: Some(theirs_src.to_string()),
        }
    }

    #[test]
    fn compact_format_for_both_modified_symbol() {
        let base = "fn target() {\n    let x = 1;\n    let y = 2;\n}\n";
        let ours = "fn target() {\n    let x = 10;\n    let y = 2;\n}\n";
        let theirs = "fn target() {\n    let x = 1;\n    let y = 20;\n}\n";

        let conflict = make_both_modified_conflict(base, ours, theirs);
        let mut out = String::new();
        let ok = write_compact_conflict(&mut out, "rust", &conflict, "abc123");

        assert!(ok, "should succeed for BothModifiedSymbol with all content");
        // BASE shown once as a code block.
        assert!(out.contains("#### BASE"));
        assert!(out.contains("```rust"));
        assert!(out.contains("fn target()"));
        // OURS and THEIRS shown as diffs.
        assert!(out.contains("#### OURS"));
        assert!(out.contains("#### THEIRS"));
        assert!(out.contains("```diff"));
        // Should NOT contain three full code blocks.
        let rust_block_count = out.matches("```rust").count();
        assert_eq!(rust_block_count, 1, "BASE should be the only rust block");
    }

    #[test]
    fn compact_format_falls_back_when_content_missing() {
        use phantom_core::conflict::{ConflictDetail, ConflictKind, ConflictSpan};
        use phantom_core::id::ChangesetId;

        let conflict = ResolveConflictContext {
            detail: ConflictDetail {
                kind: ConflictKind::BothModifiedSymbol,
                file: std::path::PathBuf::from("src/lib.rs"),
                symbol_id: None,
                ours_changeset: ChangesetId("cs-1".into()),
                theirs_changeset: ChangesetId("cs-2".into()),
                description: "conflict".into(),
                base_span: Some(ConflictSpan {
                    byte_range: 0..10,
                    start_line: 1,
                    end_line: 1,
                }),
                ours_span: None,
                theirs_span: None,
            },
            base_content: Some("fn foo() {}".into()),
            ours_content: None, // missing
            theirs_content: Some("fn foo() { 1 }".into()),
        };

        let mut out = String::new();
        let ok = write_compact_conflict(&mut out, "rust", &conflict, "abc");
        assert!(!ok, "should fall back when ours_content is missing");
    }

    #[test]
    fn compact_format_identical_side_shows_message() {
        let base = "fn target() {\n    let x = 1;\n}\n";
        let ours = base; // identical
        let theirs = "fn target() {\n    let x = 99;\n}\n";

        let conflict = make_both_modified_conflict(base, ours, theirs);
        let mut out = String::new();
        let ok = write_compact_conflict(&mut out, "rust", &conflict, "abc");

        assert!(ok);
        assert!(
            out.contains("*(identical to BASE)*"),
            "OURS should show identical message"
        );
        // THEIRS should still show a diff.
        assert!(out.contains("```diff"));
    }

    #[test]
    fn compact_format_deleted_side() {
        use phantom_core::conflict::{ConflictDetail, ConflictKind, ConflictSpan};
        use phantom_core::id::{ChangesetId, SymbolId};

        let base = "fn target() {\n    let x = 1;\n}\n";
        let ours = "fn target() {\n    let x = 10;\n}\n"; // modified
        // theirs deleted the symbol — file still exists but symbol is gone
        let theirs = "// empty\n";

        let conflict = ResolveConflictContext {
            detail: ConflictDetail {
                kind: ConflictKind::ModifyDeleteSymbol,
                file: std::path::PathBuf::from("src/handler.rs"),
                symbol_id: Some(SymbolId("crate::handler::target::function".into())),
                ours_changeset: ChangesetId("cs-1".into()),
                theirs_changeset: ChangesetId("cs-2".into()),
                description: "ours modified target but theirs deleted it".into(),
                base_span: Some(ConflictSpan::from_byte_range(
                    base.as_bytes(),
                    0..base.rfind('}').unwrap() + 1,
                )),
                ours_span: Some(ConflictSpan::from_byte_range(
                    ours.as_bytes(),
                    0..ours.rfind('}').unwrap() + 1,
                )),
                theirs_span: None, // deleted
            },
            base_content: Some(base.into()),
            ours_content: Some(ours.into()),
            theirs_content: Some(theirs.into()),
        };

        let mut out = String::new();
        let ok = write_compact_conflict(&mut out, "rust", &conflict, "abc");
        assert!(ok);
        assert!(
            out.contains("*(symbol deleted)*"),
            "THEIRS should show deleted message"
        );
    }

    #[test]
    fn full_resolve_file_uses_compact_for_symbol_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("test".to_string());
        let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        let base = "fn target() {\n    let x = 1;\n    let y = 2;\n}\n";
        let ours = "fn target() {\n    let x = 10;\n    let y = 2;\n}\n";
        let theirs = "fn target() {\n    let x = 1;\n    let y = 20;\n}\n";

        let conflicts = vec![make_both_modified_conflict(base, ours, theirs)];

        write_resolve_context_file(
            dir.path(),
            &agent_id,
            &changeset_id,
            &base_commit,
            &conflicts,
        )
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
        // Should use compact format: one rust block + diff blocks.
        assert!(content.contains("```diff"), "should contain diff blocks");
        let rust_blocks = content.matches("```rust").count();
        assert_eq!(rust_blocks, 1, "should have exactly one rust code block (BASE)");
    }

    #[test]
    fn full_resolve_file_falls_back_for_raw_text() {
        use phantom_core::conflict::{ConflictDetail, ConflictKind};
        use phantom_core::id::ChangesetId;

        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("test".to_string());
        let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        let conflicts = vec![ResolveConflictContext {
            detail: ConflictDetail {
                kind: ConflictKind::RawTextConflict,
                file: std::path::PathBuf::from("config.toml"),
                symbol_id: None,
                ours_changeset: ChangesetId("cs-1".into()),
                theirs_changeset: ChangesetId("cs-2".into()),
                description: "text conflict".into(),
                base_span: None,
                ours_span: None,
                theirs_span: None,
            },
            base_content: Some("key = 1\n".into()),
            ours_content: Some("key = 2\n".into()),
            theirs_content: Some("key = 3\n".into()),
        }];

        write_resolve_context_file(
            dir.path(),
            &agent_id,
            &changeset_id,
            &base_commit,
            &conflicts,
        )
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
        // Should NOT use compact format — falls back to three blocks.
        assert!(
            !content.contains("```diff"),
            "RawTextConflict should not use diff format"
        );
        // Should have the three-block fallback (BASE, OURS, THEIRS headings).
        assert!(content.contains("#### BASE"));
        assert!(content.contains("#### OURS"));
        assert!(content.contains("#### THEIRS"));
    }

    #[test]
    fn resolve_rules_file_contains_all_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.md");
        write_resolve_rules_file(&path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        // All 9 rules present.
        for i in 1..=9 {
            assert!(
                content.contains(&format!("{}.", i)),
                "missing rule {i}"
            );
        }
        assert!(content.contains("After Resolution"));
        assert!(content.contains("automatically submitted and materialized"));
    }

    #[test]
    fn resolve_context_file_excludes_rules() {
        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("test".to_string());
        let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        write_resolve_context_file(
            dir.path(),
            &agent_id,
            &changeset_id,
            &base_commit,
            &[],
        )
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
        assert!(content.contains("Phantom Conflict Resolution"));
        assert!(content.contains("Agent: test"));
        // Rules should NOT be in this file — they live in the system prompt.
        assert!(!content.contains("Resolution Rules"));
        assert!(!content.contains("After Resolution"));
    }

    #[test]
    fn context_file_has_dynamic_sections_last() {
        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("a1".to_string());
        let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        write_context_file(dir.path(), &agent_id, &changeset_id, &base_commit, Some("do stuff"))
            .unwrap();

        let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
        let commands_pos = content.find("## Commands").unwrap();
        let task_pos = content.find("## Task").unwrap();
        let info_pos = content.find("## Agent Info").unwrap();
        // Static commands section should precede dynamic task and agent info.
        assert!(commands_pos < task_pos, "Commands should come before Task");
        assert!(task_pos < info_pos, "Task should come before Agent Info");
    }
}

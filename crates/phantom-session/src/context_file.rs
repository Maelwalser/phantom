//! `.phantom-task.md` generation and cleanup for agent overlays.
//!
//! The context file provides agents with metadata about their session:
//! agent ID, changeset ID, base commit, and available commands.

use std::path::Path;

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use tracing::warn;

/// Name of the generated context file placed in the overlay.
pub const CONTEXT_FILE: &str = ".phantom-task.md";

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
{task_section}
## Agent Info
- Agent: {agent_id}
- Changeset: {changeset_id}
- Base commit: {base_short}

## Commands
- `phantom submit {agent_id}` -- submit your changes
- `phantom materialize {changeset_id}` -- merge to trunk
- `phantom status` -- view all agents and changesets
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

/// Write a conflict-resolution context file into the overlay.
///
/// Generates a `.phantom-task.md` with three-version diffs and resolution
/// instructions for a background Claude Code agent.
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
    writeln!(content, "## Resolution Rules").unwrap();
    writeln!(content).unwrap();
    writeln!(
        content,
        "1. You are shown three versions of each conflicting region: BASE (common"
    )
    .unwrap();
    writeln!(
        content,
        "   ancestor), OURS (current trunk), and THEIRS (the agent's version in"
    )
    .unwrap();
    writeln!(content, "   your working directory).").unwrap();
    writeln!(
        content,
        "2. Your goal: produce a merged version that preserves the intent of BOTH sides."
    )
    .unwrap();
    writeln!(
        content,
        "3. NEVER silently drop code from either side unless one side explicitly deleted it."
    )
    .unwrap();
    writeln!(
        content,
        "4. For BothModifiedSymbol conflicts: merge both sets of changes into the symbol."
    )
    .unwrap();
    writeln!(
        content,
        "   If they modify different parts, combine them. If they make contradictory"
    )
    .unwrap();
    writeln!(
        content,
        "   changes to the same lines, prefer the more complete version and leave a"
    )
    .unwrap();
    writeln!(content, "   comment explaining the choice.").unwrap();
    writeln!(
        content,
        "5. For ModifyDeleteSymbol conflicts: keep the modification unless the deletion"
    )
    .unwrap();
    writeln!(
        content,
        "   was clearly intentional (e.g., functionality moved elsewhere)."
    )
    .unwrap();
    writeln!(
        content,
        "6. For dependency version conflicts: pick the higher version unless there is a"
    )
    .unwrap();
    writeln!(content, "   compatibility constraint.").unwrap();
    writeln!(
        content,
        "7. Edit ONLY the files listed below. Do not modify unrelated files."
    )
    .unwrap();
    writeln!(
        content,
        "8. After editing, verify the file still parses correctly."
    )
    .unwrap();
    writeln!(
        content,
        "9. If you cannot resolve a conflict with confidence, leave a comment:"
    )
    .unwrap();
    writeln!(content, "   `// PHANTOM_UNRESOLVED: <reason>`").unwrap();
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

        // BASE
        writeln!(content, "#### BASE (common ancestor at {base_short})").unwrap();
        write_code_block(
            &mut content,
            lang,
            conflict.base_content.as_deref(),
            conflict.detail.base_span.as_ref(),
        );

        // OURS
        writeln!(content, "#### OURS (current trunk)").unwrap();
        write_code_block(
            &mut content,
            lang,
            conflict.ours_content.as_deref(),
            conflict.detail.ours_span.as_ref(),
        );

        // THEIRS
        writeln!(
            content,
            "#### THEIRS (agent's version — in your working directory)"
        )
        .unwrap();
        write_code_block(
            &mut content,
            lang,
            conflict.theirs_content.as_deref(),
            conflict.detail.theirs_span.as_ref(),
        );

        writeln!(
            content,
            "Edit `{}` in your working directory to merge both changes.",
            conflict.detail.file.display()
        )
        .unwrap();
        writeln!(content).unwrap();
        writeln!(content, "---").unwrap();
    }

    writeln!(content).unwrap();
    writeln!(content, "## After Resolution").unwrap();
    writeln!(
        content,
        "Your changes will be automatically submitted and materialized when you finish."
    )
    .unwrap();

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write resolve context file to {}", path.display()))?;

    Ok(())
}

/// Extract lines around a conflict span, with padding context.
fn extract_span_lines(content: &str, span: &phantom_core::conflict::ConflictSpan) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = span.start_line.saturating_sub(10).max(1) - 1; // zero-indexed
    let end = (span.end_line + 10).min(lines.len());
    lines[start..end].join("\n")
}

/// Write a fenced code block, trimming to span if available.
fn write_code_block(
    out: &mut String,
    lang: &str,
    content: Option<&str>,
    span: Option<&phantom_core::conflict::ConflictSpan>,
) {
    use std::fmt::Write;

    match content {
        Some(text) => {
            let display = match span {
                Some(s) => extract_span_lines(text, s),
                None => {
                    let lines: Vec<&str> = text.lines().collect();
                    if lines.len() > 200 {
                        let mut truncated: String = lines[..200].join("\n");
                        truncated.push_str(&format!(
                            "\n// ... truncated ({} more lines)",
                            lines.len() - 200
                        ));
                        truncated
                    } else {
                        text.to_string()
                    }
                }
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

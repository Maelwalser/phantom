//! Plan domain instruction file generation for `phantom plan` agents.

use std::path::Path;

use anyhow::Context;

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
            "This agent did not start until all dependencies were materialized to trunk. \
             Their changes are already visible in your overlay — you can rely on them being present."
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
         be automatically submitted and merged to trunk. Do not run `phantom submit` \
         manually."
    );

    // Cross-domain signatures — appended last so volatile content doesn't
    // invalidate the prefix cache for the static sections above.
    if let Some(sigs) = cross_domain_signatures
        && !sigs.is_empty()
    {
        let _ = writeln!(content);
        let _ = write!(content, "{sigs}");
    }

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

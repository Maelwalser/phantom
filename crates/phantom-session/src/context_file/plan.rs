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

    // Category rules go first so the byte-identical rule body stays at the
    // top of the prompt across every agent in the same category (cache-
    // friendly). A trailing divider separates rules from the domain body.
    if let Some(category) = domain.category {
        content.push_str(super::category_rules::rules_body(category));
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("\n---\n\n");
    }

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
         be automatically submitted and merged to trunk. Do not run `ph submit` \
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

#[cfg(test)]
mod tests {
    use phantom_core::TaskCategory;
    use phantom_core::id::PlanId;
    use phantom_core::plan::{Plan, PlanDomain, PlanStatus};

    use super::*;

    fn sample_plan(domain_category: Option<TaskCategory>) -> Plan {
        Plan {
            id: PlanId("plan-test".into()),
            request: "test".into(),
            created_at: chrono::Utc::now(),
            domains: vec![PlanDomain {
                name: "only".into(),
                agent_id: "plan-test-only".into(),
                description: "do the thing".into(),
                files_to_modify: vec![],
                files_not_to_modify: vec![],
                requirements: vec!["r1".into()],
                verification: vec!["cargo test".into()],
                depends_on: vec![],
                category: domain_category,
            }],
            status: PlanStatus::Draft,
        }
    }

    #[test]
    fn instructions_without_category_omit_rules_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.md");
        let plan = sample_plan(None);

        write_plan_domain_instructions(&path, &plan.domains[0], &plan, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();

        assert!(!body.contains("Phantom Task Rules:"));
        assert!(body.starts_with("# Phantom Plan Domain: only"));
    }

    #[test]
    fn instructions_with_category_prepend_matching_rules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.md");
        let plan = sample_plan(Some(TaskCategory::Corrective));

        write_plan_domain_instructions(&path, &plan.domains[0], &plan, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();

        assert!(body.starts_with("# Phantom Task Rules: Corrective"));
        assert!(body.contains("PHANTOM_UNREPRODUCIBLE:"));
        // Domain content still follows the rules.
        let rules_end = body.find("\n---\n\n").expect("rules divider missing");
        assert!(body[rules_end..].contains("# Phantom Plan Domain: only"));
    }

    #[test]
    fn category_rules_sit_at_byte_zero_for_cache_stability() {
        // Two domains that differ only in their downstream content must share
        // an identical prefix (the category rules body) so the prompt cache
        // hits on every bug-fix agent.
        let dir = tempfile::tempdir().unwrap();
        let mut plan_a = sample_plan(Some(TaskCategory::Adaptive));
        plan_a.domains[0].description = "domain A".into();
        let mut plan_b = sample_plan(Some(TaskCategory::Adaptive));
        plan_b.domains[0].description = "domain B".into();

        let path_a = dir.path().join("a.md");
        let path_b = dir.path().join("b.md");
        write_plan_domain_instructions(&path_a, &plan_a.domains[0], &plan_a, None).unwrap();
        write_plan_domain_instructions(&path_b, &plan_b.domains[0], &plan_b, None).unwrap();

        let a = std::fs::read(&path_a).unwrap();
        let b = std::fs::read(&path_b).unwrap();
        let prefix_len = super::super::category_rules::rules_body(TaskCategory::Adaptive).len();
        assert_eq!(
            &a[..prefix_len],
            &b[..prefix_len],
            "adaptive rules prefix must be byte-identical across domains"
        );
    }
}

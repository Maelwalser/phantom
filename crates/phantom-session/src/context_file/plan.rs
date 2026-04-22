//! Plan domain instruction file generation for `phantom plan` agents.

use std::path::Path;

use anyhow::Context;
use phantom_toolchain::{Toolchain, VerificationVerb};

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
    write_plan_domain_instructions_with_toolchain(
        instructions_path,
        domain,
        plan,
        cross_domain_signatures,
        None,
    )
}

/// Extended form of [`write_plan_domain_instructions`] that injects the repo's
/// auto-detected toolchain commands. When `domain.verification` is empty the
/// detected commands are used directly; when the planner already supplied
/// verification commands they are kept and the detected commands are shown as
/// a supplementary "Auto-detected" block.
pub fn write_plan_domain_instructions_with_toolchain(
    instructions_path: &Path,
    domain: &phantom_core::plan::PlanDomain,
    plan: &phantom_core::plan::Plan,
    cross_domain_signatures: Option<&str>,
    toolchain: Option<&Toolchain>,
) -> anyhow::Result<()> {
    use std::fmt::Write as _;

    let mut content = String::new();

    // Category rules go first so the byte-identical rule body stays at the
    // top of the prompt across every agent in the same category (cache-
    // friendly). A trailing divider separates rules from the domain body.
    // `rules_body` returns `None` for [`TaskCategory::Custom`], in which case
    // we fall through without a rules block.
    if let Some(category) = domain.category.as_ref()
        && let Some(body) = super::category_rules::rules_body(category)
    {
        content.push_str(body);
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

    // Dependencies — upstream domains the agent depends on, with concrete
    // agent IDs and the files each upstream owns. Gives this agent enough
    // information to recognize incoming trunk notifications and connect them
    // back to the domain-level dependency graph the planner produced.
    if !domain.depends_on.is_empty() {
        let _ = writeln!(content, "## Upstream Domains");
        let _ = writeln!(
            content,
            "This domain depends on work from these upstream domains. \
             This agent did not start until each upstream materialized to trunk, \
             so their changes are already visible in your overlay."
        );
        let _ = writeln!(content);

        for dep_name in &domain.depends_on {
            let upstream = plan.domains.iter().find(|d| &d.name == dep_name);
            match upstream {
                Some(up) => {
                    let _ = writeln!(content, "- **{dep_name}** (agent `{}`)", up.agent_id);
                    if !up.files_to_modify.is_empty() {
                        let _ = writeln!(content, "  - Owns files:");
                        for file in &up.files_to_modify {
                            let _ = writeln!(content, "    - `{}`", file.display());
                        }
                    }
                    if !up.description.is_empty() {
                        let _ = writeln!(content, "  - Scope: {}", up.description);
                    }
                }
                None => {
                    // Planner referenced a dependency we can't resolve — still
                    // list it so the agent knows there's an implicit prereq.
                    let _ = writeln!(content, "- **{dep_name}** (domain not found in plan)");
                }
            }
        }
        let _ = writeln!(content);
        let _ = writeln!(
            content,
            "After each upstream submits, Phantom injects a trunk-update block \
             directly into your next turn — you do not need to re-read any file. \
             If any of your working symbols depend on a changed or deleted upstream \
             symbol, the injected `Impacted Dependencies` section tells you exactly \
             which references to review. A mirror of each update is also written to \
             `.phantom-trunk-update.md` for reference between turns."
        );
        let _ = writeln!(content);
    }

    // Verification — prefer the planner's explicit commands when present,
    // otherwise fall back to the auto-detected toolchain. When both are
    // present, list the planner commands first and append the detected set
    // under an "Auto-detected" subheading so the agent can see alternatives
    // without losing the planner's intent.
    let has_planner_commands = !domain.verification.is_empty();
    let detected_lines: Vec<String> = toolchain
        .filter(|t| !t.is_empty())
        .map(|t| {
            VerificationVerb::ALL
                .iter()
                .filter_map(|verb| {
                    t.command_for(*verb)
                        .map(|cmd| format!("- {label}: `{cmd}`", label = verb.human_label()))
                })
                .collect()
        })
        .unwrap_or_default();

    if has_planner_commands || !detected_lines.is_empty() {
        let _ = writeln!(content, "## Verification");
        if has_planner_commands {
            let _ = writeln!(content, "Run these commands before finishing:");
            for cmd in &domain.verification {
                let _ = writeln!(content, "```");
                let _ = writeln!(content, "{cmd}");
                let _ = writeln!(content, "```");
            }
        }
        if !detected_lines.is_empty() {
            if has_planner_commands {
                let _ = writeln!(content, "\n### Auto-detected for this repo");
            }
            for line in &detected_lines {
                let _ = writeln!(content, "{line}");
            }
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
    fn toolchain_fills_verification_when_planner_empty() {
        use phantom_toolchain::{DetectedLanguage, Toolchain};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.md");
        let mut plan = sample_plan(None);
        plan.domains[0].verification.clear();

        let toolchain = Toolchain {
            language: Some(DetectedLanguage::Rust),
            test_cmd: Some("cargo test".into()),
            build_cmd: Some("cargo build".into()),
            lint_cmd: None,
            typecheck_cmd: None,
            format_check_cmd: None,
        };

        write_plan_domain_instructions_with_toolchain(
            &path,
            &plan.domains[0],
            &plan,
            None,
            Some(&toolchain),
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();

        assert!(body.contains("## Verification"));
        assert!(body.contains("`cargo test`"));
        assert!(!body.contains("Auto-detected"));
    }

    #[test]
    fn planner_verification_takes_precedence_but_toolchain_appears_below() {
        use phantom_toolchain::{DetectedLanguage, Toolchain};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.md");
        let plan = sample_plan(None); // `cargo test` is the planner command.

        let toolchain = Toolchain {
            language: Some(DetectedLanguage::Rust),
            test_cmd: Some("cargo test --all".into()),
            build_cmd: Some("cargo build".into()),
            ..Toolchain::empty()
        };

        write_plan_domain_instructions_with_toolchain(
            &path,
            &plan.domains[0],
            &plan,
            None,
            Some(&toolchain),
        )
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();

        let planner_pos = body.find("cargo test\n").expect("planner command missing");
        let auto_pos = body.find("Auto-detected").expect("auto-detected missing");
        let detected_pos = body.find("cargo test --all").expect("detected missing");
        assert!(planner_pos < auto_pos);
        assert!(auto_pos < detected_pos);
    }

    #[test]
    fn upstream_domains_section_lists_agent_id_and_files() {
        use std::path::PathBuf;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.md");

        let plan = Plan {
            id: PlanId("plan-abc".into()),
            request: "multi-domain work".into(),
            created_at: chrono::Utc::now(),
            domains: vec![
                PlanDomain {
                    name: "auth".into(),
                    agent_id: "plan-abc-auth".into(),
                    description: "implement login".into(),
                    files_to_modify: vec![PathBuf::from("src/auth.rs")],
                    files_not_to_modify: vec![],
                    requirements: vec![],
                    verification: vec![],
                    depends_on: vec![],
                    category: None,
                },
                PlanDomain {
                    name: "api".into(),
                    agent_id: "plan-abc-api".into(),
                    description: "wire /login endpoint".into(),
                    files_to_modify: vec![PathBuf::from("src/api.rs")],
                    files_not_to_modify: vec!["src/auth.rs".into()],
                    requirements: vec![],
                    verification: vec![],
                    depends_on: vec!["auth".into()],
                    category: None,
                },
            ],
            status: PlanStatus::Draft,
        };

        write_plan_domain_instructions(&path, &plan.domains[1], &plan, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();

        assert!(
            body.contains("## Upstream Domains"),
            "expected Upstream Domains section: {body}"
        );
        assert!(body.contains("agent `plan-abc-auth`"));
        assert!(body.contains("`src/auth.rs`"));
        assert!(body.contains("implement login"));
        assert!(body.contains(".phantom-trunk-update.md"));
        assert!(body.contains("Impacted Dependencies"));
    }

    #[test]
    fn upstream_domains_section_handles_missing_dependency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inst.md");

        let plan = Plan {
            id: PlanId("plan-x".into()),
            request: "r".into(),
            created_at: chrono::Utc::now(),
            domains: vec![PlanDomain {
                name: "solo".into(),
                agent_id: "plan-x-solo".into(),
                description: "solo".into(),
                files_to_modify: vec![],
                files_not_to_modify: vec![],
                requirements: vec![],
                verification: vec![],
                depends_on: vec!["missing_dep".into()],
                category: None,
            }],
            status: PlanStatus::Draft,
        };

        write_plan_domain_instructions(&path, &plan.domains[0], &plan, None).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("**missing_dep** (domain not found"));
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
        let prefix_len = super::super::category_rules::rules_body(&TaskCategory::Adaptive)
            .expect("Adaptive has a canonical body")
            .len();
        assert_eq!(
            &a[..prefix_len],
            &b[..prefix_len],
            "adaptive rules prefix must be byte-identical across domains"
        );
    }
}

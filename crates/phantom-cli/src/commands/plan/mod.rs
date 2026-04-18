//! `phantom plan` — decompose a feature request into parallel agent tasks.
//!
//! Spawns an AI planner to analyze the codebase and break the request into
//! independent domains. For each domain, creates an overlay with a custom
//! instruction file and dispatches a background agent.

mod dispatch;
mod display;
mod planner;
mod validate;

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, PlanId};
use phantom_core::plan::{Plan, PlanDomain, PlanStatus, RawPlanOutput};
use phantom_core::traits::EventStore;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct PlanArgs {
    /// Description of what to implement (opens interactive editor if omitted)
    pub description: Option<String>,
    /// Skip confirmation and dispatch immediately
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Show the plan without dispatching
    #[arg(long)]
    pub dry_run: bool,
    /// Don't auto-submit (agents will wait for manual submit)
    #[arg(long)]
    pub no_submit: bool,
}

pub async fn run(args: PlanArgs) -> anyhow::Result<()> {
    let description = match args.description {
        Some(d) => d,
        None => match crate::ui::textbox::multiline_input(
            "Describe what to implement:",
            "Enter your plan description...",
        )? {
            Some(d) if !d.trim().is_empty() => d,
            _ => {
                println!("Aborted.");
                return Ok(());
            }
        },
    };

    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;

    // Fail fast if the repo has no initial commit; materialization cannot
    // anchor against a null OID. Checking here avoids a needless LLM call
    // to the planner.
    let git = ctx.open_git()?;
    let head = git
        .head_oid()
        .context("failed to read HEAD before planning")?;
    crate::context::require_initialized_head(&head)?;
    drop(git);

    // Step 1: Generate plan via AI planner.
    println!(
        "\n  {} Analyzing codebase for: {}",
        console::style("⟳").cyan(),
        console::style(&description).bold()
    );
    println!();

    let raw_output = planner::run_planner(&ctx.repo_root, &ctx.phantom_dir, &description)?;

    // Step 2: Build the Plan struct.
    let plan_id = generate_plan_id();
    let plan = build_plan(&plan_id, &description, raw_output);

    if plan.domains.is_empty() {
        crate::ui::empty_state("Planner returned no domains. Nothing to dispatch.", None);
        return Ok(());
    }

    // Step 3: Display the plan.
    display::display_plan(&plan);

    if args.dry_run {
        println!("  {}", console::style("(dry run — not dispatching)").dim());
        return Ok(());
    }

    // Step 4: Confirm.
    if !args.yes {
        print!(
            "  Dispatch {} agent(s)? [Y/n] ",
            console::style(plan.domains.len()).bold()
        );
        use std::io::Write;
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();
        if input == "n" || input == "no" {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Step 5: Persist the plan.
    let plan_dir = ctx.phantom_dir.join("plans").join(&plan_id.0);
    std::fs::create_dir_all(&plan_dir)
        .with_context(|| format!("failed to create plan directory {}", plan_dir.display()))?;

    let plan_json = serde_json::to_string_pretty(&plan).context("failed to serialize plan")?;
    std::fs::write(plan_dir.join("plan.json"), &plan_json).context("failed to write plan.json")?;

    // Step 5b: Validate no cycles in dependency graph.
    validate::validate_no_cycles(&plan.domains)?;

    // Step 5c: Warn about file overlap between parallel domains.
    validate::warn_parallel_file_overlap(&plan);

    // Step 6: Dispatch agents.
    let mut plan = plan;
    let mut dispatched_agents = Vec::new();
    let mut overlays = ctx.open_overlays_restored()?;

    for domain in &plan.domains {
        // Resolve domain name dependencies to agent IDs.
        let upstream_agent_ids: Vec<String> = domain
            .depends_on
            .iter()
            .filter_map(|dep_name| {
                plan.domains
                    .iter()
                    .find(|d| d.name == *dep_name)
                    .map(|d| d.agent_id.clone())
            })
            .collect();

        dispatch::dispatch_domain(
            &ctx,
            &events,
            &mut overlays,
            &plan,
            domain,
            &plan_dir,
            &upstream_agent_ids,
        )
        .await?;
        dispatched_agents.push(AgentId(domain.agent_id.clone()));
    }

    // Step 7: Emit PlanCreated event and update persisted status.
    plan.status = PlanStatus::Dispatched;

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: ChangesetId(format!("plan-{plan_id}")),
        agent_id: AgentId("phantom-planner".into()),
        causal_parent: None,
        kind: EventKind::PlanCreated {
            plan_id: plan_id.clone(),
            request: description.clone(),
            domain_count: plan.domains.len() as u32,
            agent_ids: dispatched_agents,
        },
    };
    events.append(event).await?;

    let plan_json = serde_json::to_string_pretty(&plan).context("failed to serialize plan")?;
    std::fs::write(plan_dir.join("plan.json"), &plan_json).context("failed to update plan.json")?;

    println!();
    crate::ui::action_hint("ph background", "to watch progress.");
    crate::ui::action_hint("ph status", "to see all agents.");

    Ok(())
}

/// Generate a timestamp-based plan ID.
fn generate_plan_id() -> PlanId {
    let now = Utc::now();
    PlanId(now.format("plan-%Y%m%d-%H%M%S").to_string())
}

/// Convert raw planner output into a full Plan struct.
fn build_plan(plan_id: &PlanId, request: &str, raw: RawPlanOutput) -> Plan {
    let domains = raw
        .domains
        .into_iter()
        .map(|d| {
            let agent_id = d.name.clone();
            PlanDomain {
                name: d.name,
                agent_id,
                description: d.description,
                files_to_modify: d.files_to_modify,
                files_not_to_modify: d.files_not_to_modify,
                requirements: d.requirements,
                verification: d.verification,
                depends_on: d.depends_on,
                category: d.category,
            }
        })
        .collect();

    Plan {
        id: plan_id.clone(),
        request: request.to_string(),
        created_at: Utc::now(),
        domains,
        status: PlanStatus::Draft,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_plan_assigns_agent_ids() {
        let raw = RawPlanOutput {
            domains: vec![phantom_core::plan::RawPlanDomain {
                name: "rate-limiting".into(),
                description: "add rate limiting".into(),
                files_to_modify: vec!["src/lib.rs".into()],
                files_not_to_modify: vec![],
                requirements: vec!["impl token bucket".into()],
                verification: vec!["cargo test".into()],
                depends_on: vec![],
                category: None,
            }],
        };
        let plan_id = PlanId("plan-20260413-143022".into());
        let plan = build_plan(&plan_id, "test", raw);
        assert_eq!(plan.domains[0].agent_id, "rate-limiting");
        assert_eq!(plan.status, PlanStatus::Draft);
    }

    #[test]
    fn generate_plan_id_has_expected_format() {
        let id = generate_plan_id();
        assert!(id.0.starts_with("plan-"));
        assert!(id.0.len() > 10);
    }
}

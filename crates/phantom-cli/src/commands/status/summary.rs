//! Summary view: all overlays, pending changesets, and conflicts.

use phantom_core::changeset::ChangesetStatus;
use phantom_core::event::EventKind;
use phantom_core::id::AgentId;
use phantom_events::{Projection, SnapshotManager, SqliteEventStore};
use phantom_git::GitOps;

use super::run_state::{AgentRunState, format_duration, read_agent_run_state};
use crate::context::PhantomContext;
use crate::ui;

/// Extract the plan prefix from an agent ID, if it matches the plan naming pattern.
///
/// Plan agent IDs follow: `plan-YYYYMMDD-HHMMSS-domain-name`.
/// Returns `Some("plan-YYYYMMDD-HHMMSS")` if matched.
pub fn extract_plan_prefix(agent_id: &str) -> Option<String> {
    if !agent_id.starts_with("plan-") {
        return None;
    }
    // Expected format: plan-YYYYMMDD-HHMMSS-rest
    // The prefix is the first 22 characters: "plan-" (5) + "YYYYMMDD" (8) + "-" (1) + "HHMMSS" (6) + "-" (1) = 21
    // But we need to be flexible. Split by '-' and take plan + date + time.
    let parts: Vec<&str> = agent_id.splitn(4, '-').collect();
    if parts.len() >= 4 && parts[1].len() == 8 && parts[2].len() == 6 {
        Some(format!("{}-{}-{}", parts[0], parts[1], parts[2]))
    } else {
        None
    }
}

/// Summary view: show all overlays, pending changesets, and conflicts.
pub(super) async fn run_summary(
    ctx: &PhantomContext,
    git: &GitOps,
    events: &SqliteEventStore,
    agent_ids: &[AgentId],
) -> anyhow::Result<()> {
    let phantom_dir = &ctx.phantom_dir;
    let head = git.head_oid()?;

    let projection = SnapshotManager::new(events).build_projection().await?;

    // Header
    let head_short = head.to_hex();
    let head_short = &head_short[..12.min(head_short.len())];
    println!(
        "{} {}",
        ui::style_dim("Trunk HEAD:"),
        ui::style_cyan(head_short)
    );
    println!();

    // All overlays that exist on disk.
    let mut overlay_agents: Vec<&AgentId> = agent_ids.iter().collect();
    overlay_agents.sort_by(|a, b| a.0.cmp(&b.0));

    // Detect plans: find PlanCreated events to map plan IDs to their request text.
    // Uses a targeted query with kind_prefix filter instead of loading all events.
    let mut plan_agents: std::collections::HashMap<String, Vec<&AgentId>> =
        std::collections::HashMap::new();
    let mut standalone_agents: Vec<&AgentId> = Vec::new();
    let mut plan_requests: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // Collect plan metadata via targeted query (only PlanCreated events).
    let plan_events = events
        .query(&crate::services::event_queries::plans_only())
        .await?;
    for event in &plan_events {
        if let EventKind::PlanCreated {
            plan_id, request, ..
        } = &event.kind
        {
            plan_requests.insert(plan_id.0.clone(), request.clone());
        }
    }

    // Classify agents into plan groups vs standalone.
    for agent in &overlay_agents {
        let plan_prefix = extract_plan_prefix(&agent.0);
        if let Some(prefix) = plan_prefix {
            plan_agents.entry(prefix).or_default().push(agent);
        } else {
            standalone_agents.push(agent);
        }
    }

    if overlay_agents.is_empty() {
        println!(
            "  {} {}",
            ui::style_bold("Overlays:"),
            ui::style_dim("(none)")
        );
    } else {
        ui::section_header("Overlays");

        let width = ui::term_width();

        // Print plan groups first.
        for (plan_prefix, agents) in &plan_agents {
            // Prefix: "  Plan: " + plan_prefix + " — "
            let prefix_len = 2 + 6 + plan_prefix.len() + 3;
            let request = plan_requests
                .get(plan_prefix)
                .map(|r| ui::truncate_line(r, width.saturating_sub(prefix_len)))
                .unwrap_or_default();
            println!(
                "  {} {} {} {}",
                ui::style_dim("Plan:"),
                ui::style_cyan(plan_prefix),
                ui::style_dim("—"),
                request
            );

            for agent in agents {
                let run_state = read_agent_run_state(phantom_dir, &agent.0);
                let indicator = ui::run_state_indicator(&run_state);
                let state_text = ui::run_state_text(&run_state);
                let status = latest_changeset_status(&projection, agent);
                let domain_name = agent
                    .0
                    .strip_prefix(&format!("{plan_prefix}-"))
                    .unwrap_or(&agent.0);
                println!("    {indicator} {domain_name:<20} {state_text:<12} {status}");
            }
            println!();
        }

        // Print standalone agents.
        for agent in &standalone_agents {
            let run_state = read_agent_run_state(phantom_dir, &agent.0);
            let indicator = ui::run_state_indicator(&run_state);
            let state_text = ui::run_state_text(&run_state);
            let elapsed_raw = match &run_state {
                AgentRunState::Running { elapsed, .. } => Some(format_duration(elapsed)),
                _ => None,
            };
            let elapsed = elapsed_raw
                .as_ref()
                .map(|e| format!(" {}", ui::style_dim(e)))
                .unwrap_or_default();
            let status = latest_changeset_status(&projection, agent);

            let task = projection
                .changesets_for_agent(agent)
                .first()
                .map(|cs| cs.task.as_str())
                .filter(|t| !t.is_empty());

            if let Some(task) = task {
                // Prefix: "  " + indicator(2) + " " + agent(14) + " " + state(12) + " elapsed" + " " + status + "  "
                let elapsed_visible = elapsed_raw.as_ref().map_or(0, |e| e.len() + 1);
                let status_visible = console::measure_text_width(&status);
                let prefix_len = 2 + 2 + 1 + 14 + 1 + 12 + elapsed_visible + 1 + status_visible + 2;
                let truncated = ui::truncate_line(task, width.saturating_sub(prefix_len));
                println!(
                    "  {indicator} {agent:<14} {state_text:<12}{elapsed} {status}  {}",
                    ui::style_dim(&truncated)
                );
            } else {
                println!("  {indicator} {agent:<14} {state_text:<12}{elapsed} {status}");
            }
        }
    }
    println!();

    // Pending: overlays with modified files that haven't been submitted yet.
    // Only create OverlayLayer for agents with InProgress changesets.
    let pending_agent_ids: Vec<&AgentId> = overlay_agents
        .iter()
        .copied()
        .filter(|agent| {
            projection
                .changesets_for_agent(agent)
                .first()
                .is_some_and(|cs| cs.status == ChangesetStatus::InProgress)
        })
        .collect();

    let mut pending_overlays: Vec<(&AgentId, usize)> = Vec::new();
    if !pending_agent_ids.is_empty() {
        let mut mgr = ctx.open_overlays();
        for agent_id in &pending_agent_ids {
            let _ = mgr.create_overlay((*agent_id).clone(), &ctx.repo_root);
        }
        for agent_id in &pending_agent_ids {
            if let Ok(layer) = mgr.get_layer(agent_id)
                && let Ok(files) = layer.modified_files()
            {
                // Match the submit pipeline: only count files that would
                // actually be submitted. The raw upper-layer walk includes
                // gitignored build artifacts (target/, node_modules/, etc.),
                // inflating the count into the thousands.
                let submittable = files
                    .iter()
                    .filter(|p| !git.is_ignored(p).unwrap_or(false))
                    .count();
                if submittable > 0 {
                    pending_overlays.push((agent_id, submittable));
                }
            }
        }
    }
    if pending_overlays.is_empty() {
        println!(
            "  {} {}",
            ui::style_bold("Pending changesets:"),
            ui::style_dim("(none)")
        );
    } else {
        ui::section_header("Pending changesets");
        println!(
            "  {}",
            ui::style_dim(&format!("{:<14} {:>7}", "AGENT", "CHANGES"))
        );
        for (agent_id, file_count) in &pending_overlays {
            println!("  {agent_id:<14} {file_count:>7}");
        }
    }
    println!();

    // Conflicted changesets
    let conflicted = projection.conflicted_changesets();
    if !conflicted.is_empty() {
        ui::section_header("Conflicted changesets");
        println!(
            "  {}",
            ui::style_dim(&format!("{:<14} {:>7}   STATUS", "AGENT", "CHANGES"))
        );
        for cs in &conflicted {
            println!(
                "  {:<14} {:>7}   {}",
                cs.agent_id,
                cs.files_touched.len(),
                ui::status_label(cs.status),
            );
        }
        println!();
    }

    let event_count = events.event_count().await?;
    println!("{}", ui::style_dim(&format!("Total events: {event_count}")));

    Ok(())
}

/// Get the styled status label for the latest changeset of an agent.
fn latest_changeset_status(projection: &Projection, agent_id: &AgentId) -> String {
    let changesets = projection.changesets_for_agent(agent_id);
    match changesets.first() {
        Some(cs) => format!("{}", ui::status_label(cs.status)),
        None => format!("{}", ui::style_dim("no changeset")),
    }
}

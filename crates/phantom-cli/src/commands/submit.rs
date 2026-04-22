//! `phantom submit` — submit an agent's work and merge it to trunk.

use anyhow::Context;
use phantom_core::event::EventKind;
use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::EventStore;
use phantom_events::Projection;
use phantom_orchestrator::materialization_service::ActiveOverlay;
use phantom_orchestrator::materializer::{MaterializeResult, Materializer};
use phantom_orchestrator::submit_service;
use tracing::info;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct SubmitArgs {
    /// Agent identifier whose work to submit
    pub agent: String,

    /// Commit message. Defaults to the agent name if omitted.
    #[arg(short, long)]
    pub message: Option<String>,
}

pub async fn run(args: SubmitArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let overlays = ctx.open_overlays_restored()?;
    let agent_id = crate::services::validate::agent_id(&args.agent)?;
    let message = args.message;

    match submit_agent(&ctx, &events, &overlays, &agent_id, message.as_deref()).await? {
        Some(changeset_id) => {
            println!(
                "  {} Changeset {} submitted.",
                console::style("✓").green(),
                console::style(&changeset_id.to_string()).bold()
            );

            // Auto-remove overlay for background agents after successful submit.
            let agent_events = events.query_by_agent(&agent_id).await?;
            let projection = Projection::from_events(&agent_events);
            let is_background = projection
                .changeset(&changeset_id)
                .is_some_and(|cs| cs.agent_pid.is_some());

            if is_background {
                info!(agent = %agent_id, "auto-removing background agent overlay after successful submit");
                super::remove::remove_agent_overlay(&ctx, &agent_id, &changeset_id).await;
                println!(
                    "  {} Background agent '{}' overlay cleaned up.",
                    console::style("♻").dim(),
                    console::style(&args.agent).bold()
                );
            }
        }
        None => {
            println!(
                "  {} No modified files found for agent '{}'.",
                console::style("·").dim(),
                args.agent
            );
        }
    }

    Ok(())
}

/// Submit an agent's overlay work: extract semantic operations, merge to trunk,
/// and ripple to other agents.
///
/// Returns `Some(changeset_id)` if changes were found and processed,
/// or `None` if the overlay has no modifications.
pub async fn submit_agent(
    ctx: &PhantomContext,
    events: &dyn EventStore,
    overlays: &phantom_overlay::OverlayManager,
    agent_id: &AgentId,
    message: Option<&str>,
) -> anyhow::Result<Option<ChangesetId>> {
    let layer = overlays
        .get_layer(agent_id)
        .with_context(|| format!("no overlay found for agent '{agent_id}'"))?;

    let upper_dir = overlays
        .upper_dir(agent_id)
        .with_context(|| format!("no upper dir for agent '{agent_id}'"))?;

    let git = ctx.open_git()?;
    let analyzer = ctx.semantic();

    let materializer = Materializer::new(&git);

    let active_overlays = build_active_overlays(events, overlays, agent_id).await?;

    let output = submit_service::submit_and_materialize(
        &git,
        events,
        &analyzer,
        agent_id,
        layer,
        upper_dir,
        &ctx.phantom_dir,
        &materializer,
        &active_overlays,
        message,
    )
    .await?;

    match output {
        Some(out) => {
            // Print submission stats.
            println!(
                "    {} additions, {} modifications, {} deletions across {} file(s)",
                console::style(out.submit.additions).green(),
                console::style(out.submit.modifications).yellow(),
                console::style(out.submit.deletions).red(),
                out.submit.modified_files.len()
            );
            for f in &out.submit.modified_files {
                println!("    {}", console::style(f.display().to_string()).dim());
            }

            // Print materialization result.
            match out.materialize.result {
                MaterializeResult::Success {
                    new_commit,
                    text_fallback_files,
                } => {
                    let short = new_commit.to_hex();
                    let short = &short[..12.min(short.len())];
                    println!(
                        "  {} Materialized → commit {}",
                        console::style("✓").green(),
                        console::style(short).cyan()
                    );

                    if !text_fallback_files.is_empty() {
                        eprintln!(
                            "\n  {} {} file(s) merged via line-based fallback (no syntax validation):",
                            console::style("⚠").yellow(),
                            text_fallback_files.len()
                        );
                        for f in &text_fallback_files {
                            eprintln!("    {}", console::style(format!("- {}", f.display())).dim());
                        }
                        eprintln!(
                            "  {}\n",
                            console::style("Review these files before deploying.").yellow()
                        );
                    }

                    if !out.materialize.ripple_effects.is_empty() {
                        println!(
                            "\n  {} The following agents may be affected:",
                            console::style("↻").cyan()
                        );
                        for effect in &out.materialize.ripple_effects {
                            if effect.merged_count > 0 || effect.conflicted_count > 0 {
                                println!(
                                    "    {} {} file(s) ({} merged, {} conflicted)",
                                    console::style(&effect.agent_id.to_string()).bold(),
                                    effect.files.len(),
                                    console::style(effect.merged_count).green(),
                                    console::style(effect.conflicted_count).red(),
                                );
                            } else if effect.files.is_empty() && effect.dep_impact_count > 0 {
                                // Dep-only ripple: no file overlap, but the
                                // agent references a trunk symbol that
                                // changed. Annotate so "0 file(s)" doesn't
                                // read as a no-op.
                                println!(
                                    "    {} 0 file(s) {}",
                                    console::style(&effect.agent_id.to_string()).bold(),
                                    console::style(format!(
                                        "[dep-graph: {} symbol impact{}]",
                                        effect.dep_impact_count,
                                        if effect.dep_impact_count == 1 { "" } else { "s" }
                                    ))
                                    .cyan(),
                                );
                            } else {
                                println!(
                                    "    {} {} file(s)",
                                    console::style(&effect.agent_id.to_string()).bold(),
                                    effect.files.len()
                                );
                            }
                        }
                    }
                }
                MaterializeResult::Conflict { details } => {
                    eprintln!(
                        "\n  {} Submission of {} failed with {} conflict(s):\n",
                        console::style("✗").red(),
                        out.submit.changeset_id,
                        details.len()
                    );
                    for detail in &details {
                        let kind_label = match detail.kind {
                            phantom_core::ConflictKind::BothModifiedSymbol => "both modified",
                            phantom_core::ConflictKind::ModifyDeleteSymbol => "modify/delete",
                            phantom_core::ConflictKind::BothModifiedDependencyVersion => {
                                "dependency version"
                            }
                            phantom_core::ConflictKind::RawTextConflict => "text conflict",
                            phantom_core::ConflictKind::BinaryFile => "binary file",
                        };
                        let location = format_conflict_location(detail);
                        eprintln!(
                            "  {} {}",
                            console::style(detail.file.display().to_string()).bold(),
                            console::style(format!("[{kind_label}]")).red()
                        );
                        eprintln!("    {}", detail.description);
                        if !location.is_empty() {
                            eprintln!("    {}", console::style(location).dim());
                        }
                        eprintln!();
                    }
                    std::process::exit(1);
                }
            }

            Ok(Some(out.submit.changeset_id))
        }
        None => Ok(None),
    }
}

/// Build the list of active overlays for ripple checking, excluding the
/// submitting agent.
pub(crate) async fn build_active_overlays(
    events: &dyn EventStore,
    overlays: &phantom_overlay::OverlayManager,
    exclude_agent: &AgentId,
) -> anyhow::Result<Vec<ActiveOverlay>> {
    let all_events = events.query_all().await?;
    let projection = Projection::from_events(&all_events);

    let active_overlays: Vec<ActiveOverlay> = projection
        .active_agents()
        .into_iter()
        .filter(|a| a != exclude_agent)
        .filter_map(|a| {
            let agent_cs = all_events
                .iter()
                .rev()
                .filter(|e| e.agent_id == a)
                .find_map(|e| match &e.kind {
                    EventKind::TaskCreated { .. } => Some(e.changeset_id.clone()),
                    _ => None,
                });
            let cs_data = agent_cs.and_then(|cs_id| projection.changeset(&cs_id).cloned());
            let agent_upper = overlays
                .upper_dir(&a)
                .ok()
                .map(std::path::Path::to_path_buf);
            match (cs_data, agent_upper) {
                (Some(cs), Some(upper)) => {
                    // `cs.files_touched` reflects only submitted / event-logged
                    // state. For a *running* agent that has unsaved writes in
                    // its FUSE upper, the projection's list is empty and the
                    // ripple would skip it — even though the agent is the
                    // exact audience an active notification is meant to reach.
                    //
                    // Union the projection view with a live walk of the upper
                    // directory so in-progress work is visible to the ripple.
                    // Filter falls back to the projection list on I/O errors
                    // so we never regress pre-existing behaviour.
                    let live = phantom_overlay::list_modified_files_in_upper(&upper)
                        .unwrap_or_default();
                    let mut files_touched = cs.files_touched.clone();
                    for path in live {
                        if !files_touched.contains(&path) {
                            files_touched.push(path);
                        }
                    }
                    Some(ActiveOverlay {
                        agent_id: a.clone(),
                        files_touched,
                        upper_dir: upper,
                    })
                }
                _ => None,
            }
        })
        .collect();

    Ok(active_overlays)
}

/// Format line-number location info for a conflict detail.
fn format_conflict_location(detail: &phantom_core::ConflictDetail) -> String {
    let mut parts = Vec::new();
    if let Some(span) = &detail.ours_span {
        if span.start_line == span.end_line {
            parts.push(format!("ours: line {}", span.start_line));
        } else {
            parts.push(format!("ours: lines {}–{}", span.start_line, span.end_line));
        }
    }
    if let Some(span) = &detail.theirs_span {
        if span.start_line == span.end_line {
            parts.push(format!("theirs: line {}", span.start_line));
        } else {
            parts.push(format!(
                "theirs: lines {}–{}",
                span.start_line, span.end_line
            ));
        }
    }
    if let Some(span) = &detail.base_span {
        if span.start_line == span.end_line {
            parts.push(format!("base: line {}", span.start_line));
        } else {
            parts.push(format!("base: lines {}–{}", span.start_line, span.end_line));
        }
    }
    parts.join(", ")
}

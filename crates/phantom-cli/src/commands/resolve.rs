//! `phantom resolve` — auto-resolve merge conflicts by launching a background
//! Claude Code agent with three-way conflict context.
//!
//! Finds the most recent conflicted changeset for the given agent, extracts
//! the three-way conflict data (base/ours/theirs file versions), generates a
//! specialized `.phantom-task.md` with conflict resolution instructions, and
//! launches a background agent to resolve the conflicts.

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, EventId};
use phantom_core::traits::EventStore;
use phantom_events::Projection;
use phantom_session::context_file::{self, ResolveConflictContext};

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct ResolveArgs {
    /// Agent name whose conflicts to resolve
    pub agent: String,
}

pub async fn run(args: ResolveArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let git = ctx.open_git()?;
    let events = ctx.open_events().await?;
    let overlays = ctx.open_overlays_restored()?;

    let agent_id = AgentId(args.agent.clone());
    let head = git.head_oid().context("failed to read HEAD")?;

    // Find the latest conflicted changeset for this agent.
    let all_events = events.query_all().await?;
    let projection = Projection::from_events(&all_events);

    // Check if a resolution is already in progress.
    if let Some(resolving) = projection.latest_resolving_changeset(&agent_id) {
        anyhow::bail!(
            "changeset {} is already being resolved — wait for the resolve agent to finish \
             or drop the changeset with `phantom rollback --changeset {}`",
            resolving.id,
            resolving.id,
        );
    }

    let changeset = projection
        .latest_conflicted_changeset(&agent_id)
        .with_context(|| format!("no conflicted changeset found for agent '{}'", args.agent))?
        .clone();

    // Guard: if this changeset was already resolved once and re-conflicted,
    // don't allow automatic re-resolution (prevents infinite loops).
    let already_resolved = all_events.iter().any(|e| {
        e.changeset_id == changeset.id
            && matches!(e.kind, EventKind::ConflictResolutionStarted { .. })
    });
    if already_resolved {
        anyhow::bail!(
            "changeset {} already had a resolution attempt that re-conflicted.\n\
             Resolve manually or drop with `phantom rollback --changeset {}`.",
            changeset.id,
            changeset.id,
        );
    }

    // Extract conflict details from the ChangesetConflicted event.
    let conflict_details = all_events
        .iter()
        .filter(|e| e.changeset_id == changeset.id)
        .filter_map(|e| match &e.kind {
            EventKind::ChangesetConflicted { conflicts } => Some(conflicts.clone()),
            _ => None,
        })
        .last()
        .unwrap_or_default();

    if conflict_details.is_empty() {
        anyhow::bail!(
            "changeset {} is marked as conflicted but no conflict details found in the event log",
            changeset.id
        );
    }

    println!(
        "\n  {} Resolving {} conflict(s) for agent '{}' (changeset {})...\n",
        console::style("↻").cyan(),
        conflict_details.len(),
        console::style(&args.agent).bold(),
        console::style(&changeset.id.to_string()).dim()
    );

    // Build the three-way conflict context for each conflict.
    let upper_dir = overlays.upper_dir(&agent_id)?.to_path_buf();

    let mut resolve_contexts = Vec::with_capacity(conflict_details.len());
    for detail in &conflict_details {
        let base_content = git
            .read_file_at_commit(&changeset.base_commit, &detail.file)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());

        let ours_content = git
            .read_file_at_commit(&head, &detail.file)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());

        let theirs_path = upper_dir.join(&detail.file);
        let theirs_content = std::fs::read_to_string(&theirs_path).ok();

        let kind_label = match detail.kind {
            phantom_core::ConflictKind::BothModifiedSymbol => "both modified",
            phantom_core::ConflictKind::ModifyDeleteSymbol => "modify/delete",
            phantom_core::ConflictKind::BothModifiedDependencyVersion => "dependency version",
            phantom_core::ConflictKind::RawTextConflict => "text conflict",
            phantom_core::ConflictKind::BinaryFile => "binary file",
        };
        println!(
            "    {} {} {}",
            console::style(detail.file.display().to_string()).bold(),
            console::style(format!("[{kind_label}]")).red(),
            console::style(&detail.description).dim()
        );

        resolve_contexts.push(ResolveConflictContext {
            detail: detail.clone(),
            base_content,
            ours_content,
            theirs_content,
        });
    }

    // Determine the work directory: FUSE mount if available, otherwise upper dir.
    let mount_point = ctx
        .phantom_dir
        .join("overlays")
        .join(&args.agent)
        .join("mount");
    let work_dir = if is_fuse_mounted(&mount_point) {
        mount_point
    } else {
        upper_dir.clone()
    };

    // Write the static resolution rules to a system prompt file (cached across sessions).
    let rules_path = ctx
        .phantom_dir
        .join("instructions")
        .join(context_file::RESOLVE_RULES_FILE);
    context_file::write_resolve_rules_file(&rules_path)?;

    // Write the dynamic conflict context file (agent info + code blocks only).
    context_file::write_resolve_context_file(
        &work_dir,
        &agent_id,
        &changeset.id,
        &changeset.base_commit,
        &resolve_contexts,
    )?;

    // Emit ConflictResolutionStarted event.
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset.id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::ConflictResolutionStarted {
            conflicts: conflict_details.clone(),
            new_base: Some(head),
        },
    };
    events.append(event).await?;

    // Spawn background agent to resolve conflicts.
    let task = "Resolve merge conflicts per .phantom-task.md";
    super::task::spawn_agent_monitor(
        &ctx.phantom_dir,
        &ctx.repo_root,
        &args.agent,
        &changeset.id,
        task,
        &work_dir,
        true, // auto_materialize
        Some(&rules_path),
    )?;

    let log_file = ctx
        .phantom_dir
        .join("overlays")
        .join(&args.agent)
        .join("agent.log");

    println!();
    println!(
        "  {} Resolve agent launched {}.",
        console::style("✓").green(),
        console::style("(background)").dim()
    );
    super::ui::key_value("Changeset", &changeset.id.to_string());
    super::ui::key_value("Log", log_file.display());
    super::ui::key_value("Overlay", work_dir.display());
    println!();
    println!(
        "  Run {} to check progress.",
        console::style(format!("phantom status {}", args.agent)).bold()
    );

    Ok(())
}

/// Check if a FUSE filesystem is mounted at `mount_point`.
fn is_fuse_mounted(mount_point: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let parent = match mount_point.parent() {
        Some(p) => p,
        None => return false,
    };

    match (std::fs::metadata(mount_point), std::fs::metadata(parent)) {
        (Ok(m), Ok(p)) => m.dev() != p.dev(),
        _ => false,
    }
}

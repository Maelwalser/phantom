//! Per-domain agent dispatch: creates an overlay, mounts FUSE, writes the
//! domain instruction file, and spawns a background agent monitor for a
//! single [`PlanDomain`].

use std::path::Path;

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::EventId;
use phantom_core::plan::{Plan, PlanDomain};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;
use phantom_session::context_file;

use super::super::task::{generate_changeset_id, spawn_agent_monitor};
use crate::context::PhantomContext;

/// Dispatch a single domain as a background agent.
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_domain(
    ctx: &PhantomContext,
    events: &SqliteEventStore,
    overlays: &mut phantom_overlay::OverlayManager,
    plan: &Plan,
    domain: &PlanDomain,
    plan_dir: &Path,
    upstream_agent_ids: &[String],
) -> anyhow::Result<()> {
    let agent_id = crate::services::validate::agent_id(&domain.agent_id)?;
    let git = ctx.open_git()?;
    let head = git.head_oid().context("failed to read HEAD")?;

    // Create the overlay.
    let handle = overlays
        .create_overlay(agent_id.clone(), &ctx.repo_root)
        .with_context(|| format!("failed to create overlay for {}", domain.agent_id))?;
    let mount_point = handle.mount_point.clone();
    let upper_dir = handle.upper_dir.clone();

    // Generate changeset ID.
    let cs_id = generate_changeset_id();

    // Emit TaskCreated event.
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: cs_id.clone(),
        agent_id: agent_id.clone(),
        causal_parent: None,
        kind: EventKind::TaskCreated {
            base_commit: head,
            task: domain.description.clone(),
        },
    };
    events.append(event).await?;

    // Write initial current_base.
    phantom_orchestrator::live_rebase::write_current_base(&ctx.phantom_dir, &agent_id, &head)
        .context("failed to write initial current_base")?;

    // Spawn FUSE daemon (blocking I/O — run off the async executor).
    let fuse_phantom_dir = ctx.phantom_dir.clone();
    let fuse_repo_root = ctx.repo_root.clone();
    let fuse_agent = domain.agent_id.clone();
    let fuse_mount = mount_point.clone();
    let fuse_upper = upper_dir.clone();
    let fuse_mounted = tokio::task::spawn_blocking(move || {
        crate::fs::fuse::spawn_daemon(
            &fuse_phantom_dir,
            &fuse_repo_root,
            &fuse_agent,
            &fuse_mount,
            &fuse_upper,
            &crate::fs::fuse::FsOverrides::default(),
            std::time::Duration::from_secs(5),
        )
        .is_ok()
    })
    .await
    .unwrap_or(false);

    let work_dir = if fuse_mounted {
        mount_point.clone()
    } else {
        upper_dir.clone()
    };

    // Extract cross-domain signatures for token-efficient context injection.
    let cross_domain_sigs =
        phantom_session::signatures::extract_cross_domain_signatures(&ctx.repo_root, domain, plan);
    let sigs_ref = if cross_domain_sigs.is_empty() {
        None
    } else {
        Some(cross_domain_sigs.as_str())
    };

    // Detect the repo's toolchain once per-domain so the generated instructions
    // and context file reference concrete commands for the agent's language.
    let detector = phantom_toolchain::ToolchainDetector::new();
    let toolchain = detector.detect_repo_root(&ctx.repo_root);

    // Generate the domain instruction file.
    let instructions_dir = plan_dir.join("instructions");
    let instructions_path = instructions_dir.join(format!("{}.md", domain.agent_id));
    context_file::write_plan_domain_instructions_with_toolchain(
        &instructions_path,
        domain,
        plan,
        sigs_ref,
        Some(&toolchain),
    )?;

    // Write context file into overlay.
    context_file::write_context_file_with_toolchain(
        &work_dir,
        &agent_id,
        &cs_id,
        &head,
        Some(&domain.description),
        Some(&toolchain),
    )?;

    // Spawn the agent monitor with the custom instruction file.
    let cli_command = crate::context::default_cli(&ctx.phantom_dir);
    spawn_agent_monitor(
        &ctx.phantom_dir,
        &ctx.repo_root,
        &domain.agent_id,
        &cs_id,
        &domain.description,
        &work_dir,
        &cli_command,
        Some(&instructions_path),
        upstream_agent_ids,
    )?;

    let log_file = ctx
        .phantom_dir
        .join("overlays")
        .join(&domain.agent_id)
        .join("agent.log");

    println!(
        "  {} Agent '{}' tasked {}",
        console::style("✓").green(),
        console::style(&domain.agent_id).bold(),
        console::style("(background)").dim()
    );
    crate::ui::key_value("Changeset", console::style(&cs_id.to_string()).cyan());
    crate::ui::key_value("Task", &domain.description);
    crate::ui::key_value("Log", console::style(log_file.display()).dim());
    crate::ui::key_value("Overlay", console::style(work_dir.display()).dim());
    if fuse_mounted {
        crate::ui::key_value("FUSE", console::style("mounted").green());
    }
    if !upstream_agent_ids.is_empty() {
        let deps: Vec<String> = upstream_agent_ids
            .iter()
            .map(|a| console::style(a).bold().to_string())
            .collect();
        crate::ui::key_value("Waiting", deps.join(", "));
    }
    println!();

    Ok(())
}

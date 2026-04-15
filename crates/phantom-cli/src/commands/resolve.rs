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
use phantom_events::SnapshotManager;
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
    let projection = SnapshotManager::new(&events).build_projection().await?;
    let all_events = events.query_all().await?;

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
        .next_back()
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

    // Group conflicts by file for parallel resolution.
    let groups = group_conflicts_by_file(resolve_contexts);

    // Emit ConflictResolutionStarted event.
    let causal_parent = events
        .latest_event_for_changeset(&changeset.id)
        .await
        .unwrap_or(None);
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset.id.clone(),
        agent_id: agent_id.clone(),
        causal_parent,
        kind: EventKind::ConflictResolutionStarted {
            conflicts: conflict_details.clone(),
            new_base: Some(head),
        },
    };
    events.append(event).await?;

    // Determine CLI to use: saved session preference or config default.
    let cli_command = phantom_session::adapter::load_session(&ctx.phantom_dir, &agent_id).map_or_else(|| crate::context::default_cli(&ctx.phantom_dir), |s| s.cli_name);

    if groups.len() <= 1 {
        // Single file group — existing single-agent background path.
        let conflicts = groups.into_iter().next().unwrap_or_default();
        context_file::write_resolve_context_file(
            &work_dir,
            &agent_id,
            &changeset.id,
            &changeset.base_commit,
            &conflicts,
            None,
        )?;

        let task = "Resolve merge conflicts per .phantom-task.md";
        super::task::spawn_agent_monitor(
            &ctx.phantom_dir,
            &ctx.repo_root,
            &args.agent,
            &changeset.id,
            task,
            &work_dir,
            &cli_command,
            Some(&rules_path),
            &[],
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
        super::ui::key_value("Changeset", changeset.id.to_string());
        super::ui::key_value("Log", log_file.display());
        super::ui::key_value("Overlay", work_dir.display());
    } else {
        // Multiple independent file groups — spawn parallel resolve agents.
        println!(
            "\n  {} Splitting into {} parallel resolve agents (one per file)...\n",
            console::style("||").cyan(),
            groups.len()
        );

        let mut context_files = Vec::with_capacity(groups.len());
        for (i, group) in groups.iter().enumerate() {
            let path = context_file::write_resolve_context_file(
                &work_dir,
                &agent_id,
                &changeset.id,
                &changeset.base_commit,
                group,
                Some(i),
            )?;
            context_files.push(path);
        }

        let exit_codes = spawn_parallel_resolve_agents(
            &ctx.phantom_dir,
            &args.agent,
            &cli_command,
            &work_dir,
            &rules_path,
            &context_files,
        )?;

        // Clean up parallel context files.
        for path in &context_files {
            let _ = std::fs::remove_file(path);
        }

        // Check results.
        let failed: Vec<(usize, Option<i32>)> = exit_codes
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, code)| *code != Some(0))
            .collect();

        if !failed.is_empty() {
            for (i, code) in &failed {
                let code_str = code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into());
                eprintln!(
                    "  {} Resolve agent {} exited with code {}",
                    console::style("!").red(),
                    i,
                    code_str
                );
            }
            anyhow::bail!(
                "{} of {} resolve agents failed",
                failed.len(),
                exit_codes.len()
            );
        }

        println!(
            "  {} All {} resolve agents completed successfully.",
            console::style("✓").green(),
            exit_codes.len()
        );

        // Run post-session flow once for the whole overlay (submit + materialize).
        let mut overlays = ctx.open_overlays_restored()?;
        let _outcome = phantom_session::post_session::post_session_flow(
            phantom_session::post_session::PostSessionContext {
                phantom_dir: &ctx.phantom_dir,
                repo_root: &ctx.repo_root,
                events: &events,
                overlays: &mut overlays,
                agent_id: &agent_id,
                changeset_id: &changeset.id,
                auto_submit: true,
            },
        )
        .await?;

        println!(
            "  {} Changes submitted and materialized.",
            console::style("✓").green()
        );
    }

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

    let Some(parent) = mount_point.parent() else {
        return false;
    };

    match (std::fs::metadata(mount_point), std::fs::metadata(parent)) {
        (Ok(m), Ok(p)) => m.dev() != p.dev(),
        _ => false,
    }
}

/// Group conflicts by file path so independent files can be resolved in parallel.
fn group_conflicts_by_file(
    contexts: Vec<ResolveConflictContext>,
) -> Vec<Vec<ResolveConflictContext>> {
    use std::collections::BTreeMap;
    let mut by_file: BTreeMap<std::path::PathBuf, Vec<ResolveConflictContext>> = BTreeMap::new();
    for ctx in contexts {
        by_file.entry(ctx.detail.file.clone()).or_default().push(ctx);
    }
    by_file.into_values().collect()
}

/// Spawn parallel headless agent processes for independent file groups.
///
/// Each process gets its own context file and log file. All processes share
/// the same work directory (safe because file groups are disjoint).
///
/// Returns the exit code of each agent (indexed by group).
fn spawn_parallel_resolve_agents(
    phantom_dir: &std::path::Path,
    agent: &str,
    cli_command: &str,
    work_dir: &std::path::Path,
    rules_path: &std::path::Path,
    context_files: &[std::path::PathBuf],
) -> anyhow::Result<Vec<Option<i32>>> {
    use phantom_session::adapter;

    let overlay_root = phantom_dir.join("overlays").join(agent);
    let cli_adapter = adapter::adapter_for(cli_command);

    let env_vars: Vec<(&str, &str)> = vec![
        ("PHANTOM_AGENT_ID", agent),
        ("PHANTOM_INTERACTIVE", "0"),
    ];

    let mut children = Vec::with_capacity(context_files.len());

    for (i, context_file) in context_files.iter().enumerate() {
        let log_file = overlay_root.join(format!("resolve-{i}.log"));
        let log_handle = std::fs::File::create(&log_file)
            .with_context(|| format!("failed to create resolve log at {}", log_file.display()))?;
        let log_stderr = log_handle
            .try_clone()
            .context("failed to clone log file handle")?;

        let task = format!(
            "Resolve merge conflicts described in {}",
            context_file.file_name().unwrap_or_default().to_string_lossy()
        );

        let mut cmd = cli_adapter
            .build_headless_command(work_dir, &task, &env_vars, Some(rules_path))
            .context("CLI adapter does not support headless mode")?;

        cmd.stdin(std::process::Stdio::null())
            .stdout(log_handle)
            .stderr(log_stderr);

        let child = cmd.spawn().with_context(|| {
            format!("failed to spawn resolve agent {i} — is '{cli_command}' installed and on PATH?")
        })?;

        println!(
            "    {} Agent {} spawned (PID {})",
            console::style("->").dim(),
            i,
            child.id()
        );

        children.push(child);
    }

    println!(
        "\n  {} Waiting for {} agents to complete...\n",
        console::style("...").dim(),
        children.len()
    );

    let mut exit_codes = Vec::with_capacity(children.len());
    for (i, mut child) in children.into_iter().enumerate() {
        let status = child
            .wait()
            .with_context(|| format!("failed to wait for resolve agent {i}"))?;
        let code = status.code();
        let label = if code == Some(0) {
            console::style("ok").green().to_string()
        } else {
            console::style(format!(
                "exit {}",
                code.map_or_else(|| "signal".into(), |c| c.to_string())
            ))
            .red()
            .to_string()
        };
        println!("    Agent {i}: {label}");
        exit_codes.push(code);
    }

    // Clean up resolve log files.
    for i in 0..context_files.len() {
        let _ = std::fs::remove_file(overlay_root.join(format!("resolve-{i}.log")));
    }

    Ok(exit_codes)
}

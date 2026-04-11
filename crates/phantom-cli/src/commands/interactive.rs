//! Interactive session launcher for `phantom dispatch`.
//!
//! Spawns a CLI process (defaults to `claude`) inside the agent's overlay
//! directory with context via environment variables and a generated context
//! file. Handles post-session automation: auto-submit and auto-materialize.

use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;
use phantom_orchestrator::materializer::MaterializeResult;
use tracing::warn;

use super::dispatch::DispatchArgs;
use crate::context::PhantomContext;

/// Name of the generated context file placed in the overlay.
const CONTEXT_FILE: &str = ".phantom-task.md";

/// Run an interactive CLI session inside the agent's overlay.
///
/// Blocks until the spawned process exits, then optionally auto-submits and
/// auto-materializes the changeset.
pub fn run_interactive_session(
    ctx: &mut PhantomContext,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    args: &DispatchArgs,
) -> anyhow::Result<()> {
    let upper_dir = ctx
        .overlays
        .upper_dir(agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .to_path_buf();

    let command = args.command.as_deref().unwrap_or("claude");

    // Write context file into the overlay so the CLI session has agent metadata
    write_context_file(
        &upper_dir,
        agent_id,
        changeset_id,
        base_commit,
        args.task.as_deref(),
    )?;

    // Spawn the interactive process
    let start = Instant::now();
    let mut child = std::process::Command::new(command)
        .current_dir(&upper_dir)
        .env("PHANTOM_AGENT_ID", &agent_id.0)
        .env("PHANTOM_CHANGESET_ID", &changeset_id.0)
        .env(
            "PHANTOM_OVERLAY_DIR",
            upper_dir.to_str().unwrap_or_default(),
        )
        .env(
            "PHANTOM_REPO_ROOT",
            ctx.repo_root.to_str().unwrap_or_default(),
        )
        .env("PHANTOM_INTERACTIVE", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to launch '{command}' — is it installed and on PATH?"))?;

    let pid = child.id();

    // Record InteractiveSessionStarted
    let start_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::InteractiveSessionStarted {
            command: command.to_string(),
            pid,
        },
    };
    ctx.events
        .append(start_event)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Block until the process exits
    let exit_status = child
        .wait()
        .context("failed to wait for interactive session")?;

    let duration_secs = start.elapsed().as_secs();
    let exit_code = exit_status.code();

    // Record InteractiveSessionEnded
    let end_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::InteractiveSessionEnded {
            exit_code,
            duration_secs,
        },
    };
    ctx.events
        .append(end_event)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Remove the generated context file so it doesn't pollute the changeset
    cleanup_context_file(&upper_dir);

    println!();
    if let Some(code) = exit_code {
        if code != 0 {
            println!("Interactive session exited with code {code}.");
        } else {
            println!("Interactive session ended.");
        }
    } else {
        println!("Interactive session terminated by signal.");
    }

    // Post-session automation
    let auto_submit = args.auto_submit || args.auto_materialize;
    post_session_flow(
        ctx,
        agent_id,
        changeset_id,
        auto_submit,
        args.auto_materialize,
    )
}

/// Write a context file into the overlay with agent metadata and optional task.
///
/// Accessible from dispatch for both interactive and background modes.
pub(super) fn write_context_file(
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
- `phantom submit {agent_id}` — submit your changes
- `phantom materialize {changeset_id}` — merge to trunk
- `phantom status` — view all agents and changesets
"#
    );

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write context file to {}", path.display()))?;

    Ok(())
}

/// Remove the generated context file from the overlay.
fn cleanup_context_file(upper_dir: &Path) {
    let path = upper_dir.join(CONTEXT_FILE);
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!(path = %path.display(), error = %e, "failed to clean up context file");
    }
}

/// Handle post-session submit and materialize automation.
fn post_session_flow(
    ctx: &mut PhantomContext,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    auto_submit: bool,
    auto_materialize: bool,
) -> anyhow::Result<()> {
    let layer = ctx
        .overlays
        .get_layer(agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let modified = layer.modified_files().map_err(|e| anyhow::anyhow!("{e}"))?;

    if modified.is_empty() {
        println!("No changes detected in overlay.");
        return Ok(());
    }

    println!("{} file(s) modified in overlay.", modified.len());

    if !auto_submit {
        println!(
            "Run `phantom submit {agent_id}` to submit, then `phantom materialize {changeset_id}` to merge."
        );
        return Ok(());
    }

    // Auto-submit
    println!("Auto-submitting changeset...");
    match super::submit::submit_agent(ctx, agent_id)? {
        Some(cs_id) => {
            println!("Changeset {cs_id} submitted.");

            if auto_materialize {
                println!("Auto-materializing...");
                match super::materialize::materialize_changeset(ctx, &cs_id)? {
                    MaterializeResult::Success { new_commit } => {
                        let hex = new_commit.to_hex();
                        let short = &hex[..12.min(hex.len())];
                        println!("Materialized {cs_id} → commit {short}");
                    }
                    MaterializeResult::Conflict { details } => {
                        eprintln!("Materialization failed with {} conflict(s):", details.len());
                        for detail in &details {
                            eprintln!(
                                "  [{:?}] {} — {}",
                                detail.kind,
                                detail.file.display(),
                                detail.description
                            );
                        }
                        anyhow::bail!("materialization failed due to conflicts");
                    }
                }
            } else {
                println!("Run `phantom materialize {cs_id}` to merge to trunk.");
            }
        }
        None => {
            println!("No changes to submit (files may have been reverted).");
        }
    }

    Ok(())
}

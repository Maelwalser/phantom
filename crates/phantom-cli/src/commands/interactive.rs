//! Interactive session launcher for `phantom <agent>`.
//!
//! Thin wrapper around `phantom_session` that integrates with the CLI's
//! `PhantomContext` and `TaskArgs`.

use std::path::Path;

use chrono::Utc;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_session::adapter::{self, CliSession};
use phantom_session::context_file;
use phantom_session::post_session;
use phantom_session::pty;
use tracing::warn;

use super::task::TaskArgs;
use crate::context::PhantomContext;

/// Run an interactive CLI session inside the agent's overlay.
///
/// Blocks until the spawned process exits, then optionally auto-submits the
/// changeset (which includes materialization to trunk).
pub async fn run_interactive_session(
    ctx: &PhantomContext,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    work_dir: &Path,
    args: &TaskArgs,
    system_prompt_file: Option<&Path>,
) -> anyhow::Result<()> {
    let config_default = crate::context::default_cli(&ctx.phantom_dir);
    let command = args.command.as_deref().unwrap_or(&config_default);
    let cli_adapter = adapter::adapter_for(command);

    // Detect repo toolchain once and thread it through so the context file
    // gets a concrete per-repo verification block.
    let detector = phantom_toolchain::ToolchainDetector::new();
    let toolchain = detector.detect_repo_root(&ctx.repo_root);

    // Write context file into the working directory.
    context_file::write_context_file_with_toolchain(
        work_dir,
        agent_id,
        changeset_id,
        base_commit,
        args.task.as_deref(),
        Some(&toolchain),
    )?;

    // Load a previously saved session for this agent + CLI combination.
    let existing_session = adapter::load_session(&ctx.phantom_dir, agent_id);
    let session_id = existing_session
        .as_ref()
        .filter(|s| s.cli_name == cli_adapter.name())
        .map(|s| s.session_id.as_str());

    // Environment variables passed to the CLI process.
    let env_vars: Vec<(&str, String)> = vec![
        ("PHANTOM_AGENT_ID", agent_id.0.clone()),
        ("PHANTOM_CHANGESET_ID", changeset_id.0.clone()),
        (
            "PHANTOM_OVERLAY_DIR",
            work_dir.to_str().unwrap_or_default().to_string(),
        ),
        (
            "PHANTOM_REPO_ROOT",
            ctx.repo_root.to_str().unwrap_or_default().to_string(),
        ),
        ("PHANTOM_INTERACTIVE", "1".to_string()),
    ];
    let env_refs: Vec<(&str, &str)> = env_vars.iter().map(|(k, v)| (*k, v.as_str())).collect();

    // Use PTY when stdin is a terminal (enables output capture for session IDs).
    // Fall back to direct Stdio::inherit() when not a TTY (tests, CI, piped input).
    let is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
    let (exit_status, captured_session_id) = if is_tty {
        pty::spawn_with_pty(
            cli_adapter.as_ref(),
            work_dir,
            session_id,
            &env_refs,
            system_prompt_file,
        )?
    } else {
        pty::spawn_direct(
            cli_adapter.as_ref(),
            work_dir,
            session_id,
            &env_refs,
            system_prompt_file,
        )?
    };

    // Persist the session ID for next task invocation.
    if let Some(ref sid) = captured_session_id {
        let session = CliSession {
            cli_name: cli_adapter.name().to_string(),
            session_id: sid.clone(),
            last_used: Utc::now(),
        };
        if let Err(e) = adapter::save_session(&ctx.phantom_dir, agent_id, &session) {
            warn!(error = %e, "failed to save CLI session for resume");
        }
    }

    // Cleanup context file if auto-submitting.
    let auto_submit = args.auto_submit;
    if auto_submit {
        let overlays = ctx.open_overlays_restored().ok();
        if let Some(ref overlays) = overlays {
            post_session::cleanup_context_files(work_dir, overlays, agent_id);
        } else {
            context_file::cleanup_context_file(work_dir);
        }
    }

    println!();
    if let Some(code) = exit_status.code() {
        if code != 0 {
            println!("Interactive session exited with code {code}.");
        } else {
            println!("Interactive session ended.");
        }
    } else {
        println!("Interactive session terminated by signal.");
    }

    // Post-session automation.
    let events = ctx.open_events().await?;
    let mut overlays = ctx.open_overlays_restored()?;

    let _outcome = post_session::post_session_flow(post_session::PostSessionContext {
        phantom_dir: &ctx.phantom_dir,
        repo_root: &ctx.repo_root,
        events: &events,
        overlays: &mut overlays,
        agent_id,
        changeset_id,
        auto_submit,
    })
    .await?;

    Ok(())
}

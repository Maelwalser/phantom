//! `phantom _notify-hook` — drain an agent's pending-notification queue and
//! emit Claude-Code-hook-shaped JSON to stdout.
//!
//! This subcommand is wired into the agent's per-overlay Claude settings file
//! (see `phantom_session::hook_config`) and invoked by Claude Code for its
//! `UserPromptSubmit`, `PostToolUse`, and `SessionStart` hooks.
//!
//! ## Output contract
//!
//! When there is at least one pending notification, prints a single JSON
//! object of the form:
//!
//! ```json
//! {
//!   "hookSpecificOutput": {
//!     "hookEventName": "UserPromptSubmit",
//!     "additionalContext": "# Trunk Updates\n..."
//!   }
//! }
//! ```
//!
//! When the queue is empty, prints nothing and exits 0 — Claude treats that
//! as "nothing to inject" and proceeds unmodified. This is the common case.
//!
//! ## Failure mode
//!
//! The hook runs on Claude's critical path before every model call. Failing
//! loud would wedge the agent's session. So any error (missing overlay,
//! corrupt notification file, race with another hook invocation) is swallowed
//! and logged to stderr; the subcommand exits 0 with empty stdout.
//!
//! See `/home/mael/.claude/plans/help-me-research-and-linear-lollipop.md` for
//! the full architecture.

use std::io::Write;
use std::path::PathBuf;

use phantom_core::id::AgentId;
use phantom_orchestrator::pending_notifications;
use serde::Serialize;

use crate::context::PhantomContext;

/// Byte cap for the `additionalContext` string.
///
/// Claude's documented limit on hook-injected context is generous but we keep
/// the budget small (~8 KB) so the prompt cache stays warm and dep-impact
/// lists cannot dominate the model's attention.
const ADDITIONAL_CONTEXT_BUDGET: usize = 8 * 1024;

#[derive(clap::Args, Debug)]
pub struct NotifyHookArgs {
    /// Agent overlay to drain notifications for.
    #[arg(long)]
    pub agent: String,

    /// Which Claude hook event is invoking us. Echoed back in the output so
    /// Claude can route the injected context correctly. Optional because some
    /// CLIs will not supply it; defaults to `UserPromptSubmit`.
    #[arg(long, default_value = "UserPromptSubmit")]
    pub event: String,
}

#[derive(Debug, Serialize)]
struct HookOutput<'a> {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecificOutput<'a>,
}

#[derive(Debug, Serialize)]
struct HookSpecificOutput<'a> {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'a str,
    #[serde(rename = "additionalContext")]
    additional_context: String,
}

/// Entry point for `phantom _notify-hook`.
pub fn run(args: &NotifyHookArgs) -> anyhow::Result<()> {
    let ctx = match PhantomContext::locate() {
        Ok(c) => c,
        Err(e) => {
            // Not inside a Phantom repo — nothing to deliver. Log and move on.
            eprintln!("phantom _notify-hook: {e}");
            trace_invocation(None, &args.agent, &args.event, "no phantom ctx");
            return Ok(());
        }
    };
    let agent_id = AgentId(args.agent.clone());
    trace_invocation(Some(&ctx.phantom_dir), &args.agent, &args.event, "invoked");
    emit_hook_output(
        &ctx.phantom_dir,
        &agent_id,
        &args.event,
        &mut std::io::stdout(),
    )
}

/// Append a one-line trace to `.phantom/notify-hook.log` (or /tmp when no
/// phantom dir was found). Keeps a durable record of every hook invocation
/// so we can tell at a glance whether Claude's hook runner is calling us —
/// essential when debugging an end-to-end demo. Zero impact on the hot path.
fn trace_invocation(phantom_dir: Option<&std::path::Path>, agent: &str, event: &str, note: &str) {
    let log_path = match phantom_dir {
        Some(p) => p.join("notify-hook.log"),
        None => std::path::PathBuf::from("/tmp/phantom-notify-hook.log"),
    };
    let ts = chrono::Utc::now().to_rfc3339();
    let line = format!("{ts} agent={agent} event={event} {note}\n");
    // Best effort: if we cannot log, we still want to proceed.
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}

/// Core logic, split for unit-testing with an injected writer.
fn emit_hook_output(
    phantom_dir: &std::path::Path,
    agent_id: &AgentId,
    event_name: &str,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let paths = match pending_notifications::list(phantom_dir, agent_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("phantom _notify-hook: failed to list queue: {e}");
            return Ok(());
        }
    };
    if paths.is_empty() {
        return Ok(());
    }

    let mut payloads: Vec<(PathBuf, pending_notifications::PendingNotification)> =
        Vec::with_capacity(paths.len());
    for path in paths {
        match pending_notifications::load(&path) {
            Ok(payload) => payloads.push((path, payload)),
            Err(e) => {
                // Corrupt entry: drain it so it does not block future deliveries.
                eprintln!(
                    "phantom _notify-hook: skipping unreadable entry {}: {e}",
                    path.display()
                );
                let _ = pending_notifications::mark_consumed(&path);
            }
        }
    }

    if payloads.is_empty() {
        return Ok(());
    }

    let additional_context = render_additional_context(&payloads);

    // Mark consumed *after* rendering but *before* emitting stdout: if stdout
    // write fails the notification is still considered delivered — Claude's
    // critical path must not re-deliver the same content repeatedly. If the
    // rename fails we log and keep going; next hook invocation will retry.
    for (path, _) in &payloads {
        if let Err(e) = pending_notifications::mark_consumed(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "phantom _notify-hook: failed to mark {} consumed: {e}",
                path.display()
            );
        }
    }

    let output = HookOutput {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: event_name,
            additional_context,
        },
    };
    let json = serde_json::to_string(&output)?;
    writeln!(out, "{json}")?;
    Ok(())
}

/// Concatenate all pending summaries into a single markdown block bounded by
/// [`ADDITIONAL_CONTEXT_BUDGET`]. When the budget is exceeded, an "N more
/// updates omitted — see `.phantom-trunk-update.md`" tail is appended so the
/// model knows where to look for the rest.
fn render_additional_context(
    payloads: &[(PathBuf, pending_notifications::PendingNotification)],
) -> String {
    let mut out = String::from(
        "# Phantom Trunk Updates\n\n\
         The trunk has advanced since your last turn. The updates below \
         describe symbol-level changes relevant to your working set. Review \
         and adjust your plan if any **Impacted Dependencies** apply.\n",
    );
    let budget = ADDITIONAL_CONTEXT_BUDGET.saturating_sub(out.len());
    let mut used = 0usize;
    let mut omitted = 0usize;

    for (i, (_, payload)) in payloads.iter().enumerate() {
        let separator = if i == 0 { "\n" } else { "\n---\n\n" };
        let block_len = separator.len() + payload.summary_md.len();
        // Reserve ~160 bytes for the "N more omitted" tail when it would be
        // needed.
        let tail_reserve = if payloads.len() - i > 1 { 160 } else { 0 };
        if used + block_len + tail_reserve > budget {
            omitted = payloads.len() - i;
            break;
        }
        out.push_str(separator);
        out.push_str(&payload.summary_md);
        used += block_len;
    }

    if omitted > 0 {
        use std::fmt::Write;
        let _ = write!(
            out,
            "\n---\n*{omitted} additional update(s) omitted to preserve your context budget. \
             See `.phantom-trunk-update.md` in your overlay for the full list.*\n",
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;
    use phantom_core::id::{ChangesetId, GitOid};
    use phantom_core::notification::{TrunkFileStatus, TrunkNotification};

    use super::*;

    fn payload(cs: &str, summary: &str) -> pending_notifications::PendingNotification {
        pending_notifications::PendingNotification {
            changeset_id: ChangesetId(cs.into()),
            submitting_agent: AgentId("agent-a".into()),
            notification: TrunkNotification {
                new_commit: GitOid::zero(),
                timestamp: Utc::now(),
                files: vec![(PathBuf::from("src/lib.rs"), TrunkFileStatus::TrunkVisible)],
                dependency_impacts: vec![],
            },
            summary_md: summary.into(),
        }
    }

    #[test]
    fn empty_queue_prints_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());
        let mut buf = Vec::new();
        emit_hook_output(tmp.path(), &agent_id, "UserPromptSubmit", &mut buf).unwrap();
        assert!(buf.is_empty(), "empty queue must produce empty stdout");
    }

    #[test]
    fn queued_notifications_emitted_and_marked_consumed() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());

        pending_notifications::write(tmp.path(), &agent_id, &payload("cs-1", "# One\n")).unwrap();
        pending_notifications::write(tmp.path(), &agent_id, &payload("cs-2", "# Two\n")).unwrap();

        let mut buf = Vec::new();
        emit_hook_output(tmp.path(), &agent_id, "PostToolUse", &mut buf).unwrap();

        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("\"hookEventName\":\"PostToolUse\""));
        assert!(out.contains("Phantom Trunk Updates"));
        assert!(out.contains("# One"));
        assert!(out.contains("# Two"));
        // Ends in newline so hook stdout parsers see a complete JSON line.
        assert!(out.ends_with('\n'));

        // Both consumed.
        let remaining = pending_notifications::list(tmp.path(), &agent_id).unwrap();
        assert!(remaining.is_empty(), "queue must be drained");
        let consumed = pending_notifications::consumed_dir(tmp.path(), &agent_id);
        assert!(consumed.join("cs-1.json").exists());
        assert!(consumed.join("cs-2.json").exists());
    }

    #[test]
    fn second_invocation_after_drain_is_silent() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());

        pending_notifications::write(tmp.path(), &agent_id, &payload("cs-1", "# One\n")).unwrap();
        let mut first = Vec::new();
        emit_hook_output(tmp.path(), &agent_id, "UserPromptSubmit", &mut first).unwrap();
        assert!(!first.is_empty());

        // Second invocation before any new notification arrives: no output.
        let mut second = Vec::new();
        emit_hook_output(tmp.path(), &agent_id, "UserPromptSubmit", &mut second).unwrap();
        assert!(
            second.is_empty(),
            "drain must be exactly-once; second hook call must be silent"
        );
    }

    #[test]
    fn oversized_payload_truncates_with_tail_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());

        // Write enough huge payloads to blow the budget.
        let big = "X".repeat(4000);
        for i in 0..4 {
            pending_notifications::write(tmp.path(), &agent_id, &payload(&format!("cs-{i}"), &big))
                .unwrap();
        }

        let mut buf = Vec::new();
        emit_hook_output(tmp.path(), &agent_id, "UserPromptSubmit", &mut buf).unwrap();

        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("omitted to preserve your context budget"));
        // All files still marked consumed — we do not leave the tail in the
        // queue to trickle out over subsequent hooks (that would desync the
        // user's view of trunk state).
        let remaining = pending_notifications::list(tmp.path(), &agent_id).unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn corrupt_entry_is_drained_not_delivered() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_id = AgentId("agent-b".into());
        let dir = pending_notifications::queue_dir(tmp.path(), &agent_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("corrupt.json"), "{not valid json").unwrap();

        let mut buf = Vec::new();
        emit_hook_output(tmp.path(), &agent_id, "UserPromptSubmit", &mut buf).unwrap();

        // Nothing valid to deliver: stdout empty, queue drained so we don't
        // loop forever on a bad entry.
        assert!(buf.is_empty());
        let remaining = pending_notifications::list(tmp.path(), &agent_id).unwrap();
        assert!(remaining.is_empty());
    }
}

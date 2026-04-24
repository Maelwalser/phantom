//! `phantom changes` — show recent submits and materializations.

use phantom_core::EventKind;
use phantom_core::id::AgentId;
use phantom_events::EventQuery;
use phantom_git::{GitOps, git_oid_to_oid};

use crate::context::PhantomContext;
use crate::ui;
use crate::ui::agent_color::AgentPalette;

#[derive(clap::Args)]
pub struct ChangesArgs {
    /// Agent/task name — show submits for this overlay
    pub agent: Option<String>,

    /// Maximum number of entries to show
    #[arg(long, short = 'n', default_value = "25")]
    pub limit: u64,
}

pub async fn run(args: ChangesArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events_store = ctx.open_events().await?;

    let query = if let Some(ref agent) = args.agent {
        let agent_id = AgentId::validate(agent)
            .map_err(|e| anyhow::anyhow!("invalid agent name '{agent}': {e}"))?;
        EventQuery {
            agent_id: Some(agent_id),
            limit: Some(args.limit),
            kind_prefixes: vec![
                "ChangesetMaterialized".to_string(),
                "ChangesetConflicted".to_string(),
            ],
            ..Default::default()
        }
    } else {
        EventQuery {
            limit: Some(args.limit),
            kind_prefixes: vec!["ChangesetMaterialized".to_string()],
            ..Default::default()
        }
    };

    let events = events_store.query(&query).await?;

    if events.is_empty() {
        if let Some(ref agent) = args.agent {
            ui::empty_state(
                &format!("No submits for agent '{agent}' yet."),
                Some("Use `phantom submit` after work completes."),
            );
        } else {
            ui::empty_state(
                "No submits yet.",
                Some("Use `phantom submit <agent>` after an agent completes work."),
            );
        }
        return Ok(());
    }

    let git = ctx.open_git().ok();

    let mut palette = AgentPalette::new();
    let width = ui::term_width();

    for event in &events {
        let ts = ui::dim_timestamp(event.timestamp);
        let agent_style = palette.style_for(&event.agent_id.0).clone();
        let agent = agent_style.apply_to(&event.agent_id.0);
        let (label, detail) = format_change(&event.kind, git.as_ref());
        // Trim the detail so the whole line fits within the terminal width.
        // Prefix is: "  " + 12-char timestamp + "  " + 14-char label + " " + agent + "  "
        let prefix_len = 2 + 12 + 2 + 14 + 1 + event.agent_id.0.len() + 2;
        let detail = ui::truncate_line(&detail, width.saturating_sub(prefix_len));
        println!("  {ts:>12}  {label:<14} {agent}  {detail}");
    }

    if let Some(ref agent) = args.agent {
        println!(
            "\n{}",
            ui::style_dim(&format!("{} submit(s) for '{agent}'", events.len()))
        );
    } else {
        println!(
            "\n{}",
            ui::style_dim(&format!("{} submit(s)", events.len()))
        );
    }

    Ok(())
}

/// Format a submit or conflict event into a styled label and detail string.
fn format_change(
    kind: &EventKind,
    git: Option<&GitOps>,
) -> (console::StyledObject<&'static str>, String) {
    match kind {
        EventKind::ChangesetMaterialized { new_commit } => {
            let message = git.and_then(|g| {
                let oid = git_oid_to_oid(new_commit).ok()?;
                let commit = g.repo().find_commit(oid).ok()?;
                commit.summary().map(String::from)
            });

            let detail = if let Some(msg) = message {
                msg
            } else {
                let hex = new_commit.to_hex();
                let short = &hex[..12.min(hex.len())];
                format!("commit {short}")
            };
            (console::style("SUBMITTED").green(), detail)
        }
        EventKind::ChangesetConflicted { conflicts } => {
            let count = conflicts.len();
            let summary = if count == 1 {
                "1 conflict".to_string()
            } else {
                format!("{count} conflicts")
            };
            (console::style("CONFLICTED").red(), summary)
        }
        _ => (console::style("UNKNOWN").dim(), String::new()),
    }
}

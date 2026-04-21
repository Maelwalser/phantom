//! `phantom conflicts` — read-only inspection of conflicted changesets.
//!
//! Lists every changeset currently in `Conflicted` or `Resolving` status. With
//! one match (or an explicit agent argument) the command renders a detail view
//! straight away; with several it presents a `dialoguer::Select` picker. The
//! detail view exposes everything the `ChangesetConflicted` event already
//! captured — file, kind, description, line spans, symbol id — plus the on-disk
//! paths a user needs to fix the conflict by hand.

use anyhow::Context;
use dialoguer::Select;
use phantom_core::changeset::Changeset;
use phantom_core::conflict::{ConflictDetail, ConflictKind};
use phantom_core::event::{Event, EventKind};
use phantom_core::id::ChangesetId;
use phantom_core::traits::EventStore;
use phantom_events::SnapshotManager;

use crate::context::PhantomContext;
use crate::ui;

#[derive(clap::Args)]
pub struct ConflictsArgs {
    /// Optional agent name — skip the menu and show this agent's conflicts directly.
    pub agent: Option<String>,
}

pub async fn run(args: ConflictsArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;

    let projection = SnapshotManager::new(&events).build_projection().await?;
    let all_events = events.query_all().await?;

    let chosen: Changeset = if let Some(name) = args.agent.as_deref() {
        let agent_id = crate::services::validate::agent_id(name)?;
        projection
            .latest_conflicted_changeset(&agent_id)
            .or_else(|| projection.latest_resolving_changeset(&agent_id))
            .cloned()
            .with_context(|| format!("no conflicted changeset found for agent '{name}'"))?
    } else {
        let conflicted = projection.conflicted_changesets();
        match conflicted.len() {
            0 => {
                ui::empty_state("No conflicts.", Some("Run `ph status` to see all agents."));
                return Ok(());
            }
            1 => conflicted[0].clone(),
            _ => {
                let Some(cs) = prompt_select(&conflicted)? else {
                    println!("  {}", ui::style_dim("Cancelled."));
                    return Ok(());
                };
                cs
            }
        }
    };

    let details = latest_conflict_details(&all_events, &chosen.id);
    render_detail(&ctx, &chosen, &details);
    Ok(())
}

/// Build the menu and let the user pick a conflicted changeset, or cancel.
fn prompt_select(conflicted: &[&Changeset]) -> anyhow::Result<Option<Changeset>> {
    let width = ui::term_width();
    let display_items: Vec<String> = conflicted
        .iter()
        .map(|cs| {
            let status_str = format!("{}", ui::status_label(cs.status));
            let status_width = console::measure_text_width(&status_str);
            let age = ui::relative_time(cs.created_at);

            let prefix_len = 16 + 2 + status_width + 2 + 2 + age.len() + 1;
            let task_max = width.saturating_sub(prefix_len).max(10);
            let task = if cs.task.is_empty() {
                String::new()
            } else {
                format!("  {}", ui::truncate_line(&cs.task, task_max))
            };

            format!("{:<16} {status_str}  ({age}){task}", cs.agent_id.0)
        })
        .collect();

    let selection = Select::new()
        .with_prompt("Select a conflicted agent")
        .items(&display_items)
        .default(0)
        .interact_opt()?;

    Ok(selection.map(|idx| conflicted[idx].clone()))
}

/// Pick the conflict payload from the most recent `ChangesetConflicted` event
/// for the given changeset. Returns an empty `Vec` if no such event exists.
fn latest_conflict_details(
    all_events: &[Event],
    changeset_id: &ChangesetId,
) -> Vec<ConflictDetail> {
    all_events
        .iter()
        .filter(|e| e.changeset_id == *changeset_id)
        .filter_map(|e| match &e.kind {
            EventKind::ChangesetConflicted { conflicts } => Some(conflicts.clone()),
            _ => None,
        })
        .next_back()
        .unwrap_or_default()
}

fn kind_label(kind: ConflictKind) -> &'static str {
    match kind {
        ConflictKind::BothModifiedSymbol => "both modified",
        ConflictKind::ModifyDeleteSymbol => "modify/delete",
        ConflictKind::BothModifiedDependencyVersion => "dependency version",
        ConflictKind::RawTextConflict => "text conflict",
        ConflictKind::BinaryFile => "binary file",
    }
}

fn render_detail(ctx: &PhantomContext, cs: &Changeset, details: &[ConflictDetail]) {
    let base_short = cs.base_commit.to_hex().chars().take(10).collect::<String>();
    let cs_short = console::style(cs.id.to_string()).dim();
    let agent_bold = console::style(&cs.agent_id.0).bold();
    let status = ui::status_label(cs.status);

    println!();
    println!(
        "  {} {}  {}  ({})",
        agent_bold,
        cs_short,
        status,
        ui::relative_time(cs.created_at),
    );
    ui::key_value("Base", &base_short);
    if !cs.task.is_empty() {
        ui::key_value("Task", &cs.task);
    }

    if details.is_empty() {
        println!();
        println!(
            "  {} no ChangesetConflicted event recorded for {}",
            console::style("·").dim(),
            cs.id
        );
        return;
    }

    println!();
    println!(
        "  {} {} conflict(s):",
        console::style("✗").red(),
        details.len()
    );

    for d in details {
        println!();
        println!(
            "    {} {} {}",
            console::style(d.file.display().to_string()).bold(),
            console::style(format!("[{}]", kind_label(d.kind))).red(),
            console::style(&d.description).dim(),
        );
        let mut span_parts: Vec<String> = Vec::new();
        if let Some(s) = &d.base_span {
            span_parts.push(format!("base L{}-{}", s.start_line, s.end_line));
        }
        if let Some(s) = &d.ours_span {
            span_parts.push(format!("ours L{}-{}", s.start_line, s.end_line));
        }
        if let Some(s) = &d.theirs_span {
            span_parts.push(format!("theirs L{}-{}", s.start_line, s.end_line));
        }
        if !span_parts.is_empty() {
            println!("      {}", ui::style_dim(&span_parts.join("  ")));
        }
        if let Some(sym) = &d.symbol_id {
            println!("      {}", ui::style_dim(&format!("symbol: {}", sym.0)));
        }
        println!(
            "      {}",
            ui::style_dim(&format!(
                "ours: {}  theirs: {}",
                d.ours_changeset, d.theirs_changeset
            ))
        );
    }

    let upper = ctx
        .phantom_dir
        .join("overlays")
        .join(&cs.agent_id.0)
        .join("upper");

    println!();
    println!("  {}", ui::style_dim("To fix manually:"));
    ui::key_value("Theirs", upper.join("<file>").display());
    ui::key_value("Ours", "git show HEAD:<file>");
    ui::key_value("Base", format!("git show {base_short}:<file>"));
    println!();
    println!(
        "  Or auto-resolve with {}.",
        console::style(format!("ph resolve {}", cs.agent_id.0)).bold()
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use phantom_core::id::{AgentId, EventId, GitOid};
    use std::path::PathBuf;

    fn conflict_with_desc(desc: &str) -> ConflictDetail {
        ConflictDetail {
            kind: ConflictKind::BothModifiedSymbol,
            file: PathBuf::from("src/lib.rs"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: desc.into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }
    }

    fn event(id: u64, cs: &str, kind: EventKind) -> Event {
        Event {
            id: EventId(id),
            timestamp: Utc::now() + Duration::seconds(id as i64),
            changeset_id: ChangesetId(cs.into()),
            agent_id: AgentId("agent-a".into()),
            causal_parent: None,
            kind,
        }
    }

    #[test]
    fn returns_latest_conflict_payload_when_multiple_present() {
        let cs = ChangesetId("cs-1".into());
        let events = vec![
            event(
                1,
                "cs-1",
                EventKind::ChangesetConflicted {
                    conflicts: vec![conflict_with_desc("first")],
                },
            ),
            event(
                2,
                "cs-other",
                EventKind::ChangesetConflicted {
                    conflicts: vec![conflict_with_desc("noise")],
                },
            ),
            event(
                3,
                "cs-1",
                EventKind::ChangesetConflicted {
                    conflicts: vec![
                        conflict_with_desc("latest-a"),
                        conflict_with_desc("latest-b"),
                    ],
                },
            ),
        ];

        let result = latest_conflict_details(&events, &cs);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].description, "latest-a");
        assert_eq!(result[1].description, "latest-b");
    }

    #[test]
    fn returns_empty_when_no_conflict_event_for_changeset() {
        let cs = ChangesetId("cs-missing".into());
        let events = vec![event(
            1,
            "cs-other",
            EventKind::ChangesetConflicted {
                conflicts: vec![conflict_with_desc("nope")],
            },
        )];
        assert!(latest_conflict_details(&events, &cs).is_empty());
    }

    #[test]
    fn ignores_non_conflicted_event_kinds() {
        let cs = ChangesetId("cs-1".into());
        let events = vec![
            event(
                1,
                "cs-1",
                EventKind::TaskCreated {
                    base_commit: GitOid::zero(),
                    task: String::new(),
                },
            ),
            event(2, "cs-1", EventKind::TaskDestroyed),
        ];
        assert!(latest_conflict_details(&events, &cs).is_empty());
    }
}

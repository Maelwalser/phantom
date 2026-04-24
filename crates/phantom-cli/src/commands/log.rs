//! `phantom log` — query the event log.

use chrono::Utc;
use phantom_core::id::{AgentId, ChangesetId, SymbolId};
use phantom_events::EventQuery;

use crate::context::PhantomContext;
use crate::ui;
use crate::ui::agent_color::AgentPalette;

#[derive(clap::Args)]
pub struct LogArgs {
    /// Agent name or changeset ID to filter by (auto-detected by "cs-" prefix)
    pub filter: Option<String>,
    /// Filter by symbol ID
    #[arg(long)]
    pub symbol: Option<String>,
    /// Only events since this duration (e.g. "2h", "1d", "30m")
    #[arg(long)]
    pub since: Option<String>,
    /// Maximum number of events to show
    #[arg(long, default_value = "10")]
    pub limit: u64,
    /// Show full event details (agent, event kind, payload)
    #[arg(short, long)]
    pub verbose: bool,
    /// Trace causal chain: show all events caused by the given event ID
    #[arg(long)]
    pub trace: Option<u64>,
}

pub async fn run(args: LogArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events_store = ctx.open_events().await?;

    // Trace mode: walk the causal DAG from a specific event.
    if let Some(root_id) = args.trace {
        let events = events_store
            .query_descendants(phantom_core::id::EventId(root_id))
            .await?;

        if events.is_empty() {
            ui::empty_state(&format!("No events found for trace root #{root_id}."), None);
            return Ok(());
        }

        let mut palette = AgentPalette::new();

        // Build depth map from causal_parent links for indentation.
        let mut depths: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
        depths.insert(root_id, 0);

        for event in &events {
            let depth = event
                .causal_parent
                .and_then(|p| depths.get(&p.0).copied())
                .map_or(0, |d| d + 1);
            depths.insert(event.id.0, depth);

            let indent = "  ".repeat(depth);
            let ts = ui::dim_timestamp(event.timestamp);
            let agent_style = palette.style_for(&event.agent_id.0).clone();
            let agent = agent_style.apply_to(&event.agent_id.0);
            let id_str = format!("#{}", event.id.0);
            let id = ui::style_dim(&id_str);

            if args.verbose {
                let kind_summary = format_event_kind(&event.kind);
                println!("{indent}  {id} {ts:>12}  {agent}  {kind_summary}");
            } else {
                let label = styled_event_kind_label(&event.kind);
                println!("{indent}  {id} {ts:>12}  {agent}  {label}");
            }
        }

        println!(
            "\n{}",
            ui::style_dim(&format!("{} event(s) in causal chain", events.len()))
        );
        return Ok(());
    }

    let since = args.since.as_deref().map(parse_duration_ago).transpose()?;

    // Auto-detect whether the positional filter is an agent or changeset.
    // Both values flow into SQL and log output — validate to reject crafted
    // names (path traversal, control bytes, etc.).
    let (agent_id, changeset_id) = match &args.filter {
        Some(f) if f.starts_with("cs-") => {
            let cs = ChangesetId::validate(f)
                .map_err(|e| anyhow::anyhow!("invalid changeset id '{f}': {e}"))?;
            (None, Some(cs))
        }
        Some(f) => {
            let agent = AgentId::validate(f)
                .map_err(|e| anyhow::anyhow!("invalid agent name '{f}': {e}"))?;
            (Some(agent), None)
        }
        None => (None, None),
    };

    let query = EventQuery {
        agent_id,
        changeset_id,
        symbol_id: args.symbol.map(SymbolId),
        since,
        limit: Some(args.limit),
        kind_prefixes: Vec::new(),
        order: phantom_events::QueryOrder::Desc,
    };

    let events = events_store.query(&query).await?;

    // In non-verbose mode, `ChangesetSubmitted` is redundant with the
    // `ChangesetMaterialized` / `ChangesetConflicted` event that always
    // follows a submit — collapse the pair to a single line per outcome.
    let events: Vec<_> = if args.verbose {
        events
    } else {
        events
            .into_iter()
            .filter(|e| !matches!(e.kind, phantom_core::EventKind::ChangesetSubmitted { .. }))
            .collect()
    };

    if events.is_empty() {
        ui::empty_state(
            "No events found.",
            Some("Use --since or --limit to broaden the search."),
        );
        return Ok(());
    }

    let total = events_store.count(&query).await?;

    let mut palette = AgentPalette::new();
    let width = ui::term_width();

    for event in &events {
        let ts = ui::dim_timestamp(event.timestamp);
        let agent_style = palette.style_for(&event.agent_id.0).clone();
        let agent = agent_style.apply_to(&event.agent_id.0);
        if args.verbose {
            let kind_summary = format_event_kind(&event.kind);
            let parent = event
                .causal_parent
                .map(|p| format!(" <- #{}", p.0))
                .unwrap_or_default();
            // Prefix: "  " + 12-char ts + "  " + agent + "  "
            let prefix_len = 2 + 12 + 2 + event.agent_id.0.len() + 2;
            let detail = format!("{kind_summary}{parent}");
            let detail = ui::truncate_line(&detail, width.saturating_sub(prefix_len));
            println!("  {ts:>12}  {agent}  {detail}");
        } else {
            let label = styled_event_kind_label(&event.kind);
            println!("  {ts:>12}  {agent}  {label}");
            // For conflict events, render a secondary line listing the
            // conflicting files so the user can diagnose without digging
            // into --verbose output.
            if let Some(detail) = conflict_detail_line(&event.kind) {
                println!("              {}", ui::style_dim(&detail));
            }
        }
    }

    let shown = events.len() as u64;
    if total > shown {
        let remaining = total - shown;
        println!(
            "\n{}",
            ui::style_dim(&format!(
                "Showing {shown} of {total} events ({remaining} more). Use --limit <n> to see more."
            ))
        );
    } else {
        println!("\n{}", ui::style_dim(&format!("{total} event(s)")));
    }

    Ok(())
}

/// Parse a human-friendly duration string like "2h", "30m", "1d" into a
/// `DateTime<Utc>` that many units ago from now.
fn parse_duration_ago(s: &str) -> anyhow::Result<chrono::DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration string");
    }

    let (num_str, unit) = s.split_at(s.len() - 1);
    let num: i64 = num_str.parse().map_err(|_| {
        anyhow::anyhow!("invalid duration: '{s}' (expected e.g. '2h', '30m', '1d')")
    })?;

    let duration = match unit {
        "s" => chrono::Duration::seconds(num),
        "m" => chrono::Duration::minutes(num),
        "h" => chrono::Duration::hours(num),
        "d" => chrono::Duration::days(num),
        _ => anyhow::bail!("unknown duration unit '{unit}' (use s, m, h, or d)"),
    };

    Ok(Utc::now() - duration)
}

/// Build a one-line "on foo.rs, bar.rs" fragment for a conflict list,
/// truncating past 3 files to keep output terse.
fn format_conflict_files(conflicts: &[phantom_core::ConflictDetail]) -> String {
    use std::collections::BTreeSet;
    let files: BTreeSet<&std::path::Path> = conflicts.iter().map(|c| c.file.as_path()).collect();
    if files.is_empty() {
        return String::new();
    }
    let mut names: Vec<String> = files
        .iter()
        .take(3)
        .map(|p| p.display().to_string())
        .collect();
    if files.len() > 3 {
        names.push(format!("+{} more", files.len() - 3));
    }
    names.join(", ")
}

/// Secondary-line detail shown under conflict events in compact log output.
/// Lists the file paths that conflicted so the user can diagnose without
/// running `ph log --verbose`.
fn conflict_detail_line(kind: &phantom_core::EventKind) -> Option<String> {
    use phantom_core::EventKind;
    match kind {
        EventKind::ChangesetConflicted { conflicts }
        | EventKind::ConflictResolutionStarted { conflicts, .. } => {
            let files = format_conflict_files(conflicts);
            if files.is_empty() {
                None
            } else {
                Some(format!("↳ {files}"))
            }
        }
        _ => None,
    }
}

/// Format an `EventKind` into a one-line summary.
fn format_event_kind(kind: &phantom_core::EventKind) -> String {
    use phantom_core::EventKind;
    match kind {
        EventKind::TaskCreated { base_commit, .. } => {
            format!(
                "TaskCreated {{ base: {} }}",
                short_hex(&base_commit.to_hex())
            )
        }
        EventKind::TaskDestroyed => "TaskDestroyed".into(),
        EventKind::FileWritten { path, .. } => {
            format!("FileWritten {{ {} }}", path.display())
        }
        EventKind::FileDeleted { path } => {
            format!("FileDeleted {{ {} }}", path.display())
        }
        EventKind::ChangesetSubmitted { operations } => {
            format!("submitted {{ {} op(s) }}", operations.len())
        }
        EventKind::ChangesetMergeChecked { result } => {
            let status = match result {
                phantom_core::MergeCheckResult::Clean => "clean",
                phantom_core::MergeCheckResult::Conflicted(_) => "conflicted",
            };
            format!("merge-checked {{ {status} }}")
        }
        EventKind::ChangesetMaterializationStarted { parent, path } => {
            format!(
                "materialization-started {{ parent: {}, path: {path:?} }}",
                short_hex(&parent.to_hex())
            )
        }
        EventKind::ChangesetMaterialized { new_commit } => {
            format!(
                "materialized {{ commit: {} }}",
                short_hex(&new_commit.to_hex())
            )
        }
        EventKind::ChangesetConflicted { conflicts } => {
            let files = format_conflict_files(conflicts);
            if files.is_empty() {
                format!("conflicted {{ {} conflict(s) }}", conflicts.len())
            } else {
                format!(
                    "conflicted {{ {} conflict(s) on {files} }}",
                    conflicts.len()
                )
            }
        }
        EventKind::ChangesetDropped { reason } => {
            format!("dropped {{ {reason} }}")
        }
        EventKind::TrunkAdvanced { new_commit, .. } => {
            format!(
                "TrunkAdvanced {{ to: {} }}",
                short_hex(&new_commit.to_hex())
            )
        }
        EventKind::AgentNotified {
            agent_id,
            changed_symbols,
        } => {
            format!(
                "AgentNotified {{ {agent_id}, {} symbol(s) }}",
                changed_symbols.len()
            )
        }
        EventKind::TestsRun(result) => {
            format!(
                "TestsRun {{ passed: {}, failed: {}, skipped: {} }}",
                result.passed, result.failed, result.skipped
            )
        }
        EventKind::LiveRebased {
            new_base,
            merged_files,
            conflicted_files,
            ..
        } => {
            format!(
                "LiveRebased {{ to: {}, {} merged, {} conflicted }}",
                short_hex(&new_base.to_hex()),
                merged_files.len(),
                conflicted_files.len()
            )
        }
        EventKind::ConflictResolutionStarted { conflicts, .. } => {
            format!(
                "ConflictResolutionStarted {{ {} conflict(s) }}",
                conflicts.len()
            )
        }
        EventKind::AgentLaunched { pid, task } => {
            format!("AgentLaunched {{ pid: {pid}, task: {task:?} }}")
        }
        EventKind::AgentCompleted {
            exit_code,
            materialized,
        } => {
            let code = exit_code.map_or_else(|| "signal".into(), |c| c.to_string());
            format!("AgentCompleted {{ exit: {code}, materialized: {materialized} }}")
        }
        EventKind::PlanCreated {
            plan_id,
            domain_count,
            ..
        } => {
            format!("PlanCreated {{ {plan_id}, {domain_count} domain(s) }}")
        }
        EventKind::PlanCompleted {
            plan_id,
            succeeded,
            failed,
        } => {
            format!("PlanCompleted {{ {plan_id}, {succeeded} ok, {failed} failed }}")
        }
        EventKind::AgentWaitingForDependencies { upstream_agents } => {
            let names: Vec<&str> = upstream_agents.iter().map(|a| a.0.as_str()).collect();
            format!(
                "AgentWaitingForDependencies {{ waiting on: {} }}",
                names.join(", ")
            )
        }
        EventKind::Unknown => "Unknown".into(),
    }
}

/// Short human-readable label for the default (non-verbose) output.
fn event_kind_label(kind: &phantom_core::EventKind) -> &'static str {
    use phantom_core::EventKind;
    match kind {
        EventKind::TaskCreated { .. } => "task created",
        EventKind::TaskDestroyed => "task destroyed",
        EventKind::FileWritten { .. } => "file written",
        EventKind::FileDeleted { .. } => "file deleted",
        EventKind::ChangesetSubmitted { .. } | EventKind::ChangesetMaterialized { .. } => {
            "submitted"
        }
        EventKind::ChangesetMaterializationStarted { .. } => "materializing",
        EventKind::ChangesetMergeChecked { .. } => "merge checked",
        EventKind::ChangesetConflicted { .. } => "conflicted",
        EventKind::ChangesetDropped { .. } => "dropped",
        EventKind::TrunkAdvanced { .. } => "trunk advanced",
        EventKind::AgentNotified { .. } => "agent notified",
        EventKind::TestsRun(_) => "tests run",
        EventKind::LiveRebased { .. } => "live rebased",
        EventKind::ConflictResolutionStarted { .. } => "resolving",
        EventKind::AgentLaunched { .. } => "agent launched",
        EventKind::AgentCompleted { .. } => "agent completed",
        EventKind::PlanCreated { .. } => "plan created",
        EventKind::PlanCompleted { .. } => "plan completed",
        EventKind::AgentWaitingForDependencies { .. } => "waiting for deps",
        EventKind::Unknown => "unknown",
    }
}

/// Styled label with semantic colors for event kinds.
fn styled_event_kind_label(kind: &phantom_core::EventKind) -> console::StyledObject<&'static str> {
    use phantom_core::EventKind;
    let label = event_kind_label(kind);
    match kind {
        EventKind::ChangesetMaterialized { .. } | EventKind::PlanCompleted { .. } => {
            console::style(label).green()
        }
        EventKind::ChangesetSubmitted { .. } => console::style(label).yellow(),
        EventKind::ChangesetConflicted { .. } => console::style(label).red(),
        EventKind::ChangesetDropped { .. } => console::style(label).red().dim(),
        EventKind::TaskCreated { .. }
        | EventKind::AgentLaunched { .. }
        | EventKind::PlanCreated { .. } => console::style(label).cyan(),
        EventKind::LiveRebased { .. } | EventKind::ConflictResolutionStarted { .. } => {
            console::style(label).cyan()
        }
        _ => console::style(label).dim(),
    }
}

fn short_hex(hex: &str) -> &str {
    &hex[..12.min(hex.len())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::{ConflictDetail, ConflictKind, EventKind};
    use std::path::PathBuf;

    fn detail(file: &str) -> ConflictDetail {
        ConflictDetail {
            kind: ConflictKind::BothModifiedSymbol,
            file: PathBuf::from(file),
            symbol_id: None,
            ours_changeset: phantom_core::ChangesetId("cs-a".into()),
            theirs_changeset: phantom_core::ChangesetId("cs-b".into()),
            description: String::new(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }
    }

    #[test]
    fn format_conflict_files_empty_returns_empty() {
        assert!(format_conflict_files(&[]).is_empty());
    }

    #[test]
    fn format_conflict_files_lists_paths() {
        let got = format_conflict_files(&[detail("src/a.rs"), detail("src/b.rs")]);
        assert_eq!(got, "src/a.rs, src/b.rs");
    }

    #[test]
    fn format_conflict_files_dedupes_same_file() {
        // Two symbol conflicts in the same file should collapse to one path.
        let got = format_conflict_files(&[detail("src/a.rs"), detail("src/a.rs")]);
        assert_eq!(got, "src/a.rs");
    }

    #[test]
    fn format_conflict_files_truncates_past_three() {
        let got = format_conflict_files(&[
            detail("a.rs"),
            detail("b.rs"),
            detail("c.rs"),
            detail("d.rs"),
            detail("e.rs"),
        ]);
        assert_eq!(got, "a.rs, b.rs, c.rs, +2 more");
    }

    #[test]
    fn conflict_detail_line_none_for_non_conflict_events() {
        assert!(conflict_detail_line(&EventKind::TaskDestroyed).is_none());
    }

    #[test]
    fn conflict_detail_line_renders_for_conflicted() {
        let line = conflict_detail_line(&EventKind::ChangesetConflicted {
            conflicts: vec![detail("src/main.rs")],
        });
        assert_eq!(line.as_deref(), Some("↳ src/main.rs"));
    }

    #[test]
    fn conflict_detail_line_renders_for_resolution_started() {
        let line = conflict_detail_line(&EventKind::ConflictResolutionStarted {
            conflicts: vec![detail("src/lib.rs")],
            new_base: None,
        });
        assert_eq!(line.as_deref(), Some("↳ src/lib.rs"));
    }

    #[test]
    fn conflict_detail_line_empty_conflicts_returns_none() {
        // An older event stored with no details should not render a
        // misleading empty fragment.
        assert!(
            conflict_detail_line(&EventKind::ChangesetConflicted { conflicts: vec![] }).is_none(),
        );
    }
}

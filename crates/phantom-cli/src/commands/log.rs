//! `phantom log` — query the event log.

use chrono::Utc;
use phantom_core::id::{AgentId, ChangesetId, SymbolId};
use phantom_events::EventQuery;

use super::agent_color::AgentPalette;
use super::ui;
use crate::context::PhantomContext;

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
    #[arg(long, default_value = "50")]
    pub limit: u64,
    /// Show full event details (agent, event kind, payload)
    #[arg(short, long)]
    pub verbose: bool,
}

pub async fn run(args: LogArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events_store = ctx.open_events().await?;

    let since = args.since.as_deref().map(parse_duration_ago).transpose()?;

    // Auto-detect whether the positional filter is an agent or changeset.
    let (agent_id, changeset_id) = match &args.filter {
        Some(f) if f.starts_with("cs-") => (None, Some(ChangesetId(f.clone()))),
        Some(f) => (Some(AgentId(f.clone())), None),
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

    if events.is_empty() {
        println!("No events found.");
        return Ok(());
    }

    let mut palette = AgentPalette::new();

    for event in &events {
        let ts = ui::dim_timestamp(event.timestamp);
        let agent_style = palette.style_for(&event.agent_id.0).clone();
        let agent = agent_style.apply_to(&event.agent_id.0);
        if args.verbose {
            let kind_summary = format_event_kind(&event.kind);
            println!("  {ts:>12}  {agent}  {kind_summary}");
        } else {
            let label = styled_event_kind_label(&event.kind);
            println!("  {ts:>12}  {agent}  {label}");
        }
    }

    println!("\n{}", ui::style_dim(&format!("{} event(s)", events.len())));

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
            format!("ChangesetSubmitted {{ {} op(s) }}", operations.len())
        }
        EventKind::ChangesetMergeChecked { result } => {
            let status = match result {
                phantom_core::MergeCheckResult::Clean => "clean",
                phantom_core::MergeCheckResult::Conflicted(_) => "conflicted",
            };
            format!("ChangesetMergeChecked {{ {status} }}")
        }
        EventKind::ChangesetMaterialized { new_commit } => {
            format!(
                "ChangesetMaterialized {{ commit: {} }}",
                short_hex(&new_commit.to_hex())
            )
        }
        EventKind::ChangesetConflicted { conflicts } => {
            format!("ChangesetConflicted {{ {} conflict(s) }}", conflicts.len())
        }
        EventKind::ChangesetDropped { reason } => {
            format!("ChangesetDropped {{ {reason} }}")
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
            let code = exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into());
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
        EventKind::ChangesetSubmitted { .. } => "submitted",
        EventKind::ChangesetMergeChecked { .. } => "merge checked",
        EventKind::ChangesetMaterialized { .. } => "submitted",
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

//! `phantom log` — query the event log.

use chrono::Utc;
use phantom_core::id::{AgentId, ChangesetId, SymbolId};
use phantom_events::EventQuery;

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
    };

    let events = events_store
        .query(&query)
        .await
        ?;

    if events.is_empty() {
        println!("No events found.");
        return Ok(());
    }

    for event in &events {
        let ts = event.timestamp.format("%Y-%m-%d %H:%M:%S");
        let kind_summary = format_event_kind(&event.kind);
        println!(
            "[{ts}] {} {} {kind_summary}",
            event.changeset_id, event.agent_id
        );
    }

    println!("\n{} event(s) shown.", events.len());

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
        EventKind::ConflictResolutionStarted { conflicts } => {
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
        EventKind::Unknown => "Unknown".into(),
    }
}

fn short_hex(hex: &str) -> &str {
    &hex[..12.min(hex.len())]
}

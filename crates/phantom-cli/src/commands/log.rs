//! `phantom log` — query the event log.

use chrono::Utc;
use phantom_core::id::{AgentId, ChangesetId, SymbolId};
use phantom_events::EventQuery;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct LogArgs {
    /// Filter by agent ID
    #[arg(long)]
    pub agent: Option<String>,
    /// Filter by changeset ID
    #[arg(long)]
    pub changeset: Option<String>,
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
    let ctx = PhantomContext::load()?;

    let since = args.since.as_deref().map(parse_duration_ago).transpose()?;

    let query = EventQuery {
        agent_id: args.agent.map(AgentId),
        changeset_id: args.changeset.map(ChangesetId),
        symbol_id: args.symbol.map(SymbolId),
        since,
        limit: Some(args.limit),
    };

    let events = ctx
        .events
        .query(&query)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

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
        EventKind::OverlayCreated { base_commit, .. } => {
            format!(
                "OverlayCreated {{ base: {} }}",
                short_hex(&base_commit.to_hex())
            )
        }
        EventKind::OverlayDestroyed => "OverlayDestroyed".into(),
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
        EventKind::InteractiveSessionStarted { command, pid } => {
            format!("InteractiveSessionStarted {{ {command}, pid: {pid} }}")
        }
        EventKind::InteractiveSessionEnded {
            exit_code,
            duration_secs,
        } => {
            let code = exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into());
            format!("InteractiveSessionEnded {{ exit: {code}, {duration_secs}s }}")
        }
    }
}

fn short_hex(hex: &str) -> &str {
    &hex[..12.min(hex.len())]
}

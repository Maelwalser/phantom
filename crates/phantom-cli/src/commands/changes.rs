//! `phantom changes` — show recent submits and materializations.

use phantom_core::id::AgentId;
use phantom_core::EventKind;
use phantom_events::EventQuery;

use crate::context::PhantomContext;

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
        EventQuery {
            agent_id: Some(AgentId(agent.clone())),
            limit: Some(args.limit),
            kind_prefixes: vec!["ChangesetSubmitted".to_string()],
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
            println!("No submits for agent '{agent}' yet.");
        } else {
            println!("No materializations yet.");
        }
        return Ok(());
    }

    for event in &events {
        let ts = event.timestamp.format("%Y-%m-%d %H:%M:%S");
        let (label, detail) = format_change(&event.kind);
        println!(
            "  {ts}  {label:<14} {}  {}  {detail}",
            event.changeset_id, event.agent_id
        );
    }

    if let Some(ref agent) = args.agent {
        println!("\n{} submit(s) for '{agent}' shown.", events.len());
    } else {
        println!("\n{} materialization(s) shown.", events.len());
    }

    Ok(())
}

/// Format a submit or materialization event into a label and detail string.
fn format_change(kind: &EventKind) -> (&'static str, String) {
    match kind {
        EventKind::ChangesetSubmitted { operations } => {
            let count = operations.len();
            let summary = if count == 1 {
                "1 operation".to_string()
            } else {
                format!("{count} operations")
            };
            ("SUBMITTED", summary)
        }
        EventKind::ChangesetMaterialized { new_commit } => {
            let hex = new_commit.to_hex();
            let short = &hex[..12.min(hex.len())];
            ("MATERIALIZED", format!("commit {short}"))
        }
        _ => ("UNKNOWN", String::new()),
    }
}

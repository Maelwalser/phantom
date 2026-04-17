//! Row parsing: [`sqlx::sqlite::SqliteRow`] → [`Event`].

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};

use crate::error::EventStoreError;

/// Parse a single SQLite row into an [`Event`].
///
/// Expects columns: `id`, `timestamp`, `changeset_id`, `agent_id`, `kind`.
/// Unrecognized `EventKind` variants — whether unit variants (caught by
/// `#[serde(other)]`) or data-carrying variants from newer schema versions
/// (caught by the fallback match) — are returned as [`EventKind::Unknown`]
/// instead of propagating an error, ensuring forward compatibility.
pub(crate) fn row_to_event(row: &SqliteRow) -> Result<Event, EventStoreError> {
    let id: i64 = row.get("id");
    let ts_str: String = row.get("timestamp");
    let changeset_id: String = row.get("changeset_id");
    let agent_id: String = row.get("agent_id");
    let kind_json: String = row.get("kind");

    let timestamp = DateTime::parse_from_rfc3339(&ts_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| EventStoreError::InvalidTimestamp(ts_str, e.to_string()))?;
    let kind: EventKind = match serde_json::from_str(&kind_json) {
        Ok(k) => k,
        Err(e) => {
            tracing::debug!(
                event_id = id,
                kind_json,
                error = %e,
                "unrecognized EventKind, falling back to Unknown"
            );
            EventKind::Unknown
        }
    };

    let causal_parent: Option<i64> = row.try_get("causal_parent").unwrap_or(None);

    Ok(Event {
        id: EventId(id as u64),
        timestamp,
        changeset_id: ChangesetId(changeset_id),
        agent_id: AgentId(agent_id),
        causal_parent: causal_parent.map(|v| EventId(v as u64)),
        kind,
    })
}

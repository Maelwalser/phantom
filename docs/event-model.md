# Event Model

Phantom is event-sourced. Every state-changing action — overlay creation,
file write, submit, materialization, rollback, live rebase, ripple —
produces an immutable record in an append-only SQLite log. This document
describes the event shape, the store schema, the causal DAG, the
projection engine, and the rollback/replay mechanics.

## Why event sourcing?

Phantom coordinates multiple AI agents against a shared git repo. A
flat "latest state" representation is not enough:

- **Audit** — which agent wrote which bytes, when, based on which trunk
  commit?
- **Rollback** — remove a bad changeset *and* anything that causally
  depended on it without losing unrelated work.
- **Forward compatibility** — a newer binary can introduce new event
  kinds without breaking older binaries reading the same database.
- **Replay / what-if** — rebuild projections from scratch to validate
  assumptions or bisect bugs.

## `EventStore` trait

Defined in `phantom-core` and implemented by `phantom-events`:

```rust
#[async_trait::async_trait]
pub trait EventStore: Send + Sync {
    async fn append(&self, event: Event) -> Result<EventId, CoreError>;
    async fn query_by_changeset(&self, id: &ChangesetId) -> Result<Vec<Event>, CoreError>;
    async fn query_by_agent(&self, id: &AgentId) -> Result<Vec<Event>, CoreError>;
    async fn query_all(&self) -> Result<Vec<Event>, CoreError>;
    async fn query_since(&self, since: DateTime<Utc>) -> Result<Vec<Event>, CoreError>;
    async fn latest_event_for_changeset(
        &self,
        id: &ChangesetId,
    ) -> Result<Option<EventId>, CoreError>;
}
```

The production implementation (`SqliteEventStore`) opens a WAL-mode
SQLite database at `.phantom/events.db`. An in-memory variant is used
for unit tests.

## `Event` structure

```rust
pub struct Event {
    pub id: EventId,                       // auto-increment from SQLite
    pub timestamp: DateTime<Utc>,
    pub changeset_id: ChangesetId,
    pub agent_id: AgentId,
    pub causal_parent: Option<EventId>,    // turns the log into a DAG
    pub kind: EventKind,
}
```

`causal_parent` is the quiet but critical field. It is `None` only for
root events (`TaskCreated`, `PlanCreated`). Every other event records
the `EventId` that directly caused it:

- Lifecycle events within a changeset point to the previous event in
  that changeset (e.g. `ChangesetSubmitted.parent = TaskCreated.id`).
- Cross-changeset ripple events (`LiveRebased`, `AgentNotified`) point
  to the `ChangesetMaterialized` event on the *other* changeset that
  triggered them.

The flat insertion-ordered log is therefore also a causal DAG, which is
what makes surgical rollback safe.

## `EventKind` — the variants

All variants live in `crates/phantom-core/src/event.rs`.

### Lifecycle events (per-changeset)

| Variant | Meaning | Causal parent |
|---------|---------|---------------|
| `TaskCreated { base_commit, task }` | Agent overlay provisioned. | `None` (root) |
| `TaskDestroyed` | Agent overlay torn down. | `TaskCreated` |
| `FileWritten { path, content_hash }` | Agent wrote a file inside its overlay. | `TaskCreated` |
| `FileDeleted { path }` | Agent deleted a file inside its overlay. | `TaskCreated` |
| `ChangesetSubmitted { operations }` | Agent called `ph submit`; semantic operations have been extracted. | `TaskCreated` |
| `ChangesetMergeChecked { result }` | Explicit merge-check result (Clean / Conflicted). | `ChangesetSubmitted` |
| `ChangesetMaterialized { new_commit }` | Changeset committed to trunk. | `ChangesetSubmitted` |
| `ChangesetConflicted { conflicts }` | Semantic conflicts detected; changeset abandoned. | `ChangesetSubmitted` |
| `ConflictResolutionStarted { conflicts, new_base }` | `ph resolve` launched an AI agent on the conflicted changeset. | `ChangesetConflicted` |
| `ChangesetDropped { reason }` | Changeset rolled back. | Previous terminal event |
| `TestsRun(TestResult)` | Test results attached. | Previous event |

### Background agent lifecycle

| Variant | Meaning |
|---------|---------|
| `AgentLaunched { pid, task }` | Background monitor spawned. |
| `AgentCompleted { exit_code, materialized }` | Background agent exited (possibly auto-materialized). |

### Cross-changeset events (ripple effects)

| Variant | Meaning | Causal parent |
|---------|---------|---------------|
| `TrunkAdvanced { old_commit, new_commit }` | Trunk advanced due to a materialization. | `ChangesetMaterialized` |
| `LiveRebased { old_base, new_base, merged_files, conflicted_files }` | An agent's upper layer was three-way-merged after trunk advanced. | Triggering `ChangesetMaterialized` on the *other* changeset |
| `AgentNotified { agent_id, changed_symbols }` | An agent was notified that symbols in its working set changed. | Same as above |

### Plan orchestration

| Variant | Meaning |
|---------|---------|
| `PlanCreated { plan_id, request, domain_count, agent_ids }` | `ph plan` decomposed a request and dispatched agents. |
| `AgentWaitingForDependencies { upstream_agents }` | A planned agent is waiting for its upstream agents to finish. |
| `PlanCompleted { plan_id, succeeded, failed }` | All agents in the plan finished. |

### Forward-compatibility fallback

| Variant | Meaning |
|---------|---------|
| `Unknown` | An event kind this binary does not recognize. Preserved in the log but skipped during projection and replay. |

Unit variants (e.g. `"SomeFutureVariant"`) are caught by
`#[serde(other)]` on `EventKind`. Data-carrying unknown variants
(`{"NewFeatureEvent":{...}}`) fail `serde_json` deserialization; the
store row-reader catches the error and substitutes `EventKind::Unknown`
so a replay never crashes.

## SQLite schema

The live schema is built by `ddl.rs` + `migrations.rs` in
`phantom-events`. The current schema version is **5**.

### v1 baseline (DDL)

```sql
CREATE TABLE schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp    TEXT NOT NULL,
    changeset_id TEXT NOT NULL,
    agent_id     TEXT NOT NULL,
    kind         TEXT NOT NULL,          -- JSON-encoded EventKind
    dropped      INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_events_changeset ON events(changeset_id);
CREATE INDEX idx_events_agent     ON events(agent_id);
CREATE INDEX idx_events_timestamp ON events(timestamp);
```

### Migrations

Forward-only, contiguous starting from version 2. The
`CURRENT_SCHEMA_VERSION` constant and the `MIGRATIONS` slice stay in
lockstep (enforced by a unit test).

| From → To | Name | What it adds |
|-----------|------|--------------|
| 1 → 2 | `add_kind_version_column` | `kind_version INTEGER NOT NULL DEFAULT 1` on `events` for envelope versioning of the kind JSON. |
| 2 → 3 | `create_projection_snapshots` | New `projection_snapshots(id, snapshot_at, data, created_at)` table so projections can load from a snapshot instead of replaying every event. |
| 3 → 4 | `add_causal_parent_column` | Nullable `causal_parent INTEGER` on `events` plus `idx_events_causal_parent`. Turns the flat log into a causal DAG. |
| 4 → 5 | `add_composite_indexes` | `idx_events_dropped_changeset`, `idx_events_dropped_agent`, `idx_events_dropped_timestamp`. Leading with `dropped` lets SQLite skip dropped rows via the index rather than scanning. |

Migrations are idempotent — a crash mid-ALTER is safe because
`add_column_if_missing` treats "duplicate column" errors as success.

### Projection snapshots

`projection_snapshots` stores a serialized
`HashMap<ChangesetId, Changeset>` alongside the `event.id` at which it
was taken. `Projection::from_snapshot(base, tail)` deserializes the
snapshot and replays only events with `id > snapshot_at`, avoiding a
full-log replay on every CLI invocation.

## Projection

`phantom_events::Projection` derives the current state of all
changesets from an event stream:

```
TaskCreated          → new Changeset{ status = InProgress }
FileWritten          → update files_touched
FileDeleted          → update files_touched
ChangesetSubmitted   → status = Submitted, operations stored
ChangesetMaterialized → status stays Submitted (merge succeeded)
ChangesetConflicted  → status = Conflicted
ChangesetDropped     → status = Dropped
TestsRun             → test_result updated
AgentLaunched        → agent_pid set, agent_launched_at set
AgentCompleted       → agent_completed_at set, agent_exit_code set
```

Events with `EventKind::Unknown` or `dropped = 1` are silently skipped.

### Changeset status transitions

Enforced by `ChangesetStatus::can_transition_to` in `phantom-core`:

```
InProgress ──► Submitted
            ├─► Conflicted
            └─► Dropped

Conflicted ──► Resolving
           └─► Dropped

Resolving  ──► Submitted
           ├─► Conflicted
           └─► Dropped

Submitted  ──► Dropped           (only via rollback)

Dropped    ── terminal (no outgoing transitions)
```

`try_transition_to` returns `CoreError::InvalidStatusTransition` for any
move not in this table.

## Causal DAG and rollback

Because every non-root event records a `causal_parent`, the event log is
a DAG rooted at `TaskCreated` / `PlanCreated` events.

Rolling back a changeset walks the DAG:

1. `ReplayEngine::materialized_changesets()` lists every
   `ChangesetMaterialized` event in insertion order.
2. `ReplayEngine::changesets_after(id)` returns every materialized
   changeset with a higher `event.id` than the target's materialization.
3. The CLI uses these to mark the target's events as `dropped = 1`,
   emit a `ChangesetDropped` event, and create a `git revert` commit
   so trunk reflects the new reality.
4. Downstream changesets (materialized *after* the target) are flagged
   for the operator to review — they are not auto-rolled-back, because
   their work may still be desired.

## Query patterns

The common queries powering the CLI:

| CLI command | Query pattern |
|-------------|---------------|
| `ph log` (no filter) | `query_since(since)` with an optional time window |
| `ph log <agent>` | `query_by_agent(agent_id)` |
| `ph log <cs-id>` | `query_by_changeset(changeset_id)` |
| `ph log --trace <event-id>` | Walk `causal_parent` back to the root, and children forward |
| `ph status` | `query_all()` → `Projection::from_events()` → iterate changesets |
| `ph changes` | `kind LIKE 'ChangesetSubmitted%'` or `'ChangesetMaterialized%'` with `dropped = 0` |
| `ph rollback <cs-id>` | `ReplayEngine::changesets_after` + `UPDATE events SET dropped = 1 …` |

For prefix kind matches the code uses dedicated helpers in the
`kind_pattern` module (e.g. `materialized_prefix()` returns
`'{"ChangesetMaterialized":%'`).

## Appending: what happens on every write

```
┌──────────────────────────────┐
│ orchestrator / CLI builds    │
│ Event { timestamp, ids, kind,│
│         causal_parent }      │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│ SqliteEventStore::append     │
│ INSERT INTO events           │
│  (timestamp, changeset_id,   │
│   agent_id, kind,            │
│   kind_version, dropped = 0, │
│   causal_parent)             │
│ VALUES (...)                 │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│ returns EventId(new row id)  │
└──────────────────────────────┘
```

The WAL mode keeps writes fast and survives crashes cleanly. The
`dropped` column is only ever set to `1` by rollback; it is never
deleted, so historical state is always reconstructible.

## Guarantees and non-guarantees

### Guarantees

- **Append-only** — no event is ever mutated or physically deleted.
- **Total order** — `id` is strictly increasing within a single
  database. Two concurrent submits serialize via the orchestrator's
  cross-process lock before appending.
- **Forward compatibility** — unknown kinds deserialize to `Unknown`.
- **Causal traceability** — every non-root event has a
  `causal_parent`, enabling DAG traversal.

### Non-guarantees

- **Event kinds are not a stable wire format** for long-term archival.
  The recommendation is to snapshot to a higher-level format if you
  want out-of-tree consumers.
- **No cross-repo global ordering** — each `.phantom/events.db` is
  local to its repo.
- **No compaction** — the log grows monotonically. Projection
  snapshots mitigate read cost but do not shrink the log.

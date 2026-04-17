# Architecture

This document describes how the Phantom workspace is structured, how data
flows between crates, and why each boundary was drawn where it is.

## One-line summary

Phantom is a semantic-aware, event-sourced version control layer for
agentic AI development. Each agent gets an isolated FUSE overlay on top of
the git working tree; completed work is merged back via symbol-level
three-way merge; every lifecycle step is recorded as an immutable event.

## High-level data flow

```
agent CLI (Claude Code, etc.)
        │
        │  read/write through FUSE mount point
        ▼
  FUSE filesystem  ────────────►  OverlayLayer (COW: upper + lower)
 (phantom-overlay)                upper = agent writes
                                  lower = trunk working tree
        │
        │  ph submit
        ▼
  Submit service  ──────────►  SemanticAnalyzer (tree-sitter)
(phantom-orchestrator)          │
        │                       └─►  SemanticOperation list
        │
        │  materialize
        ▼
  Materializer  ────────────►  Three-way merge per file
                                │
                                ├─► Clean → git commit
                                └─► Conflict → ChangesetConflicted
        │
        │  ripple + live rebase
        ▼
  Other agents' upper layers are updated in place for shadowed files,
  trunk notifications are dropped in `.phantom/overlays/<agent>/trunk-notifications/`
        │
        │  append event on every step
        ▼
  SqliteEventStore (phantom-events)
  WAL-mode SQLite at `.phantom/events.db`
```

## Workspace layout

Nine production crates plus one integration-test crate, all in a single
Cargo workspace (`edition = "2024"`, `rust-version = "1.85"`).

```
crates/
├── phantom-core/           # Types, traits, errors. Zero phantom deps.
├── phantom-git/            # git2 wrapper + tree building + text merge.
├── phantom-events/         # SQLite WAL event store, projection, replay.
├── phantom-overlay/        # FUSE + copy-on-write layer (Linux-only FUSE).
├── phantom-semantic/       # tree-sitter parsing + symbol diff + semantic merge.
├── phantom-orchestrator/   # Submit, materialize, ripple, live rebase.
├── phantom-session/        # PTY, CLI adapters, context files, post-session automation.
├── phantom-cli/            # The `ph` binary.
└── phantom-testkit/        # Shared test builders, mocks, and fixtures.

tests/integration/          # End-to-end tests against real git repos.
```

### Crate dependency graph

```
                       ┌──────────────┐
                       │ phantom-core │  (no phantom deps)
                       └──────┬───────┘
                ┌─────────────┼─────────────┬─────────────┐
                ▼             ▼             ▼             ▼
         ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌───────────┐
         │phantom-  │  │phantom-  │  │phantom-  │  │phantom-   │
         │git       │  │events    │  │overlay   │  │semantic   │
         └─────┬────┘  └─────┬────┘  └─────┬────┘  └──────┬────┘
               │             │             │              │
               └─────────────┴──────┬──────┴──────────────┘
                                    ▼
                         ┌───────────────────────┐
                         │ phantom-orchestrator  │
                         └───────────┬───────────┘
                                     │
                                     ▼
                         ┌───────────────────────┐
                         │ phantom-session       │
                         └───────────┬───────────┘
                                     │
                                     ▼
                         ┌───────────────────────┐
                         │ phantom-cli (bin `ph`)│
                         └───────────────────────┘
```

All dependency arrows point inward toward `phantom-core`. Breaking this
rule is a review-blocker — it is the single invariant that keeps the core
types reusable and testable in isolation.

## Crate responsibilities

### `phantom-core`

The domain model. Every other crate in the workspace depends on this one,
and it depends on none of them.

- **IDs** (`id.rs`) — newtype wrappers: `ChangesetId`, `AgentId`,
  `EventId(u64)`, `SymbolId`, `PlanId`, plus `ContentHash` (BLAKE3,
  32 bytes) and `GitOid` (20-byte byte array, independent of `git2`).
- **Changeset** (`changeset.rs`) — the atomic unit of work. Carries a
  lifecycle status (`InProgress → Submitted / Conflicted / Resolving /
  Dropped`), the list of `SemanticOperation`s, test results, and
  background-agent process metadata.
- **Events** (`event.rs`) — `Event` and `EventKind`, the append-only
  record of everything that happens. Forward-compatible via
  `EventKind::Unknown` (unit variants fall through `serde(other)`; data
  variants are caught at the store boundary).
- **Symbols** (`symbol.rs`) — `SymbolEntry` and `SymbolKind`. Symbols are
  the currency of the semantic merge engine.
- **Conflict** (`conflict.rs`) — `ConflictDetail`, `ConflictKind`,
  `ConflictSpan`, `MergeResult`, `MergeStrategy`, `MergeReport`.
- **Traits** (`traits.rs`) — `EventStore`, `SymbolIndex`,
  `SemanticAnalyzer`. These define the contract between core and the
  downstream crates that implement them.
- **Plan** (`plan.rs`) — multi-domain task decomposition for `ph plan`.
- **Notification** (`notification.rs`) — `TrunkNotification` and
  per-file `TrunkFileStatus` used by the ripple system.

### `phantom-git`

Thin wrapper around `git2`. Deliberately contains no knowledge of events,
overlays, or semantic merging — just the repo operations needed by the
orchestrator.

- `GitOps` — `head_oid()`, `read_file_at_commit()`, `changed_files()`,
  `revert_commit_oid()`, `reset_to_commit()`, `text_merge()`.
- `tree` — build trees from blobs or overlay directories.
- `GitOid` ↔ `git2::Oid` conversions.
- `test_support` — helpers for integration tests (init repo, advance
  trunk, commit a file).

### `phantom-events`

SQLite (WAL mode) event store via `sqlx`. Schema is versioned and
forward-migrating; see [event-model.md](event-model.md) for the full
schema and migration story.

- `SqliteEventStore` — implements the `EventStore` trait. Opens a
  database file or an in-memory store for tests.
- `Projection` — derives current state (per-changeset status, per-agent
  activity) by replaying events. Supports snapshot-then-tail for speed.
- `ReplayEngine` — `materialized_changesets()` and `changesets_after()`
  power rollback.
- `EventQuery` — composable filters (by agent, by changeset, by time
  window) for the `ph log` command.

### `phantom-overlay`

The per-agent isolated filesystem. Linux-specific FUSE code is gated
behind the `fuse` feature (on by default) and `#[cfg(target_os = "linux")]`.

- `OverlayLayer` (`layer/`) — the copy-on-write engine. Upper directory
  captures writes; lower directory (the trunk working tree) is
  read-through. Deleted files are tracked in a whiteout set persisted to
  `upper/.whiteouts.json`. Split into submodules by responsibility:
  `classify`, `read`, `write`, `rename`, `maintenance`, `io_util`.
- `PhantomFs` (`fuse_fs/`) — full `fuser::Filesystem` implementation.
  Handles lookup, getattr, create, open, read, write, rename, unlink,
  mkdir, rmdir, link, readdir. Inode allocation in `inode_table.rs`.
- `OverlayManager` (`manager.rs`) — create, list, destroy overlays at
  `.phantom/overlays/<agent>/`.
- `TrunkView` (`trunk_view.rs`) — read-through to the git working tree.

### `phantom-semantic`

tree-sitter-based parsing and Weave-style entity matching. See
[semantic-merge.md](semantic-merge.md) for the algorithm.

- `Parser` — routes files to extractors by extension (primary) or exact
  filename (for `Dockerfile`, `Makefile`). Holds a reusable
  `tree_sitter::Parser` behind a `Mutex` to avoid reallocation.
- `LanguageExtractor` trait — implemented per language in
  `languages/{rust,typescript,python,go,yaml,toml,json,bash,css,hcl,dockerfile,makefile}.rs`.
- `InMemorySymbolIndex` — simple in-memory `SymbolIndex`.
- `SemanticMerger` — implements `SemanticAnalyzer`. `extract_symbols`,
  `diff_symbols`, `three_way_merge`.
- `merge/` — the merge engine:
  - `conflict.rs` — conflict detection at the symbol level.
  - `reconstruct.rs` — rebuild a file from merged symbol regions.
  - `text.rs` — line-level fallback via `diffy`.
  - `mod.rs` — short-circuit for trivial cases, then semantic, then
    text fallback.

### `phantom-orchestrator`

The coordination layer. Composes git, events, semantic, and overlay.

- `submit_service/` — the unified submit pipeline
  (`submit_and_materialize`). Scans the agent's overlay, extracts
  semantic operations, builds a `Changeset`, calls the materializer,
  appends `ChangesetSubmitted` and `ChangesetMaterialized` events, and
  returns the operation counts for the CLI.
- `materializer/` — single-changeset application to trunk. Performs the
  three-way merge per file, builds a git tree, and commits. Includes a
  cross-process lock (`lock.rs`) so concurrent submits serialize safely.
- `materialization_service/` — the materialize-and-ripple orchestrator.
  After a successful materialization, classifies trunk changes per
  active agent, performs live rebase on shadowed files, and writes
  trunk notifications.
- `ripple.rs` — `RippleChecker::check_ripple` computes the set of files
  shared between the materialized changeset and each active overlay.
- `live_rebase.rs` — three-way merges shadowed files in an agent's
  upper layer. Atomic write via tmp-file + rename. Persists the agent's
  `current_base` so subsequent submits use the correct merge base.
- `trunk_update.rs` — notification file writer.

### `phantom-session`

Everything about binding a coding session to an overlay.

- `CliAdapter` trait — session ID extraction, resume-flag construction.
  - `ClaudeAdapter` — extracts the `--resume <UUID>` token from Claude
    Code output.
  - `GenericAdapter` — fallback for arbitrary commands (no resume).
- `pty/` — PTY-based spawning. Raw-mode terminal, SIGINT forwarding,
  rolling 8 KiB output buffer for session ID capture.
- `context_file/` — generates `.phantom-task.md` with agent metadata.
  Separate contexts for standard tasks (`task.rs`), plan domains
  (`plan.rs`), and resolve sessions (`resolve.rs`).
- `signatures/` — session signature validation.
- `post_session/` — post-exit automation (auto-submit).

### `phantom-cli`

The `ph` binary.

- `main.rs` — Tokio entry point, command dispatch.
- `cli.rs` — clap `Commands` enum with aliases and the
  `external_subcommand` catch-all that routes `ph <agent>` to task
  creation.
- `context.rs` — `PhantomContext::locate()` walks up from `cwd` to find
  `.phantom/` and the repo root, then lazily opens subsystems.
- `fs/fuse.rs` — spawn/waitfor/unmount helpers for the FUSE daemon.
- `commands/` — one module per subcommand: `init`, `task` (with
  `resume.rs` and `spawn.rs`), `submit`, `status`, `tasks`, `plan`,
  `resolve`, `rollback`, `log`, `changes`, `destroy`, `background`,
  `resume`, `exec`, `down`, `fuse_mount` (internal), `agent_monitor`
  (internal).
- `services/` — validation helpers (agent ID, changeset ID).
- `ui/` — terminal styling and textbox helpers.

### `phantom-testkit`

Shared test utilities. `TestContext` builds a temp git repo with a
`.phantom/` directory wired up. Builders for `Changeset`, `Event`.
Mock implementations of the core traits for unit tests.

## The `.phantom/` directory

Every Phantom-managed repo has a `.phantom/` directory at the repo root:

```
.phantom/
├── events.db                # SQLite WAL event store
├── config.toml              # Minimal config (default_cli)
└── overlays/
    └── <agent>/
        ├── upper/            # COW upper layer (agent writes)
        │   └── .whiteouts.json
        ├── mount/            # FUSE mount point (merged view)
        ├── current_base      # Git OID the overlay is based on
        ├── cli_session.json  # Saved session ID (for --resume)
        ├── agent.log         # Background agent stdout/stderr
        ├── agent.pid         # Monitor process PID
        └── trunk-notifications/
            └── <timestamp>.json  # Ripple notifications
```

`.phantom/` is added to `.gitignore` automatically on `ph init`.

## Submit pipeline

A single `ph submit` call runs the whole submit-merge-ripple pipeline:

1. **Scan overlay** — list the agent's modified files from the upper
   layer (`OverlayLayer::modified_files()`).
2. **Parse and diff** — for each modified file, parse the base and
   current versions with tree-sitter and run `diff_symbols` to produce
   `SemanticOperation`s. Files without a grammar produce a `RawDiff`
   operation.
3. **Build changeset** — assemble a `Changeset` with the operation list,
   modified-file list, and base commit.
4. **Emit `ChangesetSubmitted`** — append the event with the extracted
   operations and the `causal_parent` pointing to `TaskCreated`.
5. **Materialize** — under a cross-process lock, for each modified file:
   - Read base, ours (trunk), theirs (agent upper).
   - Run `SemanticAnalyzer::three_way_merge`.
   - Clean → stage the new blob.
   - Conflict → record a `ConflictDetail`.
   If any file conflicts, emit `ChangesetConflicted` and abort. Otherwise
   build a tree, write a commit, update `HEAD`, and emit
   `ChangesetMaterialized`.
6. **Ripple** — compute the intersection of the new commit's changed
   files with each other active agent's touched files. For each
   affected agent:
   - Classify per file (TrunkVisible / Shadowed / RebaseMerged /
     RebaseConflict).
   - For shadowed files, call `live_rebase::rebase_agent` to three-way
     merge the trunk change into the agent's upper layer.
   - Write a `TrunkNotification` to
     `.phantom/overlays/<agent>/trunk-notifications/`.
   - Emit `LiveRebased` and `AgentNotified` events.

Failure modes that do not require manual intervention all produce a
recoverable event (`ChangesetConflicted`, `LiveRebased` with
`conflicted_files` populated). Crashes mid-materialization are covered by
the `materialize_append_crash` integration test and the cross-process
lock.

## Design invariants

1. **Dependency direction points inward.** `phantom-core` never depends
   on any other Phantom crate. Every other crate can depend on
   `phantom-core`. Infrastructure depends on domain, not vice versa.
2. **Events are immutable.** An event is never mutated or deleted; a
   rollback sets `dropped = 1` in the store and emits `ChangesetDropped`.
3. **Forward compatibility.** Unknown `EventKind` variants deserialize
   to `EventKind::Unknown`. Older binaries can read newer databases
   without crashing; they just skip what they can't understand.
4. **FUSE is optional.** `phantom-overlay` compiles without FUSE
   (non-Linux platforms, CI sandboxes). The `fuse` feature is on by
   default; `--no-fuse` skips the mount at the CLI layer.
5. **No shared state between agents.** Two agents' uppers are
   independent directories. Interference only happens via the
   explicit ripple pipeline, which is observable as events.

## Further reading

- [event-model.md](event-model.md) — event schema, causal DAG,
  projection, rollback, migrations.
- [semantic-merge.md](semantic-merge.md) — symbol extraction, entity
  matching, three-way merge algorithm, fallback strategies.
- [manual-tests.md](manual-tests.md) — scenarios that require real
  FUSE / kernel / PTY behavior and are verified by hand before release.

# CLAUDE.md — Phantom

## Project Identity

**Phantom** is an event-sourced, semantic-aware version control layer for agentic AI development, built on top of Git. It enables multiple AI coding agents to work on the same codebase simultaneously with zero human merge resolution, automatic conflict detection at the symbol level, and instant propagation of finished work.

Phantom is written in **Rust**. It targets Linux first (FUSE support), with macOS support via NFS overlay as a secondary target.

## Problem Statement

Git branches model human workflows — long-lived divergent lines of work reconciled later. Agentic development is different: multiple agents work on small, scoped tasks simultaneously, and their outputs must compose cleanly without manual merge resolution.

Current approaches (git worktrees per agent) provide filesystem isolation but do nothing about merge conflicts. When Agent A and Agent B both add functions to the same file, git declares a conflict even if the changes are logically independent. A human must intervene. This breaks the core promise of agentic parallelism.

Phantom solves this by combining four architectural ideas into a single system:

| # | Concept | Role in Phantom |
|---|---------|-----------------|
| 1 | **Changeset model** | Unit of work (replaces branches) |
| 3 | **Semantic merging** | Conflict resolution via AST, not lines |
| 4 | **Shadow overlays** | Runtime isolation per agent (FUSE) |
| 6 | **Event sourcing** | Auditability, rollback, replay |

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                    CLI: `phantom`                    │
│  phantom init · phantom task · phantom status      │
│  phantom materialize · phantom rollback · phantom log│
├─────────────────────────────────────────────────────┤
│                   Orchestrator                       │
│  Task queue · changeset priority · task loop     │
│  Ripple checker (notify agents of trunk changes)     │
├─────────────────────────────────────────────────────┤
│                 Semantic Index                        │
│  Live AST map of trunk: symbols, types, imports,     │
│  dependencies. Updated on each materialization.      │
│  Powered by tree-sitter via `tree-sitter` crate.     │
├──────────┬──────────┬──────────┬────────────────────┤
│ Agent A  │ Agent B  │ Agent C  │  ...               │
│ Overlay  │ Overlay  │ Overlay  │                    │
│ (FUSE)   │ (FUSE)   │ (FUSE)   │                    │
├──────────┴──────────┴──────────┴────────────────────┤
│              Trunk (single source of truth)          │
│              Backed by a real git repository         │
├─────────────────────────────────────────────────────┤
│                Event Log (append-only)               │
│  SQLite WAL-mode database                            │
│  Every agent action, every materialization           │
└─────────────────────────────────────────────────────┘
```

## Workspace Layout

```
phantom/
├── Cargo.toml                  # Workspace root
├── CLAUDE.md                   # This file
├── README.md
├── LICENSE                     # MIT
│
├── crates/
│   ├── phantom-cli/            # Binary crate — the `phantom` command
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       └── commands/       # Subcommand modules
│   │           ├── mod.rs
│   │           ├── init.rs     # `phantom init` — initialize phantom in a git repo
│   │           ├── task.rs     # `phantom task` — assign task to agent overlay
│   │           ├── status.rs   # `phantom status` — show overlays, locks, queue
│   │           ├── materialize.rs  # `phantom materialize` — commit overlay to trunk
│   │           ├── rollback.rs # `phantom rollback` — drop changeset, replay
│   │           └── log.rs      # `phantom log` — query event log
│   │
│   ├── phantom-core/           # Core types, traits, error handling
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── changeset.rs    # Changeset struct, metadata, serialization
│   │       ├── event.rs        # Event types for the event log
│   │       ├── symbol.rs       # Symbol identity types (name, kind, scope, hash)
│   │       ├── conflict.rs     # Conflict types and resolution strategies
│   │       └── error.rs
│   │
│   ├── phantom-overlay/        # FUSE overlay filesystem per agent
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── fuse_fs.rs      # FUSE Filesystem trait impl (uses `fuser` crate)
│   │       ├── layer.rs        # Copy-on-write layer (upper = agent writes, lower = trunk)
│   │       ├── manager.rs      # Create/destroy/list overlays
│   │       └── trunk_view.rs   # Read-through to current trunk state
│   │
│   ├── phantom-semantic/       # Semantic index and AST-based merging
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── index.rs        # SymbolIndex — maps file→symbols, symbol→hash
│   │       ├── parser.rs       # tree-sitter parsing, symbol extraction
│   │       ├── merge.rs        # Three-way semantic merge engine
│   │       ├── diff.rs         # Semantic diff: changeset → list of operations
│   │       └── languages/      # Per-language symbol extraction configs
│   │           ├── mod.rs
│   │           ├── rust.rs
│   │           ├── typescript.rs
│   │           ├── python.rs
│   │           └── go.rs
│   │
│   ├── phantom-events/         # Event log (append-only, SQLite-backed)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── store.rs        # SQLite event store (WAL mode)
│   │       ├── replay.rs       # Event replay engine
│   │       ├── query.rs        # Query events by agent, time, symbol, changeset
│   │       └── projection.rs   # Project event log → current codebase state
│   │
│   └── phantom-orchestrator/   # Coordination layer
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── scheduler.rs    # Task queue, priority, scheduling
│           ├── materializer.rs # Apply changeset to trunk atomically
│           ├── ripple.rs       # Notify agents when trunk changes under them
│           └── git.rs          # Git operations (commit, read tree, worktree mgmt)
│
├── tests/
│   ├── integration/
│   │   ├── two_agents_disjoint.rs      # Two agents, no file overlap → auto-merge
│   │   ├── two_agents_same_file.rs     # Same file, different symbols → auto-merge
│   │   ├── two_agents_same_symbol.rs   # Same symbol → conflict detection
│   │   ├── materialize_and_ripple.rs   # Agent B sees Agent A's materialized work
│   │   ├── rollback_replay.rs          # Drop changeset, replay downstream
│   │   └── event_log_query.rs          # Query event log for audit
│   └── fixtures/
│       └── sample_repo/                # Minimal git repo for testing
│
└── docs/
    ├── architecture.md
    ├── semantic-merge.md
    └── event-model.md
```

## Core Concepts

### 1. Changesets (replaces branches)

A changeset is the atomic unit of work in Phantom. When an agent is assigned a task, it produces a changeset — not a branch.

```rust
/// crates/phantom-core/src/changeset.rs

pub struct Changeset {
    /// Unique identifier (e.g. "cs-0042")
    pub id: ChangesetId,
    /// Which agent produced this
    pub agent_id: AgentId,
    /// Human-readable task description
    pub task: String,
    /// The trunk commit this changeset was built against
    pub base_commit: git2::Oid,
    /// Files touched (for quick overlap detection before semantic analysis)
    pub files_touched: Vec<PathBuf>,
    /// Semantic operations extracted by phantom-semantic after the agent finishes
    pub operations: Vec<SemanticOperation>,
    /// Test results (pass/fail/skip counts)
    pub test_result: Option<TestResult>,
    /// Timestamp
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Current status
    pub status: ChangesetStatus,
}

pub enum ChangesetStatus {
    InProgress,     // Agent is still working
    Submitted,      // Agent finished, awaiting merge check
    Merging,        // Semantic merge in progress
    Materialized,   // Successfully committed to trunk
    Conflicted,     // Semantic conflict detected, needs re-task
    Dropped,        // Rolled back / removed from event log
}

pub enum SemanticOperation {
    AddFunction { module: String, name: String, signature: String, body_hash: ContentHash },
    ModifyFunction { module: String, name: String, old_hash: ContentHash, new_hash: ContentHash },
    DeleteFunction { module: String, name: String },
    AddStruct { module: String, name: String, fields_hash: ContentHash },
    ModifyStruct { module: String, name: String, old_hash: ContentHash, new_hash: ContentHash },
    AddImport { module: String, path: String },
    RemoveImport { module: String, path: String },
    AddDependency { name: String, version: String },
    ModifyDependency { name: String, old_version: String, new_version: String },
    AddTest { module: String, name: String, body_hash: ContentHash },
    AddFile { path: PathBuf },
    DeleteFile { path: PathBuf },
    /// Catch-all for changes the semantic layer can't classify
    RawDiff { path: PathBuf, patch: String },
}
```

Changesets are **reorderable**: if cs-002 has no dependency on cs-001 (no overlapping symbols), they can be materialized in either order. This is a fundamental advantage over branches, which encode arbitrary linear history.

### 2. FUSE Overlay Filesystem (agent isolation)

Each agent gets a FUSE-mounted overlay filesystem. The overlay has two layers:

- **Lower layer (read-only):** Current trunk state. Reads fall through to trunk.
- **Upper layer (read-write):** Agent's modifications. Writes go here (copy-on-write).

```
Agent B's view:
  /phantom/overlays/agent-b/
    src/
      main.rs    → [falls through to trunk — agent hasn't touched it]
      db.rs      → [agent B's modified version — stored in upper layer]
      cache.rs   → [new file created by agent B — upper layer]
```

**Implementation details:**
- Use the `fuser` crate (Rust rewrite of libfuse, production-ready, actively maintained).
- Each overlay is a `PhantomFs` struct implementing `fuser::Filesystem`.
- On `read`/`getattr`: check upper layer first, fall through to lower (trunk) if not found.
- On `write`/`create`: always go to upper layer.
- On `unlink`: place a whiteout marker in upper layer.
- Upper layer storage: a directory on the host filesystem (`~/.phantom/overlays/<agent-id>/upper/`).
- Lower layer: the git working tree at the trunk HEAD commit.

**Trunk update propagation:** When another agent materializes (commits to trunk), the lower layer pointer updates to the new HEAD. The overlay immediately reflects this for any file the current agent hasn't modified. No rebase step needed — agents automatically see fresh trunk on their next read.

**macOS fallback:** FUSE requires macFUSE (kernel extension, fragile). On macOS, use a localhost NFS server instead (same approach as AgentFS). The copy-on-write semantics are identical, only the transport differs.

### 3. Semantic Index and Merging (AST-level conflict resolution)

The semantic index is a live map of every symbol in trunk. It is updated on each materialization.

```rust
/// crates/phantom-semantic/src/index.rs

pub struct SymbolIndex {
    /// symbol_id → SymbolEntry
    symbols: HashMap<SymbolId, SymbolEntry>,
    /// file_path → vec of symbol_ids in that file
    file_to_symbols: HashMap<PathBuf, Vec<SymbolId>>,
    /// The trunk commit this index was built from
    indexed_at: git2::Oid,
}

pub struct SymbolEntry {
    pub id: SymbolId,
    pub kind: SymbolKind,          // Function, Struct, Enum, Trait, Import, Const, etc.
    pub name: String,
    pub scope: String,             // e.g. "crate::handlers" or "src/handlers.ts::default"
    pub file: PathBuf,
    pub byte_range: Range<usize>,  // Position in file
    pub content_hash: ContentHash, // Hash of the symbol's AST subtree
}

pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Import,
    Const,
    TypeAlias,
    Module,
    Test,
    // Language-specific kinds can be added
    Class,       // TS/Python/Go
    Interface,   // TS/Go
    Method,      // Within a class/impl
}
```

**Parsing:** Use the `tree-sitter` crate with language-specific grammars (`tree-sitter-rust`, `tree-sitter-typescript`, `tree-sitter-python`, `tree-sitter-go`). Parse each file into a CST, then walk the tree to extract top-level and nested symbol definitions.

**Semantic diff:** When an agent submits a changeset, Phantom parses both the base version and the agent's version of each touched file. It computes a list of `SemanticOperation`s by comparing the symbol sets:

```
Base symbols in handlers.rs: { handle_login, handle_logout }
Agent's symbols in handlers.rs: { handle_login, handle_logout, handle_register }

Diff: AddFunction { module: "handlers", name: "handle_register", ... }
```

**Three-way semantic merge:** When materializing, compare the changeset's operations against any operations that occurred on trunk since the changeset's base commit:

```
Changeset cs-0042 operations:
  - AddFunction: handlers::handle_register
  - ModifyFunction: router::build (added register route)

Trunk changes since cs-0042's base:
  - cs-0040: AddFunction: handlers::handle_admin
  - cs-0041: ModifyFunction: router::build (added admin route)

Conflict analysis:
  handlers::handle_register vs handlers::handle_admin → NO CONFLICT (different symbols)
  router::build: both modified → DRILL DOWN:
    cs-0041 added: `.route("/admin", handle_admin)`
    cs-0042 added: `.route("/register", handle_register)`
    Both are ADDITIVE route insertions → AUTO-MERGEABLE
```

**Conflict categories:**

| Scenario | Resolution |
|----------|-----------|
| Both add different symbols to same file | Auto-merge (no conflict) |
| Both add different fields to same struct | Auto-merge (disjoint field changes) |
| Both modify same function body | **CONFLICT** — re-task agent |
| One modifies, other deletes same symbol | **CONFLICT** — re-task agent |
| Both add same import | Auto-deduplicate |
| Both modify same dependency version | **CONFLICT** — re-task agent |
| Additive insertions to same collection (routes, middleware, etc.) | Auto-merge |

**Fallback:** For files the semantic layer can't parse (binary files, config formats without tree-sitter grammars, etc.), fall back to git's line-based three-way merge. If that also conflicts, mark as `RawDiff` conflict.

**Key prior art to study:**
- **Mergiraf** — AST-level merge driver for git, uses tree-sitter + GumTree matching algorithm. Written in Rust, GPLv3. Study its architecture: parse → match → flatten to facts → reconstruct merged tree.
- **Weave** — Entity-level semantic merge driver. Uses tree-sitter via `sem-core`. Matches entities by identity (name + type + scope) rather than AST node position. Resolves 31/31 benchmarks vs git's 15/31. Written in Rust.
- **Difftastic** — Structural diff tool (Rust, tree-sitter). Does not merge, but its AST diffing approach informs semantic diff design.

### 4. Event Log (auditability, rollback, replay)

Every action in Phantom is an immutable event appended to a SQLite database in WAL mode.

```rust
/// crates/phantom-core/src/event.rs

pub struct Event {
    pub id: EventId,                // Auto-incrementing
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub changeset_id: ChangesetId,
    pub agent_id: AgentId,
    pub kind: EventKind,
}

pub enum EventKind {
    // Lifecycle
    OverlayCreated { base_commit: git2::Oid },
    OverlayDestroyed,

    // Agent work
    FileWritten { path: PathBuf, content_hash: ContentHash },
    FileDeleted { path: PathBuf },

    // Changeset lifecycle
    ChangesetSubmitted { operations: Vec<SemanticOperation> },
    ChangesetMergeChecked { result: MergeCheckResult },
    ChangesetMaterialized { new_commit: git2::Oid },
    ChangesetConflicted { conflicts: Vec<ConflictDetail> },
    ChangesetDropped { reason: String },

    // Ripple
    TrunkAdvanced { old_commit: git2::Oid, new_commit: git2::Oid },
    AgentNotified { agent_id: AgentId, changed_symbols: Vec<SymbolId> },

    // Tests
    TestsRun { passed: u32, failed: u32, skipped: u32 },
}
```

**Rollback via replay:**

To roll back changeset cs-0040:
1. Mark all events with `changeset_id = cs-0040` as `Dropped`.
2. Identify all changesets that materialized *after* cs-0040.
3. Reset trunk to the commit *before* cs-0040's materialization.
4. Replay remaining changesets in order, running semantic merge for each.
5. Any changeset that depended on cs-0040's symbols and fails merge → re-task.

```
Event log replay without cs-0040:
  cs-0039: ✅ applies clean
  cs-0041: ✅ no dependency on cs-0040
  cs-0042: ✅ no dependency on cs-0040
  cs-0045: ❌ depends on symbol from cs-0040 → re-task
```

**Query capabilities:**
- "What did Agent B do?" → `SELECT * FROM events WHERE agent_id = 'agent-b'`
- "Why does this function exist?" → trace events that created/modified the symbol
- "What if we hadn't done task X?" → replay without that changeset's events

## CLI Design

```bash
# Initialize phantom in an existing git repo
phantom init
# Creates .phantom/ directory with config, event log DB, overlay root

# Dispatch a task to a new agent overlay
phantom task --agent agent-a --task "add rate limiting to API"
# Creates FUSE overlay at .phantom/overlays/agent-a/
# Agent sees a normal filesystem, writes go to upper layer

# Check status of all agents and the changeset queue
phantom status
# Shows: active overlays, pending changesets, trunk HEAD, event count

# Submit an agent's work as a changeset (called by agent or wrapper script)
phantom submit --agent agent-a
# Extracts semantic operations, creates changeset, appends events

# Run semantic merge check and materialize to trunk
phantom materialize --changeset cs-0042
# Parses operations, runs three-way semantic merge, commits to git if clean
# Notifies other running agents of trunk change (ripple)

# Roll back a changeset
phantom rollback --changeset cs-0040
# Drops events, resets trunk, replays remaining changesets

# Query the event log
phantom log
phantom log --agent agent-b
phantom log --changeset cs-0042
phantom log --symbol "handlers::handle_login"
phantom log --since "2h ago"

# Tear down an agent's overlay
phantom destroy --agent agent-a
```

## Dependencies

### Rust Crates

| Crate | Purpose | Version |
|-------|---------|---------|
| `fuser` | FUSE filesystem implementation | latest stable |
| `git2` | Git operations (libgit2 bindings) | latest stable |
| `tree-sitter` | Incremental parsing library | latest stable |
| `tree-sitter-rust` | Rust grammar | latest stable |
| `tree-sitter-typescript` | TypeScript grammar | latest stable |
| `tree-sitter-python` | Python grammar | latest stable |
| `tree-sitter-go` | Go grammar | latest stable |
| `rusqlite` | SQLite with WAL mode for event log | latest stable |
| `clap` | CLI argument parsing | 4.x |
| `serde` + `serde_json` | Serialization for changesets, events | 1.x |
| `chrono` | Timestamps | latest stable |
| `blake3` | Content hashing (fast, parallel) | latest stable |
| `tokio` | Async runtime (for overlay I/O, ripple notifications) | 1.x |
| `tracing` | Structured logging | latest stable |
| `thiserror` | Error types | latest stable |
| `tempfile` | Temp dirs for testing | latest stable |

### System Dependencies

- **Linux:** `libfuse3-dev` (or `fuse3` package) for FUSE support
- **macOS:** No kernel extension needed — use NFS overlay fallback
- **Git:** Standard git installation (Phantom operates on real git repos)

## Implementation Phases

### Phase 1: Foundation (MVP)
**Goal:** Two agents can work in parallel on the same repo. Disjoint file changes auto-merge. Same-file changes fall back to git merge.

- [ ] `phantom-core`: Changeset, Event, SymbolId types
- [ ] `phantom-events`: SQLite event store (append, query, basic replay)
- [ ] `phantom-overlay`: FUSE overlay with copy-on-write (single overlay works)
- [ ] `phantom-orchestrator`: Git operations (commit, read tree), basic materializer (git merge)
- [ ] `phantom-cli`: `phantom init`, `phantom task`, `phantom submit`, `phantom materialize`, `phantom status`
- [ ] Integration test: two agents, disjoint files, both materialize cleanly

### Phase 2: Semantic Merging
**Goal:** Two agents can modify the same file and auto-merge if they touch different symbols.

- [ ] `phantom-semantic`: tree-sitter parsing for Rust files (start with one language)
- [ ] Symbol extraction: functions, structs, enums, impls, imports
- [ ] SymbolIndex: build and update on materialization
- [ ] Semantic diff: changeset → list of SemanticOperations
- [ ] Three-way semantic merge: detect symbol-level conflicts
- [ ] Auto-merge for additive, non-overlapping symbol changes
- [ ] Conflict reporting with clear messages ("Agent A modified `get_user`, Agent B deleted `get_user`")
- [ ] Integration test: two agents add different functions to same file → auto-merge

### Phase 3: Ripple & Re-task
**Goal:** Running agents are automatically notified when trunk changes affect their in-progress work.

- [ ] Ripple checker: after materialization, diff new trunk against each active overlay's base
- [ ] Identify which active agent overlays touch symbols that just changed
- [ ] Notification mechanism (file-based signal, Unix socket, or stdout message)
- [ ] Re-task protocol: agent wrapper detects notification, re-reads affected files
- [ ] Integration test: Agent A materializes, Agent B's overlay auto-updates, Agent B re-runs tests

### Phase 4: Rollback & Replay
**Goal:** Any changeset can be surgically removed and downstream work is automatically identified for re-task.

- [ ] `phantom rollback`: mark events as dropped, reset trunk, replay
- [ ] Dependency graph: track which changesets depend on which symbols
- [ ] Selective replay: skip independent changesets, re-task dependent ones
- [ ] Integration test: materialize 5 changesets, rollback #3, verify #4 and #5 replay correctly

### Phase 5: Multi-Language & Production Polish
**Goal:** Support TypeScript, Python, Go. Production-ready error handling, performance, docs.

- [ ] Language support: TypeScript, Python, Go symbol extraction
- [ ] macOS NFS overlay fallback
- [ ] Performance: incremental index updates (don't re-parse unchanged files)
- [ ] Configuration file (`.phantom/config.toml`)
- [ ] Agent wrapper scripts for Claude Code, Cursor, Codex
- [ ] Documentation, README, usage guides
- [ ] Benchmarks: merge throughput, overlay I/O overhead

## Coding Conventions

### Rust Style
- Edition 2024 (`edition = "2024"` in Cargo.toml workspace)
- Use `thiserror` for all error types. Each crate defines its own error enum.
- Use `tracing` for all logging. No `println!` in library crates.
- All public types and functions have doc comments.
- Use `#[must_use]` on functions returning `Result` or important values.
- Prefer `&str` over `String` in function parameters where possible.
- Use newtypes for IDs: `ChangesetId(String)`, `AgentId(String)`, `EventId(u64)`, `SymbolId(String)`.
- Keep crate boundaries clean: `phantom-core` has zero dependencies on other phantom crates.

### Testing
- Unit tests in `#[cfg(test)]` modules within each source file.
- Integration tests in `tests/integration/` using real git repos (created via `git2` in test setup).
- Use `tempfile::TempDir` for all test repos — never write to the real filesystem.
- Test naming: `test_<scenario>_<expected_outcome>`, e.g. `test_two_agents_disjoint_files_auto_merges`.
- Every semantic merge scenario needs a test case with fixture code.

### Git Conventions
- Conventional commits: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`
- One logical change per commit.
- `main` branch is always clean and passing CI.

## Key Design Decisions

### Why FUSE over git worktrees?
Git worktrees provide directory isolation but agents can still read/write outside their worktree. FUSE provides true filesystem-level isolation with enforcement. Additionally, FUSE overlays allow instant trunk propagation — the lower layer pointer updates, and the agent immediately sees new trunk files without any rebase step. Git worktrees require explicit `git rebase` or `git merge`.

### Why SQLite for the event log?
Single-file database, zero deployment complexity, WAL mode supports concurrent readers with a single writer, and it's embeddable in the Rust binary via `rusqlite`. The event log is append-heavy with analytical reads — SQLite handles this pattern well. If scale ever demands it, the event store interface is abstract enough to swap in PostgreSQL or FoundationDB.

### Why tree-sitter over language-native parsers?
Tree-sitter provides a uniform API across 100+ languages, is incremental (sub-millisecond re-parse on edits), error-tolerant (continues parsing through syntax errors), and has production-quality Rust bindings. The tradeoff is that tree-sitter grammars model CSTs (concrete syntax trees) designed for syntax highlighting, not full semantic analysis. This is acceptable for Phantom's use case — we only need to extract top-level symbol boundaries (functions, structs, imports), not perform type checking or name resolution.

### Why not use Mergiraf / Weave directly?
Both operate as git merge drivers — they run during `git merge` and resolve conflicts in individual files. Phantom needs a *system-level* orchestrator that coordinates multiple agents, manages overlays, and maintains a live symbol index across the entire codebase. However, Phantom's semantic merge engine should study and draw from both:
- From **Mergiraf**: The GumTree-based AST matching algorithm and the "facts-based" merge reconstruction approach.
- From **Weave**: Entity-level matching by identity (name + type + scope) and the `sem-core` entity extraction library.

### Why event sourcing?
Traditional VCS stores snapshots (commits) and computes diffs on demand. Event sourcing stores the diffs (operations) and computes snapshots on demand. For agentic development, this gives:
- **Auditability:** Exactly which agent did what, when, and why.
- **Surgical rollback:** Remove one changeset without reverting everything after it.
- **Replay:** "What would the codebase look like if we hadn't done X?"
- **Conflict tracing:** When a conflict occurs, trace exactly which events are incompatible.

### Why not file-level locks?
Locks are pessimistic — they prevent parallelism when there *might* be a conflict. Semantic merging is optimistic — it allows full parallelism and only flags *actual* symbol-level conflicts after the fact. Since agents are cheap to re-task, optimistic concurrency wins. Locking only makes sense when re-task is expensive (human developers), not when it's cheap (AI agents).

## Environment Setup

```bash
# Install system dependencies (Ubuntu/Debian)
sudo apt install libfuse3-dev pkg-config build-essential

# Clone and build
git clone <repo-url> phantom
cd phantom
cargo build

# Run tests
cargo test

# Install locally
cargo install --path crates/phantom-cli

# Initialize in a git repo
cd /path/to/your/git/repo
phantom init
```

## Glossary

| Term | Definition |
|------|-----------|
| **Changeset** | An isolated, rebasable unit of work produced by an agent. Contains a diff, semantic operations metadata, and test results. Replaces the concept of a branch. |
| **Overlay** | A FUSE-mounted copy-on-write filesystem per agent. Reads fall through to trunk, writes go to the upper layer. |
| **Trunk** | The single source of truth — the `main` branch of the underlying git repo. |
| **Materialize** | The act of committing a changeset's changes to trunk atomically, after passing semantic merge checks. |
| **Semantic Index** | A live map of every symbol (function, struct, import, etc.) in trunk, built by parsing with tree-sitter. |
| **Semantic Operation** | A structured description of what an agent did: "added function X", "modified struct Y", etc. |
| **Ripple** | Notification sent to active agents when trunk changes under them. |
| **Event** | An immutable record of something that happened in Phantom. The event log is the source of truth for auditability and rollback. |
| **Replay** | Re-applying changesets from the event log after a rollback, detecting which downstream work is affected. |
| **Content Hash** | A BLAKE3 hash of a symbol's AST subtree, used for fast equality checks and change detection. |

# CLAUDE.md — Phantom

## What is Phantom

Phantom is an event-sourced, semantic-aware version control layer for agentic AI development, built on top of Git. It enables multiple AI coding agents to work on the same codebase simultaneously with automatic symbol-level conflict detection, FUSE-based filesystem isolation, and instant propagation of finished work.

Written in Rust (edition 2024, `rust-version = "1.85"`). Linux-first (FUSE). MIT license.

## Quick Reference

```bash
# Build and test
cargo build && cargo test

# System dependency (Linux)
sudo apt install libfuse3-dev pkg-config build-essential

# Install the binary
cargo install --path crates/phantom-cli

# Usage
phantom init                         # Initialize in a git repo
phantom <agent-name>                 # Create/resume agent overlay (interactive)
phantom <agent-name> --background    # Run agent in background
phantom plan "add caching layer"     # Decompose feature into parallel agents
phantom submit <agent>               # Submit overlay: semantic merge to trunk, ripple to agents
phantom resolve <agent>              # Auto-resolve conflicts via AI agent
phantom resume                       # Select and resume an interactive agent session
phantom tasks                        # List all agent task overlays
phantom status                       # Show overlays, changesets, queue
phantom log [filter]                 # Query event log (agent name or cs-id)
phantom changes                      # Recent submits and materializations
phantom background                   # Watch background agents
phantom destroy <agent>              # Tear down overlay
phantom rollback [changeset-id]      # Drop changeset, replay downstream
phantom down                         # Unmount all overlays, remove .phantom/
```

## Workspace Structure

9 crates + 1 integration test crate:

```
crates/
├── phantom-core/           # Core types, traits, errors (zero deps on other phantom crates)
├── phantom-git/            # Git operations (git2 wrapper, tree building, text merge)
├── phantom-events/         # SQLite WAL event store (sqlx)
├── phantom-overlay/        # FUSE overlay filesystem (fuser, feature-gated)
├── phantom-semantic/       # Tree-sitter parsing + semantic merge engine
├── phantom-orchestrator/   # Materializer, ripple, live rebase, submit service (uses phantom-git)
├── phantom-session/        # PTY management, CLI adapters, context files, post-session automation
├── phantom-cli/            # Binary crate — the `phantom` command
└── phantom-testkit/        # Shared test utilities (builders, mocks, test repos)

tests/integration/          # End-to-end tests with real git repos
```

## Crate Responsibilities

### phantom-core (`crates/phantom-core/`)
Zero dependencies on other phantom crates. Defines all shared types:
- **IDs**: `ChangesetId`, `AgentId`, `EventId`, `SymbolId`, `ContentHash` (BLAKE3), `GitOid` (20-byte, no git2 dependency), `PlanId`
- **Changeset**: status lifecycle (`InProgress → Submitted/Conflicted/Resolving/Dropped`), `SemanticOperation` (AddSymbol/ModifySymbol/DeleteSymbol/AddFile/DeleteFile/RawDiff), `TestResult`
- **Event**: `EventKind` enum with 17+ variants including `TaskCreated`, `ChangesetSubmitted`, `ChangesetMaterialized`, `LiveRebased`, `PlanCreated`, `AgentLaunched/Completed`, `Unknown` (forward-compat via `serde(other)`)
- **Conflict**: `ConflictDetail` with `ConflictKind` (BothModifiedSymbol, ModifyDeleteSymbol, BothModifiedDependencyVersion, RawTextConflict, BinaryFile) and `ConflictSpan` (byte ranges + line numbers)
- **Traits**: `EventStore` (async), `SymbolIndex`, `SemanticAnalyzer` (extract_symbols, diff_symbols, three_way_merge)
- **Plan**: multi-domain task decomposition (`Plan`, `PlanDomain`, `PlanStatus`)
- **Notification**: `TrunkNotification` with per-file `TrunkFileStatus` (TrunkVisible/Shadowed/RebaseMerged/RebaseConflict)

### phantom-git (`crates/phantom-git/`)
Git operations built on `git2`. Depends only on `phantom-core` and `git2` — no event store, semantic analysis, or overlay dependencies.
- `GitOps`: thin wrapper around `git2::Repository` — `head_oid()`, `read_file_at_commit()`, `changed_files()`, `revert_commit_oid()`, `reset_to_commit()`, `text_merge()`
- `GitError`: typed error enum for git operations
- `tree`: tree building from blobs/overlay — `build_tree_from_oids()`, `create_blobs_from_overlay()`, `create_blobs_from_content()`
- `oid_to_git_oid` / `git_oid_to_oid`: lossless conversions between `GitOid` and `git2::Oid`
- `test_support`: test repo helpers (`init_repo`, `advance_trunk`, `commit_file`)

### phantom-events (`crates/phantom-events/`)
SQLite WAL-mode event store via `sqlx`. Schema versioned (currently v2 with `kind_version` column).
- `SqliteEventStore`: implements `EventStore` trait, supports `in_memory()` and `open(path)`
- `EventQuery`: multi-filter queries (agent + changeset + time range intersection)
- `Projection`: derives current state from events (active agents, pending changesets, changeset status)
- `ReplayEngine`: `materialized_changesets()`, `changesets_after()` for rollback ordering
- Forward compatibility: unrecognized `EventKind` variants (unit or data-carrying) deserialize as `EventKind::Unknown`

### phantom-overlay (`crates/phantom-overlay/`)
FUSE overlay filesystem (feature-gated: `fuse` feature on by default).
- `OverlayLayer`: copy-on-write layer with upper (writes) + lower (trunk read-through). Whiteout markers for deletions. `modified_files()` for changeset extraction.
- `PhantomFs`: full `fuser::Filesystem` implementation (Linux only, `#[cfg(target_os = "linux")]`). Inode management, open file handles, rename, hardlink support.
- `OverlayManager`: create/destroy/list overlays at `.phantom/overlays/<agent>/`
- `TrunkView`: read-through to git working tree

### phantom-semantic (`crates/phantom-semantic/`)
Tree-sitter-based parsing and Weave-style entity matching.
- `Parser`: routes files by extension (and exact filename for `Dockerfile`, `Makefile`) to language extractors. Built-in: Rust (`.rs`), TypeScript/JavaScript (`.ts`, `.js`, `.tsx`, `.jsx`), Python (`.py`), Go (`.go`), YAML (`.yml`, `.yaml`), TOML (`.toml`), JSON (`.json`), Bash (`.sh`, `.bash`, `.zsh`), CSS (`.css`), HCL/Terraform (`.tf`, `.hcl`), Dockerfile, Makefile (`.mk`)
- `InMemorySymbolIndex`: implements `SymbolIndex` trait
- `SemanticMerger`: implements `SemanticAnalyzer` trait — extract symbols, diff, three-way merge
- `diff`: entity-key-based diffing (scope + name + kind)
- Symbol extraction per language via `LanguageExtractor` trait

### phantom-orchestrator (`crates/phantom-orchestrator/`)
Pure coordination layer that composes `phantom-git`, `phantom-events`, `phantom-semantic`, and `phantom-overlay`. Re-exports `phantom-git` types via `phantom_orchestrator::git` for backward compatibility.
- `Materializer`: applies changeset to trunk with semantic merge check. Three-way merge per file, atomic commit.
- `submit_service`: unified submit-and-materialize pipeline — extracts semantic operations from overlay, appends `ChangesetSubmitted` event, calls materialization service to merge and commit to trunk
- `materialization_service`: materialize-and-ripple orchestration — materialize, classify trunk changes per agent, live rebase shadowed files, emit notifications and audit events
- `RippleChecker`: detects file overlap between materialized changeset and active agent overlays
- `live_rebase`: three-way merge of shadowed files in agent upper layers. Atomic write (tmp + rename). Persists `current_base` per agent.
- `scheduler`: task queue and scheduling

### phantom-session (`crates/phantom-session/`)
- `CliAdapter` trait: session resumption abstraction per coding CLI. `ClaudeAdapter` (extracts UUID from `claude --resume <id>` output), `GenericAdapter` fallback.
- `pty`: PTY-based process spawning with raw-mode terminal, SIGINT handling, rolling 8KB output buffer for session ID extraction
- `context_file`: generates `.phantom-task.md` inside overlay with agent metadata, task description, and available commands. Submodules: `task.rs` (standard task context), `plan.rs` (plan domain instructions), `resolve.rs` (three-way conflict resolution context)
- `signatures`: session signature validation/handling
- `post_session`: auto-submit flow after agent finishes (submit includes materialization)

### phantom-testkit (`crates/phantom-testkit/`)
- `TestContext`: creates temp git repos for integration tests
- `builders`: test data builders for changesets, events
- `mocks`: mock implementations of core traits

## Key Dependency Decisions

| Crate | Why |
|-------|-----|
| `sqlx` (not rusqlite) | Async SQLite with compile-time query checking, WAL mode |
| `fuser` 0.17 | Rust FUSE impl, feature-gated to allow non-Linux builds |
| `git2` 0.20 | libgit2 bindings — used only in orchestrator and CLI |
| `tree-sitter` 0.25 | Incremental parsing for symbol extraction (13 language grammars) |
| `diffy` 0.4 | Text-level three-way merge fallback |
| `blake3` 1.x | Content hashing for symbol change detection |
| `nix` 0.30 | PTY/terminal management in session crate |
| `dialoguer` 0.11 | Interactive CLI prompts |
| `uuid` 1.x | UUID v7 for time-ordered changeset IDs |
| `console` 0.15 | Terminal styling and colors in CLI output |

## CLI Architecture

The CLI uses external subcommand parsing: `phantom <agent-name>` is caught by `ExternalTask` and parsed as `TaskArgs`. This means any unrecognized subcommand becomes an agent name for task creation.

`PhantomContext` (in `context.rs`) locates the `.phantom/` directory and repository root by walking up from cwd. Subsystems (git, events, overlays, semantic) are opened lazily via `open_*` methods.

`submit` performs both submission and materialization in a single step via `submit_service::submit_and_materialize()`. There is no separate `materialize` command.

Hidden internal commands: `_fuse-mount` (FUSE daemon), `_agent-monitor` (background agent watcher).

Key aliases: `re` (resume), `t` (tasks), `st` (status), `sub` (submit), `res` (resolve), `rb` (rollback), `l` (log), `c` (changes), `b` (background), `rm` (destroy).

## Data Flow

1. **Task creation**: `phantom <agent>` → creates overlay dirs + FUSE mount → emits `TaskCreated` event → spawns CLI session (interactive or background)
2. **Agent work**: agent reads/writes inside FUSE overlay (upper layer captures writes, lower falls through to trunk)
3. **Submit + materialize**: `phantom submit` → scans `modified_files()` → tree-sitter extracts symbols from base and current → diffs to `SemanticOperation` list → appends `ChangesetSubmitted` event → three-way semantic merge per file → atomic git commit → ripple check → live rebase shadowed files in other agents' uppers → write `TrunkNotification` files → emit audit events
4. **Conflict resolution**: `phantom resolve` → finds conflicted changeset → extracts base/ours/theirs → generates conflict-specific `.phantom-task.md` → launches background AI agent

## Coding Conventions

- Edition 2024. Workspace dependencies in root `Cargo.toml`.
- `thiserror` for library error enums, `anyhow` in CLI/binary.
- `tracing` for logging. No `println!` in library crates (OK in CLI for user output).
- Newtype IDs: `ChangesetId(String)`, `AgentId(String)`, `EventId(u64)`, `SymbolId(String)`, `PlanId(String)`, `GitOid([u8; 20])`, `ContentHash([u8; 32])`.
- `phantom-core` has zero dependencies on other phantom crates. All dependency arrows point inward.
- FUSE code is feature-gated (`#[cfg(feature = "fuse")]` / `#[cfg(target_os = "linux")]`).
- Unit tests in `#[cfg(test)]` modules. Integration tests in `tests/integration/` using real git repos via `git2` + `tempfile::TempDir`.
- Serde roundtrip tests on all core types. Forward-compatibility tests for unknown event variants.

## Implementation Status

### Complete
- Core type system (changesets, events, symbols, conflicts, plans, notifications)
- Event store with SQLite WAL, schema versioning, forward-compatible deserialization
- FUSE overlay filesystem with full Filesystem trait impl (read/write/create/delete/rename/hardlink)
- Copy-on-write layer with whiteout markers
- Tree-sitter parsing for Rust, TypeScript/JavaScript (including JSX/TSX), Python, Go, YAML, TOML, JSON, Bash, CSS, HCL/Terraform, Dockerfile, Makefile
- Semantic diff and three-way merge engine
- Git operations (commit, read tree, changed files)
- Materialization with semantic merge + conflict detection
- Submit service with conflict pre-checking
- Ripple checker + live rebase of shadowed files
- Trunk notification system (file-based, per-agent)
- CLI session management with PTY, Claude Code adapter, session resumption
- Background agent execution with monitoring
- `phantom plan` — AI-driven task decomposition into parallel agents
- `phantom resolve` — AI-driven conflict resolution
- All core CLI commands (init, task, submit, plan, resolve, status, log, changes, rollback, destroy, background, down)
- Integration tests for all major scenarios

### Not Yet Implemented
- macOS NFS overlay fallback
- Incremental symbol index updates (full reparse on each materialization)
- `.phantom/config.toml` configuration file
- Benchmarks

## Glossary

| Term | Definition |
|------|-----------|
| **Changeset** | Atomic unit of work from an agent. Contains semantic operations, test results, lifecycle status. Replaces branches. |
| **Overlay** | FUSE-mounted COW filesystem per agent. Upper = writes, lower = trunk read-through. |
| **Trunk** | `main` branch of the underlying git repo. Single source of truth. |
| **Submit** | Extract semantic operations from an overlay, run semantic merge against trunk, commit if clean (or mark as conflicted), and ripple to other agents. Single CLI step. |
| **Semantic Operation** | Structured change description: AddSymbol, ModifySymbol, DeleteSymbol, etc. |
| **Ripple** | Post-materialization notification to active agents about trunk changes. |
| **Live Rebase** | Auto-merge trunk changes into an agent's upper layer for shadowed files. |
| **Plan** | Multi-domain task decomposition. Each domain gets its own agent overlay. |
| **Content Hash** | BLAKE3 hash of symbol source text for fast change detection. |

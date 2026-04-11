# Phantom Implementation Plan

## Context

Phantom is an event-sourced, semantic-aware version control layer for agentic AI development, built in Rust on top of Git. The repo is currently empty (just README.md + CLAUDE.md spec). This plan breaks the full implementation into 8 independent sections that can be developed in separate worktrees and merged together without conflicts.

The architecture has 6 crates: `phantom-core` (shared types), `phantom-events` (SQLite event store), `phantom-overlay` (FUSE filesystem), `phantom-semantic` (tree-sitter parsing + merge), `phantom-orchestrator` (coordination), and `phantom-cli` (binary).

---

## Dependency Graph

```
                    Section 0 (phantom-core + workspace)
                   /    |    \        \
                  /     |     \        \
          Sec 1     Sec 2    Sec 3    Sec 4
        (events)  (overlay) (semantic) (git-ops)
                  \     |     /        /
                   \    |    /        /
                    Section 5 (orchestrator coordination)
                        |
                   +----+----+
                   |         |
                Sec 6     Sec 7
                (cli)   (integration tests)
```

**Merge order:** Section 0 first. Then Sections 1-4 in parallel. Then Section 5. Then Sections 6-7 in parallel.

---

## File Ownership (Merge Compatibility)

Each section owns a strict, non-overlapping set of files to guarantee zero merge conflicts:

| Section | Owns |
|---------|------|
| 0 | `Cargo.toml`, `crates/phantom-core/**`, all other crate `Cargo.toml` + stub `lib.rs`, `docs/` |
| 1 | `crates/phantom-events/src/**` |
| 2 | `crates/phantom-overlay/src/**` |
| 3 | `crates/phantom-semantic/src/**` |
| 4 | `crates/phantom-orchestrator/src/git.rs`, `crates/phantom-orchestrator/src/error.rs` |
| 5 | `crates/phantom-orchestrator/src/materializer.rs`, `scheduler.rs`, `ripple.rs`, update `lib.rs` |
| 6 | `crates/phantom-cli/src/**` |
| 7 | `tests/**` |

**Section 4 vs 5 conflict mitigation:** Section 0 creates `phantom-orchestrator/src/lib.rs` with all module declarations pre-declared (`pub mod git; pub mod error; pub mod materializer; pub mod scheduler; pub mod ripple;`). Sections 4 and 5 only create their owned files. Alternatively, merge Section 4 before starting Section 5.

---

## Section 0: Workspace Skeleton + phantom-core

**Branch:** `feat/core`
**Must merge first** — every other section depends on it.

### What to build

1. **Workspace `Cargo.toml`** with all 6 crate members and shared workspace dependencies
2. **`phantom-core`** fully implemented — all shared types, traits, errors
3. **Stub `Cargo.toml` + `lib.rs`** for every other crate (so workspace compiles)
4. **`docs/` directory** with architecture.md, semantic-merge.md, event-model.md stubs

### Workspace Cargo.toml

```toml
[workspace]
resolver = "3"
members = [
    "crates/phantom-core",
    "crates/phantom-events",
    "crates/phantom-overlay",
    "crates/phantom-semantic",
    "crates/phantom-orchestrator",
    "crates/phantom-cli",
]

[workspace.package]
edition = "2024"
license = "MIT OR Apache-2.0"
repository = "https://github.com/..."

[workspace.dependencies]
phantom-core = { path = "crates/phantom-core" }
phantom-events = { path = "crates/phantom-events" }
phantom-overlay = { path = "crates/phantom-overlay" }
phantom-semantic = { path = "crates/phantom-semantic" }
phantom-orchestrator = { path = "crates/phantom-orchestrator" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
blake3 = "1"
thiserror = "2"
tracing = "0.1"
tokio = { version = "1", features = ["full"] }
tempfile = "3"
```

### phantom-core files

**`crates/phantom-core/src/lib.rs`** — re-exports all modules.

**`crates/phantom-core/src/id.rs`** — Newtype IDs:
- `ChangesetId(String)` — unique changeset identifier
- `AgentId(String)` — agent identifier
- `EventId(u64)` — auto-incrementing event ID
- `SymbolId(String)` — symbol identity (format: `"scope::name::kind"`)
- `ContentHash([u8; 32])` — BLAKE3 hash with `from_bytes(data) -> Self` and `to_hex() -> String`
- `GitOid([u8; 20])` — git OID as plain bytes (no `git2` dependency). `from_bytes()`, `zero()`, `to_hex()`

All derive: `Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize`.
Key decision: `GitOid` is plain bytes so phantom-core stays git2-free. Conversion impls (`From<git2::Oid>` etc.) live in phantom-orchestrator.

**`crates/phantom-core/src/symbol.rs`**:
```rust
pub enum SymbolKind {
    Function, Struct, Enum, Trait, Impl, Import, Const,
    TypeAlias, Module, Test, Class, Interface, Method,
}

pub struct SymbolEntry {
    pub id: SymbolId,
    pub kind: SymbolKind,
    pub name: String,
    pub scope: String,          // e.g. "crate::handlers"
    pub file: PathBuf,
    pub byte_range: Range<usize>,
    pub content_hash: ContentHash,
}
```

**`crates/phantom-core/src/changeset.rs`**:
```rust
pub enum ChangesetStatus {
    InProgress, Submitted, Merging, Materialized, Conflicted, Dropped,
}

pub enum SemanticOperation {
    AddSymbol { file: PathBuf, symbol: SymbolEntry },
    ModifySymbol { file: PathBuf, old_hash: ContentHash, new_entry: SymbolEntry },
    DeleteSymbol { file: PathBuf, id: SymbolId },
    AddFile { path: PathBuf },
    DeleteFile { path: PathBuf },
    RawDiff { path: PathBuf, patch: String },
}

pub struct TestResult { pub passed: u32, pub failed: u32, pub skipped: u32 }

pub struct Changeset {
    pub id: ChangesetId,
    pub agent_id: AgentId,
    pub task: String,
    pub base_commit: GitOid,
    pub files_touched: Vec<PathBuf>,
    pub operations: Vec<SemanticOperation>,
    pub test_result: Option<TestResult>,
    pub created_at: DateTime<Utc>,
    pub status: ChangesetStatus,
}
```

Design note: The CLAUDE.md spec has many `SemanticOperation` variants (AddFunction, AddStruct, AddTest, etc.). This plan collapses them into `AddSymbol`/`ModifySymbol`/`DeleteSymbol` + file-level ops. The `SymbolEntry.kind` field distinguishes functions from structs from traits. This is simpler, more extensible, and the semantic meaning is fully preserved.

**`crates/phantom-core/src/conflict.rs`**:
```rust
pub enum ConflictKind {
    BothModifiedSymbol,
    ModifyDeleteSymbol,
    BothModifiedDependencyVersion,
    RawTextConflict,
}

pub struct ConflictDetail {
    pub kind: ConflictKind,
    pub file: PathBuf,
    pub symbol_id: Option<SymbolId>,
    pub ours_changeset: ChangesetId,
    pub theirs_changeset: ChangesetId,
    pub description: String,
}
```

**`crates/phantom-core/src/event.rs`**:
```rust
pub enum MergeCheckResult {
    Clean,
    Conflicted(Vec<ConflictDetail>),
}

pub enum EventKind {
    OverlayCreated { base_commit: GitOid },
    OverlayDestroyed,
    FileWritten { path: PathBuf, content_hash: ContentHash },
    FileDeleted { path: PathBuf },
    ChangesetSubmitted { operations: Vec<SemanticOperation> },
    ChangesetMergeChecked { result: MergeCheckResult },
    ChangesetMaterialized { new_commit: GitOid },
    ChangesetConflicted { conflicts: Vec<ConflictDetail> },
    ChangesetDropped { reason: String },
    TrunkAdvanced { old_commit: GitOid, new_commit: GitOid },
    AgentNotified { agent_id: AgentId, changed_symbols: Vec<SymbolId> },
    TestsRun(TestResult),
}

pub struct Event {
    pub id: EventId,
    pub timestamp: DateTime<Utc>,
    pub changeset_id: ChangesetId,
    pub agent_id: AgentId,
    pub kind: EventKind,
}
```

**`crates/phantom-core/src/error.rs`**:
```rust
#[derive(Debug, Error)]
pub enum CoreError {
    ChangesetNotFound(ChangesetId),
    AgentNotFound(AgentId),
    InvalidStatusTransition { from: ChangesetStatus, to: ChangesetStatus },
    Serialization(String),
}
```

**`crates/phantom-core/src/traits.rs`** — Trait interfaces for cross-crate boundaries:

```rust
/// Event store interface. Implemented by phantom-events.
pub trait EventStore: Send + Sync {
    fn append(&self, event: Event) -> Result<EventId, CoreError>;
    fn query_by_changeset(&self, id: &ChangesetId) -> Result<Vec<Event>, CoreError>;
    fn query_by_agent(&self, id: &AgentId) -> Result<Vec<Event>, CoreError>;
    fn query_all(&self) -> Result<Vec<Event>, CoreError>;
    fn query_since(&self, since: DateTime<Utc>) -> Result<Vec<Event>, CoreError>;
}

/// Symbol index interface. Implemented by phantom-semantic.
pub trait SymbolIndex: Send + Sync {
    fn lookup(&self, id: &SymbolId) -> Option<SymbolEntry>;
    fn symbols_in_file(&self, path: &Path) -> Vec<SymbolEntry>;
    fn all_symbols(&self) -> Vec<SymbolEntry>;
    fn update_file(&mut self, path: &Path, symbols: Vec<SymbolEntry>);
    fn remove_file(&mut self, path: &Path);
}

/// Semantic analysis interface. Implemented by phantom-semantic.
pub trait SemanticAnalyzer: Send + Sync {
    fn extract_symbols(&self, path: &Path, content: &[u8]) -> Result<Vec<SymbolEntry>, CoreError>;
    fn diff_symbols(&self, base: &[SymbolEntry], new: &[SymbolEntry]) -> Vec<SemanticOperation>;
    fn three_way_merge(&self, base: &[u8], ours: &[u8], theirs: &[u8], path: &Path) -> Result<MergeResult, CoreError>;
}

pub enum MergeResult {
    Clean(Vec<u8>),
    Conflict(Vec<ConflictDetail>),
}
```

### Stub crates

Each non-core crate gets:
- `Cargo.toml` with correct dependencies (so downstream worktrees don't need to modify it)
- `src/lib.rs` with module declarations and placeholder comments

Example for `phantom-orchestrator/src/lib.rs`:
```rust
pub mod error;
pub mod git;
pub mod materializer;
pub mod scheduler;
pub mod ripple;
```

With each module file containing just `// TODO: implement` so the workspace compiles with warnings but no errors.

### Tests
- Serde JSON round-trip for all types
- `ContentHash::from_bytes` determinism
- `GitOid` construction and hex conversion
- `SymbolId` format validation

### Acceptance criteria
- `cargo build` succeeds for the entire workspace
- `cargo test -p phantom-core` passes
- All crate stubs compile (even if empty)

---

## Section 1: phantom-events (Event Store)

**Branch:** `feat/events`
**Depends on:** Section 0
**Parallel with:** Sections 2, 3, 4

### What to build

SQLite-backed event store implementing `phantom_core::traits::EventStore`.

### Files

**`crates/phantom-events/src/error.rs`**:
- `EventStoreError` with variants: `Sqlite(rusqlite::Error)`, `Serialization(serde_json::Error)`, `Core(CoreError)`

**`crates/phantom-events/src/store.rs`**:
- `SqliteEventStore` struct wrapping `rusqlite::Connection`
- `open(path: &Path) -> Result<Self>` — opens DB, enables WAL mode, runs migrations
- `in_memory() -> Result<Self>` — for testing
- `ensure_schema(&self)` — creates tables + indexes
- Implements `phantom_core::traits::EventStore`

SQLite schema:
```sql
CREATE TABLE events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp    TEXT NOT NULL,
    changeset_id TEXT NOT NULL,
    agent_id     TEXT NOT NULL,
    kind         TEXT NOT NULL,  -- JSON-serialized EventKind
    dropped      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_events_changeset ON events(changeset_id);
CREATE INDEX idx_events_agent ON events(agent_id);
CREATE INDEX idx_events_timestamp ON events(timestamp);
```

Key detail: `EventKind` is stored as a JSON text column. This avoids complex normalized schemas while keeping the append-only pattern simple. Queries on `changeset_id` and `agent_id` use indexed columns.

WAL mode setup:
```rust
conn.pragma_update(None, "journal_mode", "WAL")?;
conn.busy_timeout(Duration::from_secs(5))?;
conn.pragma_update(None, "foreign_keys", "ON")?;
```

**`crates/phantom-events/src/query.rs`**:
- `EventQuery` struct with optional filters: `agent_id`, `changeset_id`, `symbol_id`, `since`, `limit`
- `SqliteEventStore::query(&self, q: &EventQuery) -> Result<Vec<Event>>`
- `SqliteEventStore::mark_dropped(&self, changeset_id: &ChangesetId) -> Result<u64>` — soft-deletes for rollback

**`crates/phantom-events/src/replay.rs`**:
- `ReplayEngine<'a>` holding a reference to `SqliteEventStore`
- `materialized_changesets(&self) -> Result<Vec<ChangesetId>>` — non-dropped, in order
- `changesets_after(&self, id: &ChangesetId) -> Result<Vec<ChangesetId>>` — for rollback replay

**`crates/phantom-events/src/projection.rs`**:
- `Projection` struct with `HashMap<ChangesetId, Changeset>`
- `from_events(events: &[Event]) -> Self` — replays events to derive current changeset states
- `changeset(&self, id: &ChangesetId) -> Option<&Changeset>`
- `active_agents(&self) -> Vec<AgentId>`
- `pending_changesets(&self) -> Vec<&Changeset>` — status == Submitted

### Tests
- Round-trip: append 10 events, query all, query by changeset, query by agent, verify counts and content
- Mark-dropped: append events for 3 changesets, drop one, verify query excludes dropped
- Projection: feed event stream, verify changeset state machine transitions
- Replay engine: verify `changesets_after` ordering
- Concurrent reads: open read connection while writing (WAL mode validation)
- Performance: 1000 events append + query in reasonable time

### Acceptance criteria
- `cargo test -p phantom-events` passes
- All `EventStore` trait methods work correctly
- WAL mode is active (verified via pragma query in test)

---

## Section 2: phantom-overlay (FUSE Overlay Filesystem)

**Branch:** `feat/overlay`
**Depends on:** Section 0
**Parallel with:** Sections 1, 3, 4

### What to build

Copy-on-write overlay filesystem with FUSE mount support. Key insight: `OverlayLayer` contains all COW logic and is testable without FUSE. `PhantomFs` is a thin adapter.

### Files

**`crates/phantom-overlay/src/error.rs`**:
- `OverlayError` with variants: `Io`, `Fuse`, `NotFound(AgentId)`, `AlreadyExists(AgentId)`, `InodeNotFound(u64)`

**`crates/phantom-overlay/src/layer.rs`** — the core COW logic:
```rust
pub struct OverlayLayer {
    lower: PathBuf,              // trunk working tree (read-only)
    upper: PathBuf,              // agent's write layer
    whiteouts: HashSet<PathBuf>, // tracks deleted files
}
```

Methods:
- `new(lower: PathBuf, upper: PathBuf) -> Result<Self>` — creates upper dir if needed
- `read_file(&self, rel_path: &Path) -> Result<Vec<u8>>` — upper first, then lower
- `write_file(&self, rel_path: &Path, data: &[u8]) -> Result<()>` — always upper
- `delete_file(&self, rel_path: &Path) -> Result<()>` — whiteout in upper
- `read_dir(&self, rel_path: &Path) -> Result<Vec<DirEntry>>` — merge upper + lower, exclude whiteouts
- `exists(&self, rel_path: &Path) -> bool`
- `getattr(&self, rel_path: &Path) -> Result<Metadata>`
- `modified_files(&self) -> Result<Vec<PathBuf>>` — walk upper layer, list all files (for changeset extraction)
- `update_lower(&mut self, new_lower: PathBuf)` — trunk advanced, update pointer

Whiteout implementation: a `.phantom_whiteout` file or a `HashSet` in memory (persisted to `upper/.whiteouts.json` on drop).

**`crates/phantom-overlay/src/fuse_fs.rs`** — FUSE adapter:
```rust
pub struct PhantomFs {
    layer: OverlayLayer,
    agent_id: AgentId,
    inodes: InodeTable,
}
```

Implements `fuser::Filesystem` for these operations:
- `init`, `destroy`
- `lookup` — resolve name in parent dir to inode
- `getattr` — file metadata
- `readdir` — list directory
- `open`, `read`, `write`, `release` — file I/O
- `create` — new file
- `unlink` — delete file (whiteout)
- `setattr` — chmod/chown/truncate
- `mkdir`, `rmdir` — directory operations

`InodeTable`: bidirectional `HashMap<u64, PathBuf>` + `HashMap<PathBuf, u64>` with atomic counter for allocation. Root inode = 1.

**`crates/phantom-overlay/src/manager.rs`**:
```rust
pub struct OverlayManager {
    phantom_dir: PathBuf,
    active_overlays: HashMap<AgentId, MountHandle>,
}

pub struct MountHandle {
    pub agent_id: AgentId,
    pub mount_point: PathBuf,
    pub upper_dir: PathBuf,
    session: Option<fuser::BackgroundSession>, // None in no-mount mode
}
```

Methods:
- `new(phantom_dir: PathBuf) -> Self`
- `create_overlay(&mut self, agent_id: AgentId, trunk_path: &Path) -> Result<&MountHandle>`
- `destroy_overlay(&mut self, agent_id: &AgentId) -> Result<()>` — unmount + cleanup
- `list_overlays(&self) -> Vec<&MountHandle>`
- `upper_dir(&self, agent_id: &AgentId) -> Result<&Path>`
- `notify_trunk_advanced(&mut self, new_trunk_path: &Path)` — update all overlays' lower layer

**`crates/phantom-overlay/src/trunk_view.rs`**:
- `TrunkView` wrapping a `PathBuf` to the working tree
- `read_file`, `list_dir`, `file_attr` — simple passthrough to std::fs

### Tests
- **OverlayLayer unit tests (no FUSE needed):**
  - Write to upper, read back — verify content
  - Read file only in lower — verify pass-through
  - File in both layers — verify upper wins
  - Delete file (whiteout) — verify not visible in reads or dir listing
  - `modified_files()` returns exactly written files
  - `update_lower()` — change lower pointer, verify new reads fall through to new lower
  - Directory merging — files from both layers appear in `read_dir`
- **FUSE integration tests (gated behind `#[cfg(feature = "fuse-tests")]`):**
  - Mount overlay, write file via std::fs through mount point, read back
  - Mount overlay, verify lower-layer files visible
  - Write through FUSE, check `modified_files()` from layer

### Acceptance criteria
- `OverlayLayer` tests pass on any OS (no FUSE required)
- On Linux with FUSE: full mount/read/write/unmount cycle works
- `modified_files()` accurately reflects agent's changes

---

## Section 3: phantom-semantic (Parsing + Symbol Extraction + Merge)

**Branch:** `feat/semantic`
**Depends on:** Section 0
**Parallel with:** Sections 1, 2, 4

### What to build

Tree-sitter based parsing, symbol extraction, semantic diff, and three-way merge. Implements `SymbolIndex` and `SemanticAnalyzer` traits from phantom-core.

### Prior art informing the design

- **Weave** (entity-level matching): Match symbols by composite identity key `(name, kind, scope)`. This is simpler and more robust than AST node-position matching for our use case.
- **Mergiraf** (PCS triples): Study for file reconstruction after merge — how to interleave changed and unchanged regions.
- **Difftastic** (structural diff): Inspiration for cost-based matching, but our entity-level approach is simpler.

### Files

**`crates/phantom-semantic/src/error.rs`**:
- `SemanticError`: `ParseError { path, detail }`, `UnsupportedLanguage { path }`, `Io(io::Error)`

**`crates/phantom-semantic/src/languages/mod.rs`**:
```rust
pub trait LanguageExtractor: Send + Sync {
    fn language(&self) -> tree_sitter::Language;
    fn extensions(&self) -> &[&str];
    fn extract_symbols(&self, tree: &tree_sitter::Tree, source: &[u8], file_path: &Path) -> Vec<SymbolEntry>;
}
```

**`crates/phantom-semantic/src/languages/rust.rs`**:
- `RustExtractor` implementing `LanguageExtractor`
- Walks tree-sitter CST for node kinds: `function_item`, `struct_item`, `enum_item`, `trait_item`, `impl_item`, `use_declaration`, `const_item`, `type_item`, `mod_item`, `macro_definition`
- For each: extracts name (from `name` field), computes scope from parent module path, hashes body bytes with BLAKE3
- Generates `SymbolId` as `"{scope}::{name}::{kind}"`

**`crates/phantom-semantic/src/languages/typescript.rs`**, **`python.rs`**, **`go.rs`**:
- Same pattern, different node kinds per grammar
- TypeScript: `function_declaration`, `class_declaration`, `interface_declaration`, `method_definition`, `import_statement`, `export_statement`, `type_alias_declaration`
- Python: `function_definition`, `class_definition`, `import_statement`, `import_from_statement`
- Go: `function_declaration`, `method_declaration`, `type_declaration`, `import_declaration`
- **Phase 2 priority: implement Rust first.** Other languages are additive and can be added later.

**`crates/phantom-semantic/src/parser.rs`**:
```rust
pub struct Parser {
    languages: HashMap<String, Box<dyn LanguageExtractor>>,
}
```
- `new() -> Self` — registers all built-in language extractors
- `parse_file(&self, path: &Path, content: &[u8]) -> Result<Vec<SymbolEntry>>` — detect language from extension, parse, extract
- `detect_language(&self, path: &Path) -> Option<&str>`
- `supports_language(&self, path: &Path) -> bool`

**`crates/phantom-semantic/src/index.rs`**:
```rust
pub struct InMemorySymbolIndex {
    symbols: HashMap<SymbolId, SymbolEntry>,
    file_to_symbols: HashMap<PathBuf, Vec<SymbolId>>,
    indexed_at: GitOid,
}
```
- Implements `phantom_core::traits::SymbolIndex`
- `build_from_directory(root: &Path, parser: &Parser, commit: GitOid) -> Result<Self>` — walk all files, parse each, populate index

**`crates/phantom-semantic/src/diff.rs`**:
```rust
pub fn diff_symbols(base: &[SymbolEntry], new: &[SymbolEntry], file: &Path) -> Vec<SemanticOperation>
```
Algorithm (Weave-style entity matching):
1. Build lookup by `(name, kind, scope)` for both base and new
2. For each in new not in base → `AddSymbol`
3. For each in both, different `content_hash` → `ModifySymbol`
4. For each in base not in new → `DeleteSymbol`

**`crates/phantom-semantic/src/merge.rs`**:
```rust
pub struct SemanticMerger {
    parser: Parser,
}
```
- Implements `phantom_core::traits::SemanticAnalyzer`
- `three_way_merge(base, ours, theirs, path)` algorithm:
  1. Parse all three versions, extract symbols
  2. Compute `diff_symbols(base, ours)` and `diff_symbols(base, theirs)`
  3. Classify operations:
     - Both add different symbols → **auto-merge** (no conflict)
     - Both add different fields to same struct → **auto-merge**
     - Both modify same symbol → **CONFLICT**
     - One modifies, other deletes same symbol → **CONFLICT**
     - Both add same import → **auto-deduplicate**
     - Additive insertions to same collection → **auto-merge**
  4. For clean merges: reconstruct file by applying non-conflicting changes to base source
  5. For conflicts: return `MergeResult::Conflict(details)`

File reconstruction strategy:
- Sort symbols by `byte_range.start`
- Walk through base file, replacing symbol regions with the appropriate version (ours/theirs/base)
- Interstitial content (whitespace, comments between symbols) preserved from base unless a symbol's byte range overlaps

Fallback: for files the semantic layer can't parse (no grammar), fall back to line-based three-way merge via `similar` crate or `diffy` crate.

### Tests
- **Symbol extraction (per language):**
  - Parse Rust file with 3 functions → verify 3 SymbolEntry with correct names, kinds, scopes, byte_ranges
  - Parse file with nested impl blocks → verify methods get correct scope
  - Parse file with imports → verify Import symbols
  - Empty file → empty vec
  - Syntax error in file → partial extraction (tree-sitter is error-tolerant)

- **Diff tests:**
  - Base `{f1, f2}`, new `{f1_modified, f2, f3}` → `[ModifySymbol(f1), AddSymbol(f3)]`
  - Base `{f1, f2}`, new `{f1}` → `[DeleteSymbol(f2)]`
  - Identical files → empty diff

- **Three-way merge tests:**
  - Both add different functions to same file → Clean merge, output contains both
  - Both modify same function → Conflict
  - One adds function, other modifies different function → Clean merge
  - One deletes function, other modifies same → Conflict
  - Both add same import → Clean merge (dedup)
  - Disjoint changes in different files → Clean (trivially)

- **Index tests:**
  - Build from directory, verify all symbols found
  - Update file, verify index reflects changes
  - Remove file, verify symbols removed

### Acceptance criteria
- `cargo test -p phantom-semantic` passes
- Rust symbol extraction handles: functions, structs, enums, traits, impls, use statements, consts, type aliases, modules, tests
- Three-way merge correctly auto-merges disjoint symbol changes
- Three-way merge correctly detects same-symbol conflicts
- Unsupported file types fall back to text merge gracefully

---

## Section 4: phantom-orchestrator — Git Operations

**Branch:** `feat/git-ops`
**Depends on:** Section 0
**Parallel with:** Sections 1, 2, 3

### What to build

The `git.rs` and `error.rs` modules of phantom-orchestrator. Pure git operations via `git2`.

### Files

**`crates/phantom-orchestrator/src/error.rs`**:
```rust
pub enum OrchestratorError {
    Git(git2::Error),
    EventStore(String),
    Semantic(String),
    Overlay(String),
    Io(io::Error),
    MaterializationFailed(String),
}
```

**`crates/phantom-orchestrator/src/git.rs`**:
```rust
pub struct GitOps {
    repo: git2::Repository,
}
```

Methods:
- `open(repo_path: &Path) -> Result<Self>` — open existing repo
- `head_oid(&self) -> Result<GitOid>` — current HEAD commit
- `read_file_at_commit(&self, oid: &GitOid, path: &Path) -> Result<Vec<u8>>` — read blob from tree
- `list_files_at_commit(&self, oid: &GitOid) -> Result<Vec<PathBuf>>` — walk tree recursively
- `commit_overlay_changes(&self, upper_dir: &Path, trunk_path: &Path, message: &str, author: &str) -> Result<GitOid>` — copy upper files to working tree, stage, commit
- `reset_to_commit(&self, oid: &GitOid) -> Result<()>` — hard reset for rollback
- `changed_files(&self, from: &GitOid, to: &GitOid) -> Result<Vec<PathBuf>>` — diff two commits
- `text_merge(&self, base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<MergeResult>` — line-based three-way merge fallback

Conversion helpers (in this crate only):
```rust
impl From<git2::Oid> for GitOid { ... }
impl TryFrom<GitOid> for git2::Oid { ... }
```

### Tests
All tests use `tempfile::TempDir` + `git2::Repository::init()`:
- Init repo, commit a file, `head_oid()` returns non-zero OID
- `read_file_at_commit`: commit file, read back at that commit, verify content
- `list_files_at_commit`: commit 3 files, verify all 3 listed
- `commit_overlay_changes`: set up upper dir with modified files, commit, verify HEAD advanced and file content matches
- `changed_files`: two commits touching different files, verify diff is correct
- `reset_to_commit`: commit twice, reset to first, verify HEAD
- `text_merge`: base "a\nb\nc", ours "a\nb\nd", theirs "a\ne\nc" → clean merge "a\ne\nd"
- `text_merge` with conflict: both modify same line → Conflict

### Acceptance criteria
- `cargo test -p phantom-orchestrator` passes (git module tests)
- Full round-trip: init repo → commit → read → verify
- Conversion between `GitOid` and `git2::Oid` is lossless

---

## Section 5: phantom-orchestrator — Materializer, Scheduler, Ripple

**Branch:** `feat/orchestrator`
**Depends on:** Sections 0, 1, 3, 4

### What to build

The coordination modules that wire everything together. Uses traits from phantom-core for testability with mocks.

### Files

**`crates/phantom-orchestrator/src/materializer.rs`**:
```rust
pub struct Materializer<S: EventStore, A: SemanticAnalyzer> {
    git: GitOps,
    store: S,
    analyzer: A,
}

pub enum MaterializeResult {
    Success { new_commit: GitOid },
    Conflict { details: Vec<ConflictDetail> },
}
```

`materialize(&self, changeset: &Changeset, upper_dir: &Path) -> Result<MaterializeResult>`:
1. Get current HEAD from git
2. If HEAD == changeset.base_commit → no trunk advancement, apply directly
3. If HEAD != base_commit → trunk advanced, need three-way merge:
   a. For each file in changeset.files_touched:
      - Read base version (at base_commit), ours (at HEAD), theirs (from upper_dir)
      - Call `analyzer.three_way_merge(base, ours, theirs, path)`
      - If any conflict, return `MaterializeResult::Conflict`
   b. For files not in trunk (new files) → apply directly
4. Copy all merged files to working tree
5. `git.commit_overlay_changes(...)` → new commit OID
6. `store.append(Event { kind: ChangesetMaterialized { new_commit } })`
7. Return `MaterializeResult::Success { new_commit }`

**`crates/phantom-orchestrator/src/scheduler.rs`**:
```rust
pub struct Scheduler<S: EventStore> {
    store: S,
    queue: VecDeque<Changeset>,
}
```
- `enqueue(&mut self, changeset: Changeset)`
- `next(&mut self) -> Option<Changeset>` — FIFO for now, priority later
- `pending(&self) -> &[Changeset]`
- `remove(&mut self, id: &ChangesetId) -> Option<Changeset>`

**`crates/phantom-orchestrator/src/ripple.rs`**:
```rust
pub struct RippleChecker<I: SymbolIndex> {
    index: I,
}
```
- `check_ripple(changed_files, active_agents: &[(AgentId, Vec<PathBuf>)]) -> HashMap<AgentId, Vec<SymbolId>>`
  - For each active agent, check if any of their touched files overlap with changed_files
  - If overlap, look up which symbols in those files changed
  - Return map of agent → affected symbols

### Tests

**With mocks (unit tests, no real deps needed):**
- Materializer: mock analyzer returns `Clean` → verify git commit happens, event appended
- Materializer: mock analyzer returns `Conflict` → verify no commit, conflict returned
- Materializer: trunk not advanced (HEAD == base) → direct apply, no merge needed
- Scheduler: enqueue 3, dequeue in FIFO order
- Scheduler: remove from middle
- Ripple: two agents, changed file overlaps agent B but not A → only B in result

**Integration tests (require Sections 1, 3, 4 merged):**
- Two agents, disjoint files, both materialize cleanly
- Two agents, same file different symbols, auto-merge succeeds
- Two agents, same symbol, conflict detected

### Acceptance criteria
- All mock-based unit tests pass
- Integration tests pass after merging with Sections 1, 3, 4

---

## Section 6: phantom-cli (Binary Crate)

**Branch:** `feat/cli`
**Depends on:** All other sections

### What to build

The `phantom` command-line tool with all subcommands.

### Files

**`crates/phantom-cli/src/main.rs`**:
```rust
#[derive(clap::Parser)]
#[command(name = "phantom", about = "Semantic version control for agentic AI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    Up,
    Dispatch(DispatchArgs),
    Submit(SubmitArgs),
    Status,
    Materialize(MaterializeArgs),
    Rollback(RollbackArgs),
    Log(LogArgs),
    Destroy(DestroyArgs),
}
```

**`crates/phantom-cli/src/commands/up.rs`** — `phantom up`:
- Verify current dir is a git repo (check for `.git/`)
- Create `.phantom/` directory structure:
  - `.phantom/events.db` — initialize SQLite event store
  - `.phantom/overlays/` — overlay root
  - `.phantom/config.toml` — default config
- Print success message with repo info

**`crates/phantom-cli/src/commands/dispatch.rs`** — `phantom dispatch --agent <id> --task <desc>`:
- Validate agent ID is unique (not already active)
- Create overlay via `OverlayManager::create_overlay()`
- Create initial `Changeset` with `InProgress` status
- Append `OverlayCreated` event
- Print overlay mount point path

**`crates/phantom-cli/src/commands/submit.rs`** — `phantom submit --agent <id>`:
- Get agent's upper dir from `OverlayManager`
- List `modified_files()` from overlay layer
- For each modified file, run `SemanticAnalyzer::extract_symbols()` on both base and agent versions
- Compute `diff_symbols()` to get `Vec<SemanticOperation>`
- Update changeset status to `Submitted`
- Append `ChangesetSubmitted` event
- Print summary of operations

**`crates/phantom-cli/src/commands/materialize.rs`** — `phantom materialize --changeset <id>`:
- Load changeset from projection
- Call `Materializer::materialize()`
- On success: print new commit OID, run ripple check, notify affected agents
- On conflict: print conflict details, set changeset status to `Conflicted`

**`crates/phantom-cli/src/commands/rollback.rs`** — `phantom rollback --changeset <id>`:
- Mark changeset events as dropped
- Get commit before this changeset's materialization
- `git.reset_to_commit()`
- Identify downstream changesets via `ReplayEngine::changesets_after()`
- For each downstream: attempt re-materialize
- Print results (which replayed cleanly, which need re-dispatch)

**`crates/phantom-cli/src/commands/status.rs`** — `phantom status`:
- Build projection from event log
- Display: active overlays, pending changesets, trunk HEAD, event count
- Formatted table output

**`crates/phantom-cli/src/commands/log.rs`** — `phantom log [--agent <id>] [--changeset <id>] [--symbol <id>] [--since <duration>]`:
- Build `EventQuery` from args
- Execute query
- Display events in chronological order with formatting

**`crates/phantom-cli/src/commands/destroy.rs`** — `phantom destroy --agent <id>`:
- Unmount and destroy overlay
- Append `OverlayDestroyed` event
- Print confirmation

### Shared CLI utilities

**`crates/phantom-cli/src/context.rs`**:
```rust
/// Loads the Phantom context for the current directory.
pub struct PhantomContext {
    pub phantom_dir: PathBuf,
    pub git_ops: GitOps,
    pub event_store: SqliteEventStore,
    pub overlay_manager: OverlayManager,
    pub semantic: SemanticMerger,
}

impl PhantomContext {
    pub fn load() -> Result<Self> { /* find .phantom/ dir, open connections */ }
}
```

### Tests
- Use `assert_cmd` crate for CLI integration testing
- `phantom up` in temp git repo → verify `.phantom/` structure
- `phantom dispatch` → verify overlay dir created
- `phantom status` → verify output format
- Full workflow: up → dispatch x2 → submit x2 → materialize x2 → log → verify

### Acceptance criteria
- All subcommands parse correctly
- `phantom up` creates correct directory structure
- `phantom status` displays meaningful output
- Full workflow completes on Linux with FUSE

---

## Section 7: Integration Tests + Fixtures

**Branch:** `feat/integration-tests`
**Depends on:** All other sections

### What to build

End-to-end integration tests covering the core scenarios from CLAUDE.md.

### Files

All in `tests/integration/`:

**`two_agents_disjoint.rs`** — `test_two_agents_disjoint_files_auto_merges`:
1. Create git repo with `src/a.rs` (has `fn alpha()`) and `src/b.rs` (has `fn beta()`)
2. Set up Phantom (event store, overlay manager, semantic analyzer)
3. Create overlay for agent-a and agent-b
4. Agent-a writes new function to `src/a.rs`
5. Agent-b writes new function to `src/b.rs`
6. Submit both as changesets
7. Materialize agent-a's changeset → success
8. Materialize agent-b's changeset → success (no overlap)
9. Verify trunk has all 4 functions

**`two_agents_same_file.rs`** — `test_two_agents_same_file_different_symbols_auto_merges`:
1. Create repo with `src/handlers.rs` containing `fn handle_login()`
2. Agent-a adds `fn handle_register()`
3. Agent-b adds `fn handle_admin()`
4. Both materialize → auto-merge succeeds
5. Verify trunk has all 3 functions

**`two_agents_same_symbol.rs`** — `test_two_agents_same_symbol_conflicts`:
1. Create repo with `src/lib.rs` containing `fn compute()`
2. Agent-a modifies `compute()` body
3. Agent-b also modifies `compute()` body differently
4. Agent-a materializes → success
5. Agent-b materializes → `MaterializeResult::Conflict`
6. Verify conflict detail points to `compute()`

**`materialize_and_ripple.rs`** — `test_ripple_notification_after_materialize`:
1. Create repo, dispatch agent-a and agent-b
2. Both touch `src/shared.rs` (different symbols)
3. Agent-a materializes
4. Run ripple check → agent-b is notified about changed symbols in `shared.rs`
5. Verify agent-b's overlay lower layer reflects new trunk

**`rollback_replay.rs`** — `test_rollback_middle_changeset_replays_downstream`:
1. Create repo, materialize cs-001, cs-002, cs-003 in sequence
2. Rollback cs-002
3. Verify trunk reset to pre-cs-002 commit
4. Verify cs-003 is identified for replay
5. Re-materialize cs-003 → if no dependency on cs-002, succeeds

**`event_log_query.rs`** — `test_event_log_queries`:
1. Run a full workflow generating 20+ events
2. Query by agent → correct subset
3. Query by changeset → correct subset
4. Query since timestamp → correct subset
5. Verify event ordering is chronological

### Fixtures

**`tests/fixtures/sample_repo/`**:
- Pre-built Rust source files used across tests
- `src/lib.rs`, `src/handlers.rs`, `src/models.rs` with realistic function/struct definitions
- Helper function `create_test_repo(dir: &Path)` that initializes git and copies fixtures

### Test infrastructure

**`tests/common/mod.rs`** (or `tests/helpers.rs`):
```rust
pub fn setup_phantom(dir: &Path) -> PhantomTestContext { ... }
pub fn create_test_repo(dir: &Path) -> git2::Repository { ... }
pub fn write_through_overlay(upper: &Path, rel_path: &Path, content: &[u8]) { ... }
```

Note: Integration tests that need FUSE are gated behind `#[cfg(feature = "fuse-tests")]`. Tests that bypass FUSE (using `OverlayLayer` directly) run on all platforms.

### Acceptance criteria
- All 6 integration test scenarios pass
- Tests complete in under 60 seconds total
- No leaked temp directories or mount points
- Tests are deterministic (no timing-dependent assertions)

---

## Verification Plan

After all sections are merged:

1. **Build:** `cargo build` — entire workspace compiles
2. **Unit tests:** `cargo test` — all crate-level tests pass
3. **Integration tests:** `cargo test --test '*'` — all integration tests pass
4. **Lint:** `cargo clippy -- -D warnings` — no warnings
5. **Format:** `cargo fmt --check` — properly formatted
6. **Doc:** `cargo doc --no-deps` — documentation builds
7. **Manual E2E test:**
   ```bash
   cd /tmp && git init test-repo && cd test-repo
   echo 'fn main() {}' > src/main.rs && git add . && git commit -m "init"
   phantom up
   phantom dispatch --agent a --task "add feature"
   # Write to overlay
   phantom submit --agent a
   phantom materialize --changeset cs-0001
   phantom status
   phantom log
   ```

---

## Coverage Verification Against CLAUDE.md

| CLAUDE.md Requirement | Covered In |
|----------------------|------------|
| Changeset model (replaces branches) | Section 0 (types), Section 5 (lifecycle) |
| FUSE overlay per agent | Section 2 |
| Copy-on-write (upper/lower) | Section 2 (layer.rs) |
| Trunk update propagation | Section 2 (update_lower), Section 5 (ripple) |
| tree-sitter parsing | Section 3 (parser.rs, languages/) |
| Symbol extraction | Section 3 (languages/*.rs) |
| SymbolIndex (live map) | Section 3 (index.rs) |
| Semantic diff | Section 3 (diff.rs) |
| Three-way semantic merge | Section 3 (merge.rs) |
| Conflict categories table | Section 3 (merge.rs logic) |
| Event log (append-only, SQLite WAL) | Section 1 |
| Event types | Section 0 (event.rs) |
| Rollback via replay | Section 1 (replay.rs), Section 5 (materializer), Section 6 (rollback cmd) |
| Event queries | Section 1 (query.rs), Section 6 (log cmd) |
| CLI: phantom up | Section 6 |
| CLI: phantom dispatch | Section 6 |
| CLI: phantom submit | Section 6 |
| CLI: phantom materialize | Section 6 |
| CLI: phantom rollback | Section 6 |
| CLI: phantom status | Section 6 |
| CLI: phantom log | Section 6 |
| CLI: phantom destroy | Section 6 |
| Orchestrator/scheduler | Section 5 |
| Materializer | Section 5 |
| Ripple notifications | Section 5 |
| Git operations | Section 4 |
| Rust language support | Section 3 |
| TypeScript language support | Section 3 (additive) |
| Python language support | Section 3 (additive) |
| Go language support | Section 3 (additive) |
| Integration test: disjoint files | Section 7 |
| Integration test: same file diff symbols | Section 7 |
| Integration test: same symbol conflict | Section 7 |
| Integration test: ripple | Section 7 |
| Integration test: rollback | Section 7 |
| Integration test: event log query | Section 7 |
| Coding conventions (thiserror, tracing, newtypes, doc comments) | All sections |
| macOS NFS fallback | Deferred to Phase 5 (documented but not in MVP) |
| Config file (.phantom/config.toml) | Section 6 (up command creates it) |

**Deferred items (not in this plan, as per CLAUDE.md Phase 5):**
- macOS NFS overlay fallback
- Performance: incremental index updates
- Agent wrapper scripts for Claude Code, Cursor, Codex
- Benchmarks
- Detailed config.toml options beyond defaults

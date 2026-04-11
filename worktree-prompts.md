# Worktree Prompts

Copy-paste these prompts into a fresh Claude Code session in each worktree.

---

## Section 0: `feat/core` — Workspace Skeleton + phantom-core

```
Read CLAUDE.md and plan.md thoroughly. You are implementing Section 0 of the plan: the workspace skeleton and phantom-core crate.

This is the foundational section — every other crate depends on it. You must get the types, traits, and project structure exactly right because 6 other parallel worktrees will code against these interfaces.

## What to build

1. **Workspace root `Cargo.toml`** with all 6 crate members under `crates/` and shared workspace dependencies:
   - serde 1 (with derive), serde_json 1, chrono 0.4 (with serde), blake3 1, thiserror 2, tracing 0.1, tokio 1 (full), tempfile 3
   - Workspace paths for all phantom crates
   - `[workspace.package]` with edition = "2024", license = "MIT OR Apache-2.0"

2. **`crates/phantom-core/`** — fully implemented with these files:

   **`src/id.rs`** — Newtype IDs, all deriving Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize:
   - `ChangesetId(String)` — unique changeset identifier
   - `AgentId(String)` — agent identifier
   - `EventId(u64)` — auto-incrementing event ID
   - `SymbolId(String)` — symbol identity (format: "scope::name::kind")
   - `ContentHash([u8; 32])` — BLAKE3 hash. Methods: `from_bytes(data: &[u8]) -> Self` (using blake3::hash), `to_hex() -> String`
   - `GitOid([u8; 20])` — plain 20-byte git OID (NO git2 dependency!). Methods: `from_bytes([u8; 20]) -> Self`, `zero() -> Self`, `to_hex() -> String`
   - Add Display impls for all ID types

   **`src/symbol.rs`**:
   - `SymbolKind` enum: Function, Struct, Enum, Trait, Impl, Import, Const, TypeAlias, Module, Test, Class, Interface, Method
   - `SymbolEntry` struct: id (SymbolId), kind (SymbolKind), name (String), scope (String), file (PathBuf), byte_range (Range<usize>), content_hash (ContentHash)

   **`src/changeset.rs`**:
   - `ChangesetStatus` enum: InProgress, Submitted, Merging, Materialized, Conflicted, Dropped
   - `SemanticOperation` enum: AddSymbol { file, symbol }, ModifySymbol { file, old_hash, new_entry }, DeleteSymbol { file, id }, AddFile { path }, DeleteFile { path }, RawDiff { path, patch }
   - `TestResult` struct: passed (u32), failed (u32), skipped (u32)
   - `Changeset` struct: id, agent_id, task, base_commit (GitOid), files_touched (Vec<PathBuf>), operations (Vec<SemanticOperation>), test_result (Option<TestResult>), created_at (DateTime<Utc>), status (ChangesetStatus)

   **`src/conflict.rs`**:
   - `ConflictKind` enum: BothModifiedSymbol, ModifyDeleteSymbol, BothModifiedDependencyVersion, RawTextConflict
   - `ConflictDetail` struct: kind, file (PathBuf), symbol_id (Option<SymbolId>), ours_changeset (ChangesetId), theirs_changeset (ChangesetId), description (String)

   **`src/event.rs`**:
   - `MergeCheckResult` enum: Clean, Conflicted(Vec<ConflictDetail>)
   - `EventKind` enum with variants: OverlayCreated { base_commit: GitOid }, OverlayDestroyed, FileWritten { path: PathBuf, content_hash: ContentHash }, FileDeleted { path: PathBuf }, ChangesetSubmitted { operations: Vec<SemanticOperation> }, ChangesetMergeChecked { result: MergeCheckResult }, ChangesetMaterialized { new_commit: GitOid }, ChangesetConflicted { conflicts: Vec<ConflictDetail> }, ChangesetDropped { reason: String }, TrunkAdvanced { old_commit: GitOid, new_commit: GitOid }, AgentNotified { agent_id: AgentId, changed_symbols: Vec<SymbolId> }, TestsRun(TestResult)
   - `Event` struct: id (EventId), timestamp (DateTime<Utc>), changeset_id (ChangesetId), agent_id (AgentId), kind (EventKind)

   **`src/error.rs`**:
   - `CoreError` enum using thiserror: ChangesetNotFound(ChangesetId), AgentNotFound(AgentId), InvalidStatusTransition { from: ChangesetStatus, to: ChangesetStatus }, Serialization(String)

   **`src/traits.rs`** — trait interfaces other crates implement:
   - `EventStore: Send + Sync` with methods: append(event) -> Result<EventId, CoreError>, query_by_changeset(id) -> Result<Vec<Event>>, query_by_agent(id) -> Result<Vec<Event>>, query_all() -> Result<Vec<Event>>, query_since(since: DateTime<Utc>) -> Result<Vec<Event>>
   - `SymbolIndex: Send + Sync` with methods: lookup(id) -> Option<SymbolEntry>, symbols_in_file(path) -> Vec<SymbolEntry>, all_symbols() -> Vec<SymbolEntry>, update_file(path, symbols), remove_file(path)
   - `SemanticAnalyzer: Send + Sync` with methods: extract_symbols(path, content) -> Result<Vec<SymbolEntry>>, diff_symbols(base, new) -> Vec<SemanticOperation>, three_way_merge(base, ours, theirs, path) -> Result<MergeResult>
   - `MergeResult` enum: Clean(Vec<u8>), Conflict(Vec<ConflictDetail>)

   **`src/lib.rs`** — pub mod and re-export everything.

3. **Stub Cargo.toml + src/lib.rs for every other crate** so the whole workspace compiles. Each stub lib.rs should declare its modules (even if the module files just contain `// TODO: implement`). The Cargo.toml files must have the CORRECT final dependencies so other worktrees don't need to modify them:

   - `phantom-events`: phantom-core, rusqlite (features: bundled), serde_json, chrono, tracing, thiserror
   - `phantom-overlay`: phantom-core, fuser, tracing, thiserror, libc
   - `phantom-semantic`: phantom-core, tree-sitter, tree-sitter-rust, tree-sitter-typescript, tree-sitter-python, tree-sitter-go, blake3, tracing, thiserror
   - `phantom-orchestrator`: phantom-core, phantom-events, phantom-semantic, git2, tracing, thiserror, tokio
   - `phantom-cli`: phantom-core, phantom-events, phantom-overlay, phantom-semantic, phantom-orchestrator, clap (features: derive), tokio, tracing, tracing-subscriber, anyhow

   For phantom-orchestrator/src/lib.rs specifically, declare: `pub mod error; pub mod git; pub mod materializer; pub mod scheduler; pub mod ripple;` with each module file containing just a comment.

4. **`docs/`** directory with stub files: architecture.md, semantic-merge.md, event-model.md (just a title and "TODO" in each).

## Tests for phantom-core
- Serde JSON round-trip for ALL types (serialize then deserialize, verify equality)
- `ContentHash::from_bytes` determinism (same input = same hash)
- `GitOid::zero()` is all zeros, `to_hex()` works
- Display impls for ID types

## Acceptance criteria
- `cargo build` succeeds for the ENTIRE workspace (all 6 crates)
- `cargo test -p phantom-core` passes
- `cargo clippy -- -D warnings` is clean for phantom-core
- All stub crates compile (even if they have no real code)

## Conventions
- Rust edition 2024
- Use `thiserror` for all error types
- Use `tracing` for logging (no println! in library crates)
- All public types and functions have doc comments
- Use `#[must_use]` on functions returning Result
- Derive Serialize/Deserialize on ALL types in phantom-core (they get stored in SQLite as JSON and passed between crates)
```

---

## Section 1: `feat/events` — Event Store

```
Read CLAUDE.md and plan.md thoroughly. You are implementing Section 1: the phantom-events crate.

Section 0 (phantom-core) has already been merged to main. You have access to all the core types and traits. Do NOT modify any files outside of `crates/phantom-events/src/`. The Cargo.toml for this crate already exists with correct dependencies.

## What to build

An SQLite-backed append-only event store implementing `phantom_core::traits::EventStore`. This is the persistence layer for all Phantom events.

### Files to implement

**`src/error.rs`**:
- `EventStoreError` enum with thiserror: `Sqlite(#[from] rusqlite::Error)`, `Serialization(#[from] serde_json::Error)`, `Core(#[from] phantom_core::error::CoreError)`

**`src/store.rs`** — the main store:
- `SqliteEventStore` struct wrapping `rusqlite::Connection`
- `open(path: &Path) -> Result<Self, EventStoreError>` — open or create DB, enable WAL mode, set busy_timeout(5s), enable foreign_keys, run schema migrations
- `in_memory() -> Result<Self, EventStoreError>` — for testing (uses `:memory:`)
- Private `ensure_schema(&self)` — creates the events table and indexes

SQLite schema:
```sql
CREATE TABLE IF NOT EXISTS events (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp    TEXT NOT NULL,
    changeset_id TEXT NOT NULL,
    agent_id     TEXT NOT NULL,
    kind         TEXT NOT NULL,
    dropped      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_events_changeset ON events(changeset_id);
CREATE INDEX IF NOT EXISTS idx_events_agent ON events(agent_id);
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
```

The `kind` column stores `EventKind` as JSON text (serde_json::to_string). This avoids a complex normalized schema.

Implement `phantom_core::traits::EventStore` for `SqliteEventStore`:
- `append`: INSERT the event, return the auto-generated id as EventId. The `Event.id` field passed in may have a placeholder value (EventId(0)) — use the AUTOINCREMENT id from SQLite.
- `query_by_changeset`: SELECT WHERE changeset_id = ? AND dropped = 0, ORDER BY id ASC
- `query_by_agent`: SELECT WHERE agent_id = ? AND dropped = 0, ORDER BY id ASC
- `query_all`: SELECT WHERE dropped = 0, ORDER BY id ASC
- `query_since`: SELECT WHERE timestamp >= ? AND dropped = 0, ORDER BY id ASC

Deserialize each row's `kind` column from JSON back into `EventKind`.

**`src/query.rs`** — advanced queries:
- `EventQuery` struct with optional fields: agent_id, changeset_id, symbol_id (Option<SymbolId>), since (Option<DateTime<Utc>>), limit (Option<u64>)
- `SqliteEventStore::query(&self, q: &EventQuery) -> Result<Vec<Event>, EventStoreError>` — build dynamic SQL WHERE clause from non-None fields
- `SqliteEventStore::mark_dropped(&self, changeset_id: &ChangesetId) -> Result<u64, EventStoreError>` — UPDATE events SET dropped = 1 WHERE changeset_id = ?, return number of rows affected

**`src/replay.rs`** — replay engine for rollback support:
- `ReplayEngine<'a>` holding `&'a SqliteEventStore`
- `new(store)` constructor
- `materialized_changesets(&self) -> Result<Vec<ChangesetId>>` — query events with kind containing "ChangesetMaterialized" and dropped = 0, return changeset_ids in order
- `changesets_after(&self, id: &ChangesetId) -> Result<Vec<ChangesetId>>` — find the materialization event for `id`, then return all materialized changeset_ids with a higher event id

**`src/projection.rs`** — derive current state from events:
- `Projection` struct with `HashMap<ChangesetId, Changeset>`
- `from_events(events: &[Event]) -> Self` — iterate events and build/update Changeset records:
  - OverlayCreated → create Changeset with InProgress status
  - ChangesetSubmitted → set status to Submitted, store operations
  - ChangesetMaterialized → set status to Materialized
  - ChangesetConflicted → set status to Conflicted
  - ChangesetDropped → set status to Dropped
  - TestsRun → update test_result
- `changeset(&self, id: &ChangesetId) -> Option<&Changeset>`
- `active_agents(&self) -> Vec<AgentId>` — agents with InProgress changesets
- `pending_changesets(&self) -> Vec<&Changeset>` — status == Submitted

**`src/lib.rs`** — re-export everything publicly.

## Tests (all using `SqliteEventStore::in_memory()`)

1. **Round-trip**: append 10 events across 2 changesets and 2 agents. Query all (10), query by changeset (subset), query by agent (subset). Verify event content matches.
2. **Mark-dropped**: append events for 3 changesets, mark_dropped one changeset, verify query_all excludes those events, verify mark_dropped returns correct count.
3. **query_since**: append events with different timestamps, query since a midpoint, verify only later events returned.
4. **EventQuery with multiple filters**: query with both agent_id and changeset_id set, verify intersection.
5. **Projection**: create event stream simulating full lifecycle (OverlayCreated → FileWritten → ChangesetSubmitted → ChangesetMaterialized), build projection, verify changeset status is Materialized and operations are populated.
6. **ReplayEngine**: materialize 3 changesets in order, call changesets_after(first), verify returns second and third.
7. **Empty store**: query_all on fresh store returns empty vec.
8. **WAL mode verification**: open store, query pragma journal_mode, assert it's "wal".

## Acceptance criteria
- `cargo test -p phantom-events` passes
- `cargo clippy -p phantom-events -- -D warnings` is clean
- All EventStore trait methods work correctly
- WAL mode is verified in tests

## Conventions
- Use thiserror for errors, tracing for logging
- All public items have doc comments
- Map CoreError appropriately when implementing the trait (the trait returns Result<_, CoreError>, so convert EventStoreError to CoreError::Serialization where needed, or change the approach — use impl blocks that return EventStoreError internally and have the trait impl convert)
```

---

## Section 2: `feat/overlay` — FUSE Overlay Filesystem

```
Read CLAUDE.md and plan.md thoroughly. You are implementing Section 2: the phantom-overlay crate.

Section 0 (phantom-core) has already been merged. Do NOT modify files outside `crates/phantom-overlay/src/`. The Cargo.toml already exists.

## What to build

A copy-on-write overlay filesystem for agent isolation. Key design: `OverlayLayer` handles all COW logic using plain filesystem operations (testable everywhere). `PhantomFs` is a thin FUSE adapter over it (Linux-only).

### Files to implement

**`src/error.rs`**:
- `OverlayError` enum: Io(#[from] io::Error), Fuse(String), NotFound(AgentId), AlreadyExists(AgentId), InodeNotFound(u64), PathNotFound(PathBuf)

**`src/layer.rs`** — core COW logic (the most important file):
```rust
pub struct OverlayLayer {
    lower: PathBuf,              // trunk working tree (read-only source)
    upper: PathBuf,              // agent's write layer
    whiteouts: HashSet<PathBuf>, // tracks deleted files (relative paths)
}
```

Methods:
- `new(lower: PathBuf, upper: PathBuf) -> Result<Self>` — create upper dir if needed, load whiteouts from `upper/.whiteouts.json` if it exists
- `read_file(&self, rel_path: &Path) -> Result<Vec<u8>>` — check upper first, fall through to lower. Return error if whiteout exists for this path.
- `write_file(&self, rel_path: &Path, data: &[u8]) -> Result<()>` — write to upper layer, create parent dirs as needed. Remove from whiteouts if present.
- `delete_file(&self, rel_path: &Path) -> Result<()>` — if file exists in upper, delete it. Add to whiteouts set. Persist whiteouts to `upper/.whiteouts.json`.
- `read_dir(&self, rel_path: &Path) -> Result<Vec<DirEntry>>` — merge entries from upper and lower dirs, exclude whiteouts, deduplicate by name
- `exists(&self, rel_path: &Path) -> bool` — not whiteout'd AND (exists in upper OR exists in lower)
- `getattr(&self, rel_path: &Path) -> Result<std::fs::Metadata>` — upper first, then lower
- `modified_files(&self) -> Result<Vec<PathBuf>>` — recursively walk upper directory, collect all file paths (relative), excluding .whiteouts.json
- `deleted_files(&self) -> Vec<PathBuf>` — return whiteouts as Vec
- `update_lower(&mut self, new_lower: PathBuf)` — update the lower layer pointer (trunk advanced)
- `persist_whiteouts(&self) -> Result<()>` — save whiteouts to upper/.whiteouts.json

`DirEntry` struct: name (OsString), file_type (enum: File, Directory, Symlink)

**`src/trunk_view.rs`** — read-only view of trunk:
- `TrunkView { work_tree: PathBuf }`
- `new(work_tree: PathBuf) -> Self`
- `read_file(&self, rel_path: &Path) -> Result<Vec<u8>>`
- `list_dir(&self, rel_path: &Path) -> Result<Vec<DirEntry>>`
- `file_attr(&self, rel_path: &Path) -> Result<std::fs::Metadata>`

**`src/fuse_fs.rs`** — FUSE adapter (gated behind `#[cfg(target_os = "linux")]`):
```rust
pub struct PhantomFs {
    layer: OverlayLayer,
    agent_id: AgentId,
    inodes: InodeTable,
}
```

`InodeTable`: bidirectional map between inode numbers (u64) and PathBuf. Root dir = inode 1. Use an AtomicU64 counter for allocation. Methods: `get_path(ino) -> Option<PathBuf>`, `get_or_create_inode(path) -> u64`, `remove(path)`.

Implement `fuser::Filesystem` for these operations:
- `lookup` — resolve child name in parent inode's directory, allocate inode, reply with FileAttr
- `getattr` — get path from inode, call layer.getattr, convert to fuser FileAttr
- `readdir` — get path from inode, call layer.read_dir, reply with entries (map each to inode)
- `open` — just reply with fh=0, flags (basic implementation)
- `read` — get path from inode, call layer.read_file, reply with slice at offset
- `write` — get path from inode, read current content, splice in new data at offset, call layer.write_file, reply with bytes written
- `create` — allocate inode for new path, write empty file via layer, reply
- `unlink` — call layer.delete_file, remove from inode table
- `setattr` — handle truncate (size=0 means truncate file), reply with current attr
- `mkdir` — create dir in upper layer, allocate inode
- `rmdir` — delete dir, remove from inodes

Convert std::fs::Metadata to fuser FileAttr helper function needed. Set reasonable TTL (1 second).

**`src/manager.rs`** — overlay lifecycle:
```rust
pub struct OverlayManager {
    phantom_dir: PathBuf,
    active_overlays: HashMap<AgentId, MountHandle>,
}

pub struct MountHandle {
    pub agent_id: AgentId,
    pub mount_point: PathBuf,
    pub upper_dir: PathBuf,
    // FUSE session handle, None when FUSE is not available
}
```

Methods:
- `new(phantom_dir: PathBuf) -> Self`
- `create_overlay(&mut self, agent_id: AgentId, trunk_path: &Path) -> Result<&MountHandle>` — create dirs at `.phantom/overlays/<agent_id>/upper/` and `.phantom/overlays/<agent_id>/mount/`, mount FUSE on Linux
- `destroy_overlay(&mut self, agent_id: &AgentId) -> Result<()>` — unmount, cleanup dirs
- `list_overlays(&self) -> Vec<&MountHandle>`
- `upper_dir(&self, agent_id: &AgentId) -> Result<&Path>`
- `notify_trunk_advanced(&mut self, new_trunk_path: &Path)` — update all layers' lower pointer
- `get_layer(&self, agent_id: &AgentId) -> Result<&OverlayLayer>` — for non-FUSE access

**`src/lib.rs`** — re-export everything.

## Tests

**OverlayLayer tests (no FUSE, run everywhere):**
1. Write file to upper, read back — content matches
2. File only in lower — read returns lower's content
3. File in both layers — upper wins
4. Delete file that exists in lower — read returns error, exists() returns false, read_dir excludes it
5. Delete then re-write — file is accessible again, removed from whiteouts
6. `modified_files()` returns exactly the files written to upper (not lower files, not whiteout marker)
7. `update_lower()` — change lower path, reads now fall through to new lower
8. Directory merging — files from both upper and lower appear in read_dir, no duplicates
9. Nested directories — write to `a/b/c.txt`, parent dirs created automatically
10. Whiteout persistence — delete file, create new OverlayLayer pointing to same upper, whiteouts restored

**FUSE tests (gated behind `#[cfg(all(test, feature = "fuse-tests"))]`):**
1. Mount, write file via std::fs to mount point, read back via std::fs
2. Mount, verify lower-layer files are visible at mount point
3. Mount, create and delete files, verify state

Add a `fuse-tests` feature to Cargo.toml (do NOT add it as default).

## Acceptance criteria
- `cargo test -p phantom-overlay` passes (OverlayLayer tests)
- `cargo clippy -p phantom-overlay -- -D warnings` clean
- All COW semantics correct: upper wins, whiteouts work, directory merging works
- FUSE code compiles on Linux (even if FUSE tests need the feature flag)
```

---

## Section 3: `feat/semantic` — Parsing + Symbol Extraction + Merge

```
Read CLAUDE.md and plan.md thoroughly. You are implementing Section 3: the phantom-semantic crate.

Section 0 (phantom-core) has been merged. Do NOT modify files outside `crates/phantom-semantic/src/`. The Cargo.toml already exists.

## What to build

Tree-sitter based parsing, symbol extraction, semantic diff, and three-way semantic merge engine. Implements `SymbolIndex` and `SemanticAnalyzer` traits from phantom-core.

## Design approach

Use **Weave-style entity matching**: symbols are identified by composite key `(name, kind, scope)`. This is simpler and more robust than AST node-position matching. Study how Weave (https://github.com/Ataraxy-Labs/weave) matches entities by identity.

### Files to implement

**`src/error.rs`**:
- `SemanticError` enum: ParseError { path: PathBuf, detail: String }, UnsupportedLanguage { path: PathBuf }, Io(#[from] io::Error), MergeError(String)

**`src/languages/mod.rs`** — trait + registry:
```rust
pub trait LanguageExtractor: Send + Sync {
    fn language(&self) -> tree_sitter::Language;
    fn extensions(&self) -> &[&str];
    fn extract_symbols(&self, tree: &tree_sitter::Tree, source: &[u8], file_path: &Path) -> Vec<SymbolEntry>;
}
```

**`src/languages/rust.rs`** — IMPLEMENT THIS FULLY. This is the priority language.
- `RustExtractor` implementing `LanguageExtractor`
- Returns `tree_sitter_rust::LANGUAGE.into()` for language()
- Extensions: `&["rs"]`
- `extract_symbols`: walk tree-sitter CST using TreeCursor. Extract these node kinds:
  - `function_item` → SymbolKind::Function (get name from "name" field child)
  - `struct_item` → SymbolKind::Struct
  - `enum_item` → SymbolKind::Enum
  - `trait_item` → SymbolKind::Trait
  - `impl_item` → SymbolKind::Impl (name = trait name or type name)
  - `use_declaration` → SymbolKind::Import (name = full path text)
  - `const_item` → SymbolKind::Const
  - `type_item` → SymbolKind::TypeAlias
  - `mod_item` → SymbolKind::Module
  - `macro_definition` → SymbolKind::Function (treat macros as functions for now)
  - Methods inside impl blocks → SymbolKind::Method with scope including the impl target
  - `#[test]` annotated functions → SymbolKind::Test
- For scope: walk parent nodes to find enclosing mod/impl and build scope string like "crate::module::impl_type"
- For content_hash: hash the byte range of the entire symbol node with BLAKE3
- Generate SymbolId as `"{scope}::{name}::{kind}"` (lowercase kind)

**`src/languages/typescript.rs`** — implement with these node kinds:
- `function_declaration` → Function, `class_declaration` → Class, `interface_declaration` → Interface
- `method_definition` → Method, `import_statement` → Import, `export_statement` → Import
- `type_alias_declaration` → TypeAlias, `enum_declaration` → Enum
- Extensions: `&["ts", "tsx", "js", "jsx"]`

**`src/languages/python.rs`** — implement with:
- `function_definition` → Function, `class_definition` → Class
- `import_statement` / `import_from_statement` → Import
- Extensions: `&["py"]`

**`src/languages/go.rs`** — implement with:
- `function_declaration` → Function, `method_declaration` → Method
- `type_declaration` → Struct/Interface (check inner node), `import_declaration` → Import
- Extensions: `&["go"]`

**`src/parser.rs`**:
```rust
pub struct Parser {
    languages: HashMap<String, Box<dyn LanguageExtractor>>,
}
```
- `new()` — register RustExtractor, TypeScriptExtractor, PythonExtractor, GoExtractor. Map each extension to its extractor.
- `parse_file(&self, path: &Path, content: &[u8]) -> Result<Vec<SymbolEntry>, SemanticError>` — detect language from extension, create tree_sitter::Parser, set language, parse content, call extractor
- `detect_language(&self, path: &Path) -> Option<&str>` — match extension
- `supports_language(&self, path: &Path) -> bool`

**`src/index.rs`**:
```rust
pub struct InMemorySymbolIndex {
    symbols: HashMap<SymbolId, SymbolEntry>,
    file_to_symbols: HashMap<PathBuf, Vec<SymbolId>>,
    indexed_at: GitOid,
}
```
- Implement `phantom_core::traits::SymbolIndex`
- `new(commit: GitOid) -> Self` — empty index
- `build_from_directory(root: &Path, parser: &Parser, commit: GitOid) -> Result<Self, SemanticError>` — walk directory recursively, parse each supported file, populate index

**`src/diff.rs`**:
```rust
pub fn diff_symbols(base: &[SymbolEntry], new: &[SymbolEntry], file: &Path) -> Vec<SemanticOperation>
```
Algorithm (Weave-style entity matching by composite key):
1. Build HashMap<(name, kind, scope), &SymbolEntry> for both base and new
2. For each entry in new not in base → AddSymbol
3. For each entry in both with different content_hash → ModifySymbol
4. For each entry in base not in new → DeleteSymbol
Match by (name, kind, scope) tuple — this is the entity identity.

**`src/merge.rs`** — the three-way semantic merge engine:
```rust
pub struct SemanticMerger {
    parser: Parser,
}
```
Implement `phantom_core::traits::SemanticAnalyzer` for `SemanticMerger`.

`three_way_merge(base, ours, theirs, path)` algorithm:
1. If language not supported → fall back to line-based text merge (use `diffy` or `similar` crate, or implement simple line merge)
2. Parse all three versions with tree-sitter, extract symbols
3. Compute ours_ops = diff_symbols(base_symbols, ours_symbols)
4. Compute theirs_ops = diff_symbols(base_symbols, theirs_symbols)
5. Check for conflicts:
   - Same symbol modified by both (same SymbolId in both modify sets) → CONFLICT (BothModifiedSymbol)
   - One modifies, other deletes same symbol → CONFLICT (ModifyDeleteSymbol)
   - Both add same-named symbol → if content_hash identical, deduplicate; else CONFLICT
   - Both add different symbols → no conflict
   - Both delete same symbol → no conflict (already gone)
6. If any conflicts → return MergeResult::Conflict(details)
7. If clean → reconstruct merged file:
   - Start with base source
   - Sort all symbols by byte_range.start
   - Walk through base, for each symbol region:
     - If modified by ours: use ours version's bytes
     - If modified by theirs: use theirs version's bytes
     - If deleted by either: skip
     - If unchanged: keep base bytes
   - Append symbols that were added (by ours and/or theirs) at appropriate positions
   - Return MergeResult::Clean(merged_bytes)

For file reconstruction, track "regions" of the file — the interstitial bytes between symbols come from base, symbol bytes come from the appropriate version.

**`src/lib.rs`** — re-export everything.

## Tests

**Rust symbol extraction:**
1. Parse file with 3 functions → 3 SymbolEntry, correct names/kinds/scopes
2. Parse file with struct + impl block with 2 methods → Struct + Impl + 2 Method entries, methods have scope including impl target
3. Parse file with use statements → Import symbols
4. Parse file with `#[test] fn test_foo()` → SymbolKind::Test
5. Empty file → empty vec
6. File with syntax error → partial extraction (tree-sitter is error-tolerant, should still find valid symbols)
7. Nested modules → correct scope chain

**TypeScript/Python/Go extraction:** at least 1-2 tests each verifying basic symbol extraction works.

**Diff tests:**
1. Base {f1, f2}, new {f1_modified, f2, f3} → [ModifySymbol(f1), AddSymbol(f3)]
2. Base {f1, f2}, new {f1} → [DeleteSymbol(f2)]
3. Identical files → empty diff
4. Complete rewrite (all symbols changed) → deletes + adds for everything

**Three-way merge tests (critical — use actual Rust source code strings):**
1. Both add different functions to same file → Clean merge, verify output contains both new functions plus originals
2. Both modify same function → Conflict with BothModifiedSymbol
3. One adds function, other modifies different existing function → Clean merge
4. One deletes function, other modifies same function → Conflict with ModifyDeleteSymbol
5. Both add same import → Clean (deduplicate)
6. Both add identical function (same content) → Clean (deduplicate)
7. Disjoint changes → Clean merge preserves all changes
8. Unsupported file type → falls back to text merge (or returns clean if no changes)

**Index tests:**
1. Build from directory with multiple .rs files → all symbols indexed
2. update_file → old symbols removed, new ones added
3. remove_file → symbols for that file gone

## Acceptance criteria
- `cargo test -p phantom-semantic` passes
- Rust extractor handles all major symbol types
- Three-way merge auto-merges disjoint changes and detects real conflicts
- At least basic extraction works for TypeScript, Python, Go
- Unsupported files handled gracefully (fallback, not crash)
```

---

## Section 4: `feat/git-ops` — Git Operations

```
Read CLAUDE.md and plan.md thoroughly. You are implementing Section 4: the git.rs and error.rs modules of phantom-orchestrator.

Section 0 (phantom-core) has been merged. You ONLY own these files:
- `crates/phantom-orchestrator/src/git.rs`
- `crates/phantom-orchestrator/src/error.rs`

Do NOT modify lib.rs (it already declares `pub mod git; pub mod error;`), do NOT touch any other crate. The Cargo.toml already has git2 as a dependency.

## What to build

Git operations via the `git2` crate, plus OID conversion helpers.

### Files to implement

**`src/error.rs`**:
```rust
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("event store error: {0}")]
    EventStore(String),
    #[error("semantic error: {0}")]
    Semantic(String),
    #[error("overlay error: {0}")]
    Overlay(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("materialization failed: {0}")]
    MaterializationFailed(String),
    #[error("not found: {0}")]
    NotFound(String),
}
```

**`src/git.rs`**:

First, implement conversions between phantom_core::id::GitOid and git2::Oid:
```rust
impl From<git2::Oid> for GitOid {
    fn from(oid: git2::Oid) -> Self {
        let mut bytes = [0u8; 20];
        bytes.copy_from_slice(oid.as_bytes());
        GitOid(bytes)
    }
}

impl TryFrom<GitOid> for git2::Oid {
    type Error = git2::Error;
    fn try_from(oid: GitOid) -> Result<Self, Self::Error> {
        git2::Oid::from_bytes(&oid.0)
    }
}
```

Then the main struct:
```rust
pub struct GitOps {
    repo: git2::Repository,
}
```

Methods:
- `open(repo_path: &Path) -> Result<Self, OrchestratorError>` — open existing repo
- `repo(&self) -> &git2::Repository` — expose for advanced operations
- `head_oid(&self) -> Result<GitOid, OrchestratorError>` — get HEAD commit OID. Handle the case where HEAD is unborn (new repo with no commits).
- `read_file_at_commit(&self, oid: &GitOid, path: &Path) -> Result<Vec<u8>, OrchestratorError>` — resolve OID to commit, get tree, walk tree to find blob at path, return blob content
- `list_files_at_commit(&self, oid: &GitOid) -> Result<Vec<PathBuf>, OrchestratorError>` — walk tree recursively, collect all blob paths
- `commit_overlay_changes(&self, upper_dir: &Path, trunk_path: &Path, message: &str, author: &str) -> Result<GitOid, OrchestratorError>`:
  1. Walk upper_dir recursively to find all modified files
  2. For each file, copy from upper_dir to trunk_path (working directory)
  3. Add all changed files to the index (repo.index())
  4. Write tree from index
  5. Create commit with current HEAD as parent
  6. Return new commit OID
- `reset_to_commit(&self, oid: &GitOid) -> Result<(), OrchestratorError>` — hard reset: update HEAD to point to this commit, reset index and working directory
- `changed_files(&self, from: &GitOid, to: &GitOid) -> Result<Vec<PathBuf>, OrchestratorError>` — diff two commits' trees, return list of changed file paths
- `text_merge(&self, base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<phantom_core::traits::MergeResult, OrchestratorError>` — use git2's merge_file or a simple line-based three-way merge. Return MergeResult::Clean if no conflicts, MergeResult::Conflict with a ConflictDetail if there are conflicts.

## Tests

All tests use `tempfile::TempDir` and `git2::Repository::init()`. Create helper functions for common setup.

1. **open + head_oid**: init repo, make initial commit, verify head_oid returns correct OID
2. **read_file_at_commit**: commit a file with known content, read it back, verify exact bytes
3. **list_files_at_commit**: commit 3 files in nested dirs, verify all paths returned
4. **commit_overlay_changes**: 
   - Init repo with initial commit containing `src/main.rs`
   - Create upper dir with modified `src/main.rs` and new `src/lib.rs`
   - Call commit_overlay_changes
   - Verify HEAD advanced (new OID != old OID)
   - Read files at new commit, verify content matches upper dir
5. **reset_to_commit**: make 3 commits, reset to first, verify HEAD points to first
6. **changed_files**: two commits, verify diff shows correct paths
7. **text_merge clean**: base="a\nb\nc\n", ours="a\nb\nd\n", theirs="a\ne\nc\n" → Clean merge "a\ne\nd\n"
8. **text_merge conflict**: both modify same line → Conflict result
9. **GitOid round-trip**: create git2::Oid, convert to GitOid, convert back, verify equality

## Acceptance criteria
- `cargo test -p phantom-orchestrator` passes (only git and error module tests, other modules are stubs)
- Conversions between GitOid and git2::Oid are lossless
- All git operations work correctly against real temp repos
- Error handling is comprehensive (no unwrap/expect in library code)
```

---

## Section 5: `feat/orchestrator` — Materializer, Scheduler, Ripple

```
Read CLAUDE.md and plan.md thoroughly. You are implementing Section 5: the materializer.rs, scheduler.rs, and ripple.rs modules of phantom-orchestrator.

Sections 0, 1, 3, and 4 have been merged. You have access to:
- phantom_core (types, traits)
- phantom_events::SqliteEventStore (implements EventStore)
- phantom_semantic::SemanticMerger (implements SemanticAnalyzer)
- phantom_orchestrator::git::GitOps and error::OrchestratorError

You ONLY own:
- `crates/phantom-orchestrator/src/materializer.rs`
- `crates/phantom-orchestrator/src/scheduler.rs`
- `crates/phantom-orchestrator/src/ripple.rs`

You may need to update `src/lib.rs` if the module declarations need adjustment, but the stub from Section 0 should already declare all modules.

## What to build

The coordination layer that wires git, events, and semantic analysis together.

### Files to implement

**`src/scheduler.rs`** — task queue:
```rust
pub struct Scheduler {
    queue: VecDeque<Changeset>,
}
```
- `new() -> Self`
- `enqueue(&mut self, changeset: Changeset)` — push to back
- `next(&mut self) -> Option<Changeset>` — pop from front (FIFO)
- `pending(&self) -> Vec<&Changeset>` — view all pending (don't consume)
- `remove(&mut self, id: &ChangesetId) -> Option<Changeset>` — remove specific changeset
- `len(&self) -> usize`
- `is_empty(&self) -> bool`

Simple FIFO for now. Priority scheduling can be added later.

**`src/ripple.rs`** — trunk change notification:
```rust
pub struct RippleChecker;
```
- `new() -> Self`
- `check_ripple(changed_files: &[PathBuf], active_agents: &[(AgentId, Vec<PathBuf>)]) -> HashMap<AgentId, Vec<PathBuf>>`:
  - For each active agent, check if any of their touched files overlap with changed_files
  - Return map of agent_id → list of overlapping file paths
  - This is a simple set intersection per agent

**`src/materializer.rs`** — the core coordination engine:
```rust
pub struct Materializer {
    git: GitOps,
}
```

Use trait objects or generics where needed. The materializer coordinates:

- `new(git: GitOps) -> Self`

- `materialize(&self, changeset: &Changeset, upper_dir: &Path, event_store: &dyn EventStore, analyzer: &dyn SemanticAnalyzer) -> Result<MaterializeResult, OrchestratorError>`:

  The algorithm:
  1. Get current HEAD via `git.head_oid()`
  2. Get the working tree path from the repo
  3. **If HEAD == changeset.base_commit** (trunk hasn't advanced since agent started):
     - No merge needed, just apply changes directly
     - Call `git.commit_overlay_changes(upper_dir, trunk_path, message, agent_id)`
     - Append ChangesetMaterialized event to event_store
     - Return MaterializeResult::Success { new_commit }
  4. **If HEAD != changeset.base_commit** (trunk advanced, need merge):
     - For each file in changeset.files_touched:
       a. Read base version: `git.read_file_at_commit(&changeset.base_commit, &file)`
       b. Read current trunk (ours): `git.read_file_at_commit(&head, &file)` 
       c. Read agent's version (theirs): read from upper_dir
       d. Call `analyzer.three_way_merge(base, ours, theirs, &file)`
       e. If MergeResult::Conflict → collect conflicts
       f. If MergeResult::Clean → store merged content
     - For new files (AddFile in operations, not in base): just copy from upper_dir
     - If any conflicts: 
       - Append ChangesetConflicted event
       - Return MaterializeResult::Conflict { details }
     - If all clean:
       - Write merged files to working tree
       - Git commit
       - Append ChangesetMaterialized event
       - Return MaterializeResult::Success { new_commit }

  Handle edge cases:
  - File exists in upper but not in base (new file) → just add, no merge needed
  - File deleted by agent (in whiteouts) → git rm
  - File doesn't exist at base_commit (was added after base) → skip or handle

```rust
pub enum MaterializeResult {
    Success { new_commit: GitOid },
    Conflict { details: Vec<ConflictDetail> },
}
```

## Tests

**Scheduler tests (pure unit tests):**
1. Enqueue 3 changesets, next() returns them in FIFO order
2. Remove from middle, verify remaining order
3. pending() shows all without consuming
4. Empty scheduler: next() returns None

**RippleChecker tests:**
1. Two agents, changed file overlaps agent B's files but not A's → only B in result
2. No overlap → empty result
3. Multiple overlapping files → all listed for affected agent
4. Same file touched by both agents → both agents in result

**Materializer tests (use mock implementations):**

Create mock structs for testing:
```rust
struct MockEventStore { events: RefCell<Vec<Event>> }
struct MockAnalyzer { merge_results: HashMap<PathBuf, MergeResult> }
```

Tests:
1. **Direct apply (trunk not advanced):** base_commit == HEAD, materialize succeeds, verify commit created, event appended
2. **Clean merge (trunk advanced):** base_commit != HEAD, analyzer returns Clean for all files, verify merged content committed
3. **Conflict detected:** analyzer returns Conflict for one file, verify MaterializeResult::Conflict returned, no commit made, ChangesetConflicted event appended
4. **New file (not in base):** changeset adds a file that didn't exist at base_commit, verify it's included in commit without merge
5. **Multiple files:** changeset touches 3 files, 2 merge clean, 1 conflicts → whole materialization returns Conflict

For tests that need real git repos, use tempfile::TempDir + git2::Repository::init() with GitOps::open().

## Acceptance criteria
- `cargo test -p phantom-orchestrator` passes (all modules)
- Materializer correctly handles: direct apply, clean merge, conflict detection
- Scheduler is FIFO and supports removal
- RippleChecker correctly identifies affected agents
- No unwrap/expect in library code
```

---

## Section 6: `feat/cli` — Binary Crate

```
Read CLAUDE.md and plan.md thoroughly. You are implementing Section 6: the phantom-cli binary crate.

All other sections have been merged. You have access to every phantom crate. You ONLY own files under `crates/phantom-cli/src/`.

## What to build

The `phantom` CLI tool with all subcommands. Uses clap derive for argument parsing.

### Files to implement

**`src/main.rs`**:
```rust
use clap::Parser;

#[derive(Parser)]
#[command(name = "phantom", version, about = "Semantic version control for agentic AI development")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Initialize Phantom in an existing git repository
    Up,
    /// Assign a task to a new agent overlay
    Dispatch(commands::dispatch::DispatchArgs),
    /// Submit an agent's work as a changeset
    Submit(commands::submit::SubmitArgs),
    /// Show status of overlays and changesets
    Status,
    /// Materialize a changeset to trunk
    Materialize(commands::materialize::MaterializeArgs),
    /// Roll back a changeset and replay downstream
    Rollback(commands::rollback::RollbackArgs),
    /// Query the event log
    Log(commands::log::LogArgs),
    /// Destroy an agent's overlay
    Destroy(commands::destroy::DestroyArgs),
}
```

Use `#[tokio::main]` and `tracing_subscriber::fmt::init()` for setup.

**`src/context.rs`** — shared context loaded by each command:
```rust
pub struct PhantomContext {
    pub phantom_dir: PathBuf,
    pub repo_root: PathBuf,
    pub git: GitOps,
    pub events: SqliteEventStore,
    pub overlays: OverlayManager,
    pub semantic: SemanticMerger,
}

impl PhantomContext {
    /// Find .phantom/ by walking up from current dir. Open all subsystems.
    pub fn load() -> anyhow::Result<Self>;
}
```

**`src/commands/mod.rs`** — declare all subcommand modules.

**`src/commands/up.rs`** — `phantom up`:
1. Verify current directory is a git repo (look for `.git/`)
2. Check `.phantom/` doesn't already exist (or offer to reinitialize)
3. Create directory structure:
   - `.phantom/overlays/`
   - `.phantom/events.db` — create via SqliteEventStore::open()
   - `.phantom/config.toml` — write default config (phantom_version, created_at)
4. Add `.phantom/` to `.gitignore` if not already there
5. Print success: "Phantom initialized in <path>"

**`src/commands/dispatch.rs`** — `phantom dispatch --agent <name> --task "<description>"`:
- Args: agent (String), task (String)
1. Load PhantomContext
2. Generate changeset ID (e.g., "cs-" + sequential number from event count, or uuid short)
3. Create overlay via overlays.create_overlay(agent_id, repo_root)
4. Create Changeset with InProgress status, base_commit = current HEAD
5. Append OverlayCreated event
6. Print: "Agent '<name>' dispatched. Overlay at <mount_point>"

**`src/commands/submit.rs`** — `phantom submit --agent <name>`:
- Args: agent (String)
1. Load PhantomContext
2. Get upper_dir from overlay manager
3. List modified_files from overlay layer
4. For each modified file:
   - Read base version (from git at base_commit)
   - Read agent version (from upper dir)
   - Extract symbols from both, compute diff
5. Build Vec<SemanticOperation> from all diffs
6. Append ChangesetSubmitted event with operations
7. Print summary: "Changeset <id> submitted: X additions, Y modifications, Z deletions"

**`src/commands/materialize.rs`** — `phantom materialize --changeset <id>`:
- Args: changeset (String)
1. Load PhantomContext
2. Build projection to find the changeset and its agent
3. Get upper_dir for the agent
4. Call Materializer::materialize()
5. On Success: print "Materialized <id> → commit <oid>". Run ripple check, print affected agents.
6. On Conflict: print each conflict detail

**`src/commands/rollback.rs`** — `phantom rollback --changeset <id>`:
- Args: changeset (String)
1. Load PhantomContext
2. Mark changeset events as dropped
3. Find the commit OID before this changeset's materialization (from event log)
4. git.reset_to_commit(pre_commit_oid)
5. Use ReplayEngine to find downstream changesets
6. Print: "Rolled back <id>. Downstream changesets requiring re-dispatch: [list]"
7. Do NOT auto-replay (that's for the user/orchestrator to decide)

**`src/commands/status.rs`** — `phantom status`:
1. Load PhantomContext
2. Build projection from all events
3. Display formatted output:
   - Trunk HEAD: <oid short>
   - Active overlays: table of (agent_id, mount_point, status)
   - Pending changesets: table of (id, agent, task, status)
   - Total events: count

**`src/commands/log.rs`** — `phantom log [options]`:
- Args: --agent (Option<String>), --changeset (Option<String>), --symbol (Option<String>), --since (Option<String> like "2h" or "1d"), --limit (Option<u64>)
1. Load PhantomContext  
2. Build EventQuery from args. Parse --since into a DateTime.
3. Execute query
4. Display events chronologically with formatting:
   ```
   [2025-01-15 10:30:00] cs-0042 agent-a ChangesetMaterialized { commit: abc123 }
   ```

**`src/commands/destroy.rs`** — `phantom destroy --agent <name>`:
- Args: agent (String)
1. Load PhantomContext
2. Destroy overlay (unmount + cleanup)
3. Append OverlayDestroyed event
4. Print: "Agent '<name>' overlay destroyed"

## Tests

Use `assert_cmd` crate for CLI testing and `tempfile` for temp repos.

1. **phantom up**: run in temp git repo → verify .phantom/ exists with events.db, overlays/, config.toml
2. **phantom up outside git repo**: should fail with meaningful error
3. **phantom dispatch + status**: dispatch agent, run status, verify agent appears
4. **phantom help**: verify all subcommands listed
5. **Full workflow smoke test**: up → dispatch → (write files to upper dir manually) → submit → materialize → log → verify events

## Acceptance criteria
- `cargo build -p phantom-cli` produces a working `phantom` binary
- All subcommands parse correctly and produce helpful --help output
- `phantom up` creates correct directory structure
- `phantom status` displays meaningful formatted output
- Error messages are user-friendly (no raw backtraces)
- .phantom/ is added to .gitignore
```

---

## Section 7: `feat/integration-tests` — Integration Tests

```
Read CLAUDE.md and plan.md thoroughly. You are implementing Section 7: workspace-level integration tests.

All other sections have been merged. You own the `tests/` directory at the workspace root. Do NOT modify any crate code.

## What to build

End-to-end integration tests covering the 6 core scenarios from CLAUDE.md. These tests exercise multiple crates together.

### Test infrastructure

**`tests/common/mod.rs`** (or use a helper module):
```rust
use tempfile::TempDir;
use git2::Repository;
use phantom_core::id::*;
use phantom_events::SqliteEventStore;
use phantom_semantic::merge::SemanticMerger;
use phantom_orchestrator::git::GitOps;
use phantom_orchestrator::materializer::{Materializer, MaterializeResult};
use phantom_overlay::layer::OverlayLayer;

pub struct TestContext {
    pub dir: TempDir,
    pub repo: Repository,
    pub git: GitOps,
    pub events: SqliteEventStore,
    pub analyzer: SemanticMerger,
}

impl TestContext {
    /// Create a test git repo with initial commit, initialize event store.
    pub fn new() -> Self;

    /// Create an overlay layer for an agent (no FUSE, just OverlayLayer).
    pub fn create_agent_layer(&self, agent_id: &str) -> (AgentId, OverlayLayer, PathBuf);

    /// Write a file to an agent's upper layer.
    pub fn agent_write(&self, upper_dir: &Path, rel_path: &str, content: &str);

    /// Build a Changeset from an agent's overlay.
    pub fn build_changeset(&self, agent_id: &AgentId, upper_dir: &Path, task: &str) -> Changeset;

    /// Commit initial files to the repo.
    pub fn commit_files(&self, files: &[(&str, &str)]) -> GitOid;

    /// Get current HEAD.
    pub fn head(&self) -> GitOid;

    /// Read a file from the current trunk working tree.
    pub fn read_trunk_file(&self, path: &str) -> String;
}
```

Key design: use `OverlayLayer` directly (NOT FUSE mounts) so tests run on any platform without privileges.

### Test files

**`tests/integration/two_agents_disjoint.rs`**:
```rust
#[test]
fn test_two_agents_disjoint_files_auto_merges() {
    // 1. Create repo with src/a.rs containing `fn alpha() -> i32 { 1 }` and src/b.rs containing `fn beta() -> i32 { 2 }`
    // 2. Create overlay layers for agent-a and agent-b
    // 3. Agent-a modifies src/a.rs: adds `fn alpha_two() -> i32 { 12 }`
    // 4. Agent-b modifies src/b.rs: adds `fn beta_two() -> i32 { 22 }`
    // 5. Build changesets for both
    // 6. Materialize agent-a's changeset → assert Success
    // 7. Materialize agent-b's changeset → assert Success (different files, no conflict)
    // 8. Read trunk src/a.rs → contains both alpha and alpha_two
    // 9. Read trunk src/b.rs → contains both beta and beta_two
}
```

**`tests/integration/two_agents_same_file.rs`**:
```rust
#[test]
fn test_two_agents_same_file_different_symbols_auto_merges() {
    // 1. Create repo with src/handlers.rs containing `fn handle_login() { }`
    // 2. Agent-a adds `fn handle_register() { }` to the file
    // 3. Agent-b adds `fn handle_admin() { }` to the file
    // 4. Materialize agent-a → Success
    // 5. Materialize agent-b → Success (different symbols, semantic merge handles it)
    // 6. Read trunk src/handlers.rs → contains all 3 functions
}
```

**`tests/integration/two_agents_same_symbol.rs`**:
```rust
#[test]
fn test_two_agents_same_symbol_conflicts() {
    // 1. Create repo with src/lib.rs containing `fn compute() -> i32 { 42 }`
    // 2. Agent-a modifies compute(): `fn compute() -> i32 { 100 }`
    // 3. Agent-b modifies compute(): `fn compute() -> i32 { 200 }`
    // 4. Materialize agent-a → Success
    // 5. Materialize agent-b → Conflict (both modified same symbol)
    // 6. Verify ConflictDetail mentions compute and BothModifiedSymbol
}
```

**`tests/integration/materialize_and_ripple.rs`**:
```rust
#[test]
fn test_ripple_notification_after_materialize() {
    // 1. Create repo with src/shared.rs containing `fn helper() { }`
    // 2. Agent-a adds `fn new_func()` to src/shared.rs
    // 3. Agent-b has touched src/shared.rs (added a different function)
    // 4. Materialize agent-a → Success
    // 5. Get changed_files between old HEAD and new HEAD
    // 6. Run RippleChecker with agent-b's touched files
    // 7. Assert agent-b is in the ripple result with src/shared.rs
    // 8. Verify agent-b's overlay lower-layer would reflect new trunk (update_lower called)
}
```

**`tests/integration/rollback_replay.rs`**:
```rust
#[test]
fn test_rollback_middle_changeset_replays_downstream() {
    // 1. Create repo with src/lib.rs
    // 2. Materialize cs-001 (adds fn one)
    // 3. Materialize cs-002 (adds fn two)
    // 4. Materialize cs-003 (adds fn three, independent of cs-002)
    // 5. Record commit OID before cs-002
    // 6. Mark cs-002 events as dropped
    // 7. Reset trunk to pre-cs-002 commit
    // 8. Use ReplayEngine to find changesets after cs-002 → [cs-003]
    // 9. Re-materialize cs-003 → should succeed (no dependency on cs-002)
    // 10. Verify trunk has fn one and fn three, but NOT fn two
}
```

**`tests/integration/event_log_query.rs`**:
```rust
#[test]
fn test_event_log_queries() {
    // 1. Run a workflow that generates 15+ events across 2 agents and 3 changesets
    // 2. Query by agent-a → verify only agent-a events
    // 3. Query by cs-002 → verify only cs-002 events
    // 4. Query since a timestamp → verify correct subset
    // 5. Query all → verify total count
    // 6. Mark cs-001 as dropped → query all returns fewer events
    // 7. Verify event ordering is by id (chronological)
}
```

### Fixtures

**`tests/fixtures/`** — sample Rust source files:
- `sample_handlers.rs` — a file with 2-3 handler functions
- `sample_models.rs` — a file with structs and impls
- `sample_lib.rs` — a file with mixed content (functions, structs, imports)

These are used by test helpers to seed repos. Keep them small but realistic enough for symbol extraction to work.

## Implementation notes

- All tests use `tempfile::TempDir` — never write to real filesystem
- Tests are independent — no shared state between tests
- No FUSE — use OverlayLayer directly for all tests
- Test helper functions go in `tests/common/mod.rs`
- Use `#[test]` (not async) unless async is needed
- Source code in fixtures should be valid, parseable Rust
- Keep test assertions specific: assert exact function names, exact conflict types

## Acceptance criteria
- All 6 integration tests pass: `cargo test --test '*'`
- Tests complete in under 60 seconds
- No temp directory leaks (TempDir cleans up on drop)
- Tests are deterministic — no flaky timing-dependent assertions
- Tests validate the CORE PROMISE: disjoint changes auto-merge, same-symbol conflicts are caught, rollback works
```

---

These prompts are self-contained. Each worktree session gets everything it needs without reading the full plan.
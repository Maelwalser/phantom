# Semantic Merge

This document describes how Phantom turns multiple agents' edits into a
clean trunk commit without line-based conflicts when the edits touch
disjoint symbols.

The engine is implemented in `phantom-semantic` and invoked by
`phantom-orchestrator` on every submit.

## The problem

Git merges on text lines. When two agents add different functions to the
same Rust file, they each touch roughly the same lines (the area at the
end of the file where new functions get appended), so git reports a
conflict:

```
<<<<<<< agent-a
fn handle_register() { ... }
=======
fn handle_admin()    { ... }
>>>>>>> agent-b
```

There is no semantic conflict here — the functions are independent. The
conflict is a rendering artifact of text-line merging.

Phantom merges at the *symbol* level instead: it parses the file with
tree-sitter, identifies the symbols each side added / modified /
deleted, and composes the output from symbol regions. Two adds to the
same file merge cleanly as long as the two symbols have different
identities.

## The pipeline

```
base bytes   ours bytes   theirs bytes      file path
     │            │            │                  │
     ▼            ▼            ▼                  ▼
 ┌────────────────────────────────────────────────────┐
 │  SemanticMerger::three_way_merge                   │
 │                                                    │
 │  1. Short-circuit cases                            │
 │     ours == theirs        → Clean(ours)            │
 │     ours == base          → Clean(theirs)          │
 │     theirs == base        → Clean(ours)            │
 │                                                    │
 │  2. Language supported? (Parser::supports_language)│
 │                                                    │
 │        yes                        no               │
 │         │                          │               │
 │         ▼                          ▼               │
 │  3. semantic_merge()        4. text_merge()        │
 │     ├─ parse all three         (diffy line-based,  │
 │     ├─ build symbol maps        ConflictKind       │
 │     ├─ detect conflicts         ::RawTextConflict  │
 │     ├─ reconstruct merged       on conflict)       │
 │     │  file from regions                           │
 │     └─ if merged parses        → MergeReport with  │
 │        cleanly → Clean           strategy =        │
 │        else → fall back to       TextFallback-     │
 │        text_merge                Unsupported       │
 │                                                    │
 └────────────────────────────────────────────────────┘
     │
     ▼
 MergeReport { result, strategy }
```

`MergeStrategy` records which path produced the result:

| Strategy | Meaning |
|----------|---------|
| `Semantic` | Full tree-sitter path succeeded end-to-end. |
| `Trivial` | Short-circuit hit — conceptually correct, no parse needed. |
| `TextFallbackUnsupported` | Language has no extractor; used line-based merge only. |
| `TextFallbackSemanticError` | Semantic merger raised an error; fell back to text merge. |
| `TextFallbackInvalidSyntax` | Semantic merge produced output that failed to re-parse; fell back to text merge. |

`MergeStrategy::is_text_fallback()` is the gate the CLI uses to surface
a "merged via text fallback" warning to users.

## Symbol extraction

Each supported language implements the `LanguageExtractor` trait:

```rust
pub trait LanguageExtractor: Send + Sync {
    fn language(&self) -> tree_sitter::Language;
    fn extensions(&self) -> &[&str];
    fn filenames(&self) -> &[&str] { &[] }          // e.g. "Dockerfile"
    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry>;
}
```

The `Parser` registers extractors from `all_extractors()` and routes
incoming files by:

1. **Exact filename** (for `Dockerfile`, `Makefile`).
2. **Lowercased extension** (everything else).

Parser creation is cheap; a shared `tree_sitter::Parser` is held behind
a `Mutex` and the language is hot-swapped per file via
`parser.set_language(...)`.

### What counts as a symbol?

`SymbolKind` is the vocabulary (from `phantom-core`):

`Function, Struct, Enum, Trait, Impl, Import, Const, TypeAlias, Module,
Test, Class, Interface, Method, Section, Directive, Variable`.

Not every language produces every kind. The table below lists the
extractors that ship today and what they actually emit.

### Supported languages

| Language | File match | Symbols emitted |
|----------|-----------|-----------------|
| Rust | `.rs` | Functions, structs, enums, traits, impls, imports, constants, modules, tests, macros, type aliases |
| TypeScript / JavaScript | `.ts`, `.js` | Functions, classes, interfaces, methods, imports, enums, type aliases |
| TSX / JSX | `.tsx`, `.jsx` | Same as TypeScript |
| Python | `.py` | Functions, classes, methods, imports |
| Go | `.go` | Functions, methods (with receiver type in scope), structs, interfaces, type declarations, imports |
| YAML | `.yml`, `.yaml` | Top-level keys (`Section`) |
| TOML | `.toml` | Top-level tables and keys (`Section`) |
| JSON | `.json` | Top-level keys (`Section`) |
| Bash | `.sh`, `.bash`, `.zsh` | Functions, aliases, variables |
| CSS | `.css` | Selectors, at-rules (`Directive`), custom properties |
| HCL / Terraform | `.tf`, `.hcl` | Resources (`Section`), variables, outputs, modules |
| Dockerfile | `Dockerfile` (exact filename) | Stages (`Section`), instructions (`Directive`) |
| Makefile | `.mk`, `Makefile` | Targets (`Section`), variables |

Files without a matching grammar fall through to line-based text merge
(`MergeStrategy::TextFallbackUnsupported`).

### `SymbolEntry` shape

```rust
pub struct SymbolEntry {
    pub id: SymbolId,            // "scope::name::kind", unique per file
    pub kind: SymbolKind,
    pub name: String,            // e.g. "handle_login"
    pub scope: String,           // e.g. "crate::handlers"
    pub file: PathBuf,
    pub byte_range: Range<usize>,
    pub content_hash: ContentHash, // BLAKE3 of the symbol bytes
}
```

The `content_hash` is the crucial bit: whether a symbol's body changed
between two versions is answered by a single 32-byte comparison, not a
substring diff.

## Weave-style entity matching

Symbols from different versions of a file must be matched up to decide
whether each one was added, modified, or deleted. Phantom uses the
composite key

```rust
type EntityKey = (String, SymbolKind, String);  // (name, kind, scope)
```

Two symbols in different versions are considered "the same entity"
iff their `(name, kind, scope)` keys are equal. Byte ranges are
ignored for matching — moving a function in the file does not create
a phantom "delete + add".

## `diff_symbols`: base → current

```rust
pub fn diff_symbols(base: &[SymbolEntry], current: &[SymbolEntry], file: &Path)
    -> Vec<SemanticOperation>
```

The algorithm:

1. Build `base_map: HashMap<EntityKey, &SymbolEntry>` from `base`.
2. Build `current_map` from `current`.
3. For each `(key, new_entry)` in `current_map`:
   - If `key` is not in `base_map` → `AddSymbol`.
   - If `key` is in `base_map` and the content hashes differ →
     `ModifySymbol { old_hash, new_entry }`.
   - Otherwise the symbol is unchanged — emit nothing.
4. For each `key` in `base_map` not in `current_map` →
   `DeleteSymbol`.
5. Sort by operation type (`Add < Modify < Delete`) for deterministic
   output.

The resulting `Vec<SemanticOperation>` is what ends up on a
`ChangesetSubmitted` event.

## `three_way_merge`: base + ours + theirs

```rust
fn three_way_merge(base, ours, theirs, path) -> MergeReport
```

### Step 1 — short-circuits (`MergeStrategy::Trivial`)

```
ours == theirs   → Clean(ours)
ours == base     → Clean(theirs)   // theirs made the only change
theirs == base   → Clean(ours)     // we made the only change
```

No parse required.

### Step 2 — unsupported language

If `Parser::supports_language(path) == false`, return
`MergeReport::text_fallback(text_merge(...),
TextFallbackUnsupported)`.

### Step 3 — semantic conflict detection

Parse all three versions. Build three symbol maps
(`base_map`, `ours_map`, `theirs_map`) keyed by `EntityKey`.

**Both-modified / both-added conflict.** For each key present in both
`ours_map` and `theirs_map`:

- Determine `ours_changed` and `theirs_changed` by comparing to base
  (or treating missing-in-base as "both added").
- If both changed *and* the content hashes differ, emit a
  `ConflictKind::BothModifiedSymbol` detail.
- If both changed and the content hashes *match*, it is a duplicate
  add — deduplicate silently.

**Modify-delete conflict.** For each key present in `base_map`:

- Present in `ours_map` with different content, missing from
  `theirs_map` → `ConflictKind::ModifyDeleteSymbol` (theirs deleted,
  ours modified).
- Mirror case → same kind, opposite sides.

If `conflicts` is non-empty, the function returns
`(MergeResult::Conflict(conflicts), MergeStrategy::Semantic)` — the
orchestrator records these in a `ChangesetConflicted` event and marks
the changeset `Conflicted`.

### Step 4 — reconstruct the merged file

With no conflicts detected, `reconstruct_merged_file` composes the
output:

1. Build a byte-range map of base symbols.
2. Walk base, replacing each symbol region with the appropriate
   version from ours or theirs based on who changed it (if anyone).
3. Gather symbols added by either side that are absent from base.
4. Anchor each added symbol to its nearest base sibling (preceding
   or following) by `find_base_sibling`, preserving relative ordering.
5. Splice added symbols into the output buffer at the resolved
   anchor positions, tracking insertion order to break ties when
   multiple additions share an anchor.
6. Ensure a trailing newline separates inserted symbols from
   surrounding content (`ensure_newline`).

### Step 5 — syntax safety net

The reconstructed bytes are re-parsed. If tree-sitter reports any
syntax errors, the engine treats the reconstruction as untrustworthy,
calls `text_merge` instead, and tags the result
`MergeStrategy::TextFallbackInvalidSyntax`.

This guards against edge cases where symbol byte ranges drift (e.g. a
newly added struct field shifts subsequent impl blocks by a few bytes).
We would rather accept a less-pretty line-level merge than emit a file
the compiler rejects.

## `ConflictDetail`

```rust
pub struct ConflictDetail {
    pub kind: ConflictKind,      // BothModifiedSymbol, ModifyDeleteSymbol, ...
    pub file: PathBuf,
    pub symbol_id: Option<SymbolId>,
    pub ours_changeset: ChangesetId,
    pub theirs_changeset: ChangesetId,
    pub description: String,
    pub ours_span: Option<ConflictSpan>,   // byte + 1-indexed lines
    pub theirs_span: Option<ConflictSpan>,
    pub base_span: Option<ConflictSpan>,
}
```

Conflict kinds:

- `BothModifiedSymbol` — both sides modified the same symbol body.
- `ModifyDeleteSymbol` — one side modified, the other deleted.
- `BothModifiedDependencyVersion` — both sides touched the same
  dependency line (TOML / JSON).
- `RawTextConflict` — text-fallback merge found a line-level conflict.
- `BinaryFile` — file is binary or invalid UTF-8; refuses to merge.

The CLI uses the spans to print file:line references so users can
jump directly to the conflict in their editor.

## Text-merge fallback

`text::text_merge` wraps the `diffy` crate's three-way line merger:

- On clean merge → `MergeResult::Clean(bytes)`.
- On conflict → a single `ConflictDetail` with
  `ConflictKind::RawTextConflict` and a best-effort span covering the
  first conflict region.
- On invalid UTF-8 in any input → `ConflictKind::BinaryFile`.

## Interaction with the orchestrator

During a submit:

1. `overlay_scan` enumerates modified files.
2. For each file, `operations` calls `extract_symbols` on base and
   current, then `diff_symbols` to build the operation list.
3. `changeset_builder` assembles the `Changeset`.
4. `pipeline::run` calls `materializer::materialize`, which for each
   touched file:
   - Reads base, ours (current trunk), theirs (agent upper).
   - Calls `three_way_merge(base, ours, theirs, path)`.
   - On `Clean(bytes)` → stage the blob.
   - On `Conflict(details)` → accumulate into a
     `ChangesetConflicted` event.
5. If every file merges cleanly, build a tree, commit, emit
   `ChangesetMaterialized`.

During a live rebase (ripple):

- `live_rebase::rebase_agent` calls `three_way_merge` on each shadowed
  file in the *other* agent's upper layer, using
  `old_base` (the agent's previous base commit), `new_head` (the
  updated trunk), and the agent's current upper bytes.
- Clean merges are atomically written (tmp file + rename).
- Conflicts leave the upper untouched — the agent keeps its version
  and is notified via `TrunkNotification`.

## Testing

Semantic merge tests live in:

- `crates/phantom-semantic/src/merge/tests.rs` — unit tests driven
  through `SemanticMerger::three_way_merge` with concrete source
  strings.
- `tests/integration/tests/two_agents_disjoint.rs` — end-to-end on
  different files.
- `tests/integration/tests/two_agents_same_file.rs` — same file,
  different symbols (the headline case).
- `tests/integration/tests/two_agents_same_symbol.rs` — same symbol
  → verifies `ChangesetConflicted`.
- `tests/integration/tests/semantic_merge_fallback.rs` — unsupported
  language falls through to text merge.

## Adding a new language

1. Add a module in `crates/phantom-semantic/src/languages/<lang>.rs`.
2. Implement `LanguageExtractor` — choose the tree-sitter grammar,
   list extensions (and filenames if appropriate), and walk the CST
   emitting `SymbolEntry` values using the shared helpers
   (`push_symbol`, `push_named_symbol`, `for_each_named_child`).
3. Add the tree-sitter dependency to `Cargo.toml` under
   `[workspace.dependencies]` and wire it into
   `phantom-semantic`'s manifest.
4. Register the extractor in `all_extractors()` in
   `languages/mod.rs`.
5. Add unit tests in the new module.
6. Add an integration test demonstrating semantic merge on a pair of
   agents editing a file in the new language.

Because `Parser` is fully data-driven, no changes to `SemanticMerger`
or the orchestrator are required.

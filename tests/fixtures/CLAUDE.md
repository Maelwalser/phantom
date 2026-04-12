# CLAUDE.md — Phantom Test Repository

This is a **test repository** used to validate the Phantom version control system. Your job is to make small, predictable code changes so that Phantom's overlay filesystem, semantic merging, conflict detection, and materialization can be exercised.

## What This Repo Contains

A small Rust project with three source files:

| File | Contents |
|------|----------|
| `src/lib.rs` | A `compute()` function, a `Config` struct with `new()` impl |
| `src/handlers.rs` | `handle_login()` and `handle_logout()` functions |
| `src/models.rs` | `User` struct with `new()`, `Post` struct |

## Rules for Making Changes

1. **Keep changes small and self-contained.** One function, one struct, or one import per task.
2. **Never modify existing function bodies** unless the task explicitly says to. Add new functions instead.
3. **Use valid Rust syntax.** The semantic index parses these files with tree-sitter.
4. **No external dependencies.** Only use `std` library types.
5. **No `mod` declarations or new files** unless the task explicitly asks for it.

## Task Catalog

Pick a task from this list. Each task is designed to exercise a specific Phantom scenario.

### Disjoint File Changes (no conflict expected)

- **Task A1:** Add a `handle_register()` function to `src/handlers.rs`.
- **Task A2:** Add a `Comment` struct to `src/models.rs`.
- **Task A3:** Add a `validate()` function to `src/lib.rs`.

Running A1 + A2 concurrently should auto-merge with zero conflicts.

### Same File, Different Symbols (no conflict expected)

- **Task B1:** Add `handle_register()` to `src/handlers.rs`.
- **Task B2:** Add `handle_health_check()` to `src/handlers.rs`.

Running B1 + B2 concurrently targets the same file but different symbols. Phantom should auto-merge.

### Same Symbol Conflict (conflict expected)

- **Task C1:** Add a `handle_register()` function to `src/handlers.rs` that prints "register v1".
- **Task C2:** Add a `handle_register()` function to `src/handlers.rs` that prints "register v2".

Running C1 + C2 concurrently should produce a **semantic conflict** on `handlers::handle_register`.

### Struct Field Additions (no conflict expected)

- **Task D1:** Add an `email: String` field to `User` in `src/models.rs`.
- **Task D2:** Add a `created_at: u64` field to `Post` in `src/models.rs`.

Same file, different structs. Should auto-merge.

### Same Struct Conflict (conflict expected)

- **Task E1:** Add an `email: String` field to `User` in `src/models.rs`.
- **Task E2:** Add an `email: Option<String>` field to `User` in `src/models.rs`.

Both modify the same struct with conflicting definitions. Should produce a conflict.

### Import Changes (no conflict expected)

- **Task F1:** Add `use std::fmt;` to `src/lib.rs`.
- **Task F2:** Add `use std::io;` to `src/handlers.rs`.

Disjoint imports across files. Should auto-merge.

### Duplicate Import Dedup

- **Task G1:** Add `use std::collections::HashSet;` to `src/lib.rs`.
- **Task G2:** Add `use std::collections::HashSet;` to `src/lib.rs`.

Both add the same import. Phantom should auto-deduplicate.

### Modify vs Delete Conflict (conflict expected)

- **Task H1:** Modify the body of `handle_logout()` in `src/handlers.rs`.
- **Task H2:** Delete `handle_logout()` from `src/handlers.rs`.

One agent modifies a symbol, the other deletes it. Should produce a conflict.

### Ripple / Notification Test

- **Task R1:** Add a `get_user_by_id()` function to `src/models.rs`.
- **Task R2 (after R1 materializes):** Add a `handle_profile()` function to `src/handlers.rs` that calls `get_user_by_id()`.

R1 materializes first. Agent working on R2 should receive a ripple notification that trunk changed.

## Example Change

If your task is "Add a `handle_register()` function to `src/handlers.rs`", produce exactly this kind of change:

```rust
fn handle_register() {
    // register new user
}
```

Append it after the existing functions. Do not rewrite the file.

## What NOT to Do

- Do not refactor or reorganize existing code.
- Do not add tests, benchmarks, or documentation.
- Do not rename files or modules.
- Do not add Cargo.toml dependencies.
- Do not make changes outside `src/`.
- Do not produce large diffs. A good change is 3-10 lines.

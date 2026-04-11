<div align="center">
<pre>
.----. .-. .-.  .--.  .-. .-. .---.  .----. .-.   .-.
| {}  }| {_} | / {} \ |  `| |{_   _}/  {}  \|  `.'  |
| .--' | { } |/  /\  \| |\  |  | |  \      /| |\ /| |
`-'    `-' `-'`-'  `-'`-' `-'  `-'   `----' `-' ` `-'
</pre>
</div>

**Event-sourced semantic version control for parallel AI agents**

Phantom is a version control layer built on top of Git that enables multiple AI coding agents to work on the same codebase simultaneously — with automatic conflict resolution at the symbol level, not the line level.

<p align="center">
  <img src="docs/assets/demo.gif" alt="Phantom CLI demo" width="800" />
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> &middot;
  <a href="#installation">Installation</a> &middot;
  <a href="#commands">Commands</a> &middot;
  <a href="#how-it-works">How It Works</a> &middot;
  <a href="#contributing">Contributing</a>
</p>

---

## Why Phantom?

Git branches model human workflows — long-lived divergent lines of work reconciled later. Agentic development is different: multiple agents work on small, scoped tasks simultaneously, and their outputs must compose cleanly without manual merge resolution.

| Approach | What happens when two agents edit the same file? |
|----------|--------------------------------------------------|
| Git worktrees | Line-based conflict. Human must intervene. |
| **Phantom** | AST-level semantic merge. Auto-resolves if different symbols were touched. |

Phantom replaces branches with **changesets** — reorderable, atomic units of work. Two agents can add different functions to the same file and Phantom merges them automatically, because it understands code structure, not just text lines.

## Features

- **Semantic merging** — Conflict detection at the AST level via tree-sitter. Two agents adding different functions to the same file? No conflict.
- **FUSE overlays** — Each agent gets an isolated copy-on-write filesystem. Reads fall through to trunk; writes are captured. No git branches, no rebasing.
- **Event sourcing** — Every agent action is an immutable event in an append-only log. Full auditability, surgical rollback, and "what-if" replay.
- **Instant trunk propagation** — When one agent's work is materialized, all other agents immediately see the updated trunk through their overlays. No rebase step.
- **Multi-language support** — Symbol extraction for Rust, TypeScript, Python, and Go via tree-sitter grammars.
- **Zero-config conflict resolution** — Disjoint symbol changes auto-merge. True conflicts (same function modified by two agents) are detected and reported clearly.

## Quick Start

```bash
# Install (requires libfuse3-dev on Linux)
cargo install --path crates/phantom-cli

# Initialize in any git repository
cd /path/to/your/repo
phantom up

# Dispatch two agents in parallel
phantom d agent-a --background --task "add user authentication"
phantom d agent-b --background --task "add rate limiting"

# Agents work on their isolated overlays...
# When done, submit and materialize
phantom sub agent-a
phantom mat cs-0001

# Agent B's overlay automatically sees Agent A's changes.
# If Agent B touched different symbols, it merges cleanly.
phantom sub agent-b
phantom mat cs-0002
```

## Installation

### Prerequisites

- **Rust** toolchain (edition 2024)
- **Git** (standard installation)
- **Linux:** `libfuse3-dev` and `pkg-config`

<details>
<summary><strong>Ubuntu / Debian</strong></summary>

```bash
sudo apt install libfuse3-dev pkg-config build-essential
```
</details>

<details>
<summary><strong>Fedora</strong></summary>

```bash
sudo dnf install fuse3-devel pkg-config
```
</details>

<details>
<summary><strong>Arch Linux</strong></summary>

```bash
sudo pacman -S fuse3 pkgconf
```
</details>

### From source

```bash
git clone https://github.com/Maelwalser/phantom.git
cd phantom
cargo build --release
cargo install --path crates/phantom-cli
```

### Verify

```bash
phantom --help
```

## Commands

| Command | Shortcut | Description |
|---------|----------|-------------|
| `phantom up` | | Initialize Phantom in the current git repository |
| `phantom dispatch` | `phantom d` | Create an isolated overlay and assign a task to an agent |
| `phantom submit` | `phantom sub` | Package an agent's changes into a changeset with semantic operations |
| `phantom materialize` | `phantom mat` | Run semantic merge and commit a changeset to trunk |
| `phantom status` | `phantom st` | Show active overlays, pending changesets, and trunk state |
| `phantom rollback` | `phantom rb` | Drop a changeset and identify downstream work needing re-dispatch |
| `phantom log` | `phantom l` | Query the event log with filters |
| `phantom destroy` | `phantom rm` | Tear down an agent's overlay |

### `phantom up`

Initialize Phantom in an existing git repository. Creates the `.phantom/` directory with an event store, overlay root, and configuration.

```bash
cd /path/to/your/repo
phantom up
```

### `phantom dispatch` / `d`

Assign a task to a new agent. Creates a copy-on-write overlay filesystem where the agent can read trunk files and write modifications in isolation.

```bash
# Interactive — launches a Claude Code session inside the overlay
phantom d agent-a

# Background — for scripted/headless agents (--task required)
phantom dispatch agent-a --background --task "implement caching layer"
```

The agent works in `.phantom/overlays/agent-a/` — a normal directory where reads fall through to trunk and writes are captured.

### `phantom submit` / `sub`

When an agent finishes, submit its work. Phantom parses the modified files with tree-sitter, extracts semantic operations (functions added, structs modified, imports changed), and records everything in the event log.

```bash
phantom sub agent-a
```

### `phantom materialize` / `mat`

Apply a submitted changeset to trunk. Phantom runs a three-way semantic merge: if the trunk has advanced since the changeset's base commit, it checks for symbol-level conflicts rather than line-level conflicts.

```bash
phantom mat cs-0001
```

**Possible outcomes:**
- **Success** — Changes committed to trunk. Other agents' overlays automatically reflect the new trunk.
- **Conflict** — Two agents modified the same symbol. The changeset is marked conflicted; re-dispatch the agent.

### `phantom rollback` / `rb`

Surgically remove a changeset from history. Phantom marks the changeset's events as dropped and identifies any downstream changesets that depend on it.

```bash
phantom rb cs-0001
```

### `phantom log` / `l`

Query the append-only event log. Every agent action, every materialization, and every conflict is recorded.

```bash
# All recent events
phantom l

# Filter by agent
phantom l agent-b

# Filter by changeset
phantom log cs-0042

# Filter by time
phantom l --since 2h

# Filter by symbol
phantom log --symbol "handlers::handle_login"

# Limit results
phantom l --limit 20
```

### `phantom status` / `st`

View the current state of the system: active agents, pending changesets, trunk HEAD, and event count.

```bash
phantom st
```

### `phantom destroy` / `rm`

Remove an agent's overlay and clean up its resources.

```bash
phantom rm agent-a
```

## How It Works

Phantom sits between your AI agents and the Git repository. Each agent works in an isolated overlay — it sees the latest trunk but writes to its own private layer. When an agent finishes, Phantom analyzes the changes at the AST level and merges them into trunk automatically.

```
                     ┌─────────────────┐
                     │  Orchestrator   │
                     └────────┬────────┘
                          dispatch
            ┌─────────────────┼─────────────────┐
            ▼                 ▼                 ▼
      ┌───────────┐    ┌───────────┐    ┌───────────┐
      │  Agent A  │    │  Agent B  │    │  Agent C  │
      │  (FUSE)   │    │  (FUSE)   │    │  (FUSE)   │
      └─────┬─────┘    └─────┬─────┘    └─────┬─────┘
          submit             │                │
            ▼                ▼                ▼
      ┌───────────────────────────────────────────┐
      │            Changesets (upper)             │
      └─────────────────────┬─────────────────────┘
                            │ check
                            ▼
                  ┌──────────────────┐
                  │  Semantic Index  │◄──┐
                  │  (tree-sitter)   │   │ parse
                  └────────┬─────────┘   │
                           │ merge       │
                           ▼             │
                  ┌──────────────────┐   │
                  │  Trunk (git main)├───┘
                  └────────┬─────────┘
                           │ log
                           ▼
                  ┌──────────────────┐
                  │  Event Log       │
                  │  (SQLite)        │
                  └──────────────────┘
```

### The merge problem Phantom solves

```
  Git sees this:                    Phantom sees this:

  handlers.rs                       handlers.rs
  <<<<<<< claude-a                  ┌──────────────────────────┐
  + fn handle_register()            │ fn handle_login()  [unchanged]
  =======                           │ fn handle_register() [claude-a added]
  + fn handle_admin()               │ fn handle_admin()    [claude-b added]
  >>>>>>> claude-b                  └──────────────────────────┘
                                    → Different symbols. Auto-merge.
  CONFLICT — manual fix needed
```

### How it flows

1. **Dispatch** — `phantom d claude-a` creates a FUSE overlay and launches an interactive session. Use `--background --task "..."` for headless agents.

2. **Work** — The agent (Claude, Cursor, Codex, etc.) writes code. It reads trunk files through the overlay's lower layer and writes changes to the upper layer. Other agents can't see or interfere with its work.

3. **Submit** — `phantom sub claude-a` parses the modified files with tree-sitter, extracts what changed at the symbol level (functions added, structs modified, imports changed), and records a changeset in the event log.

4. **Materialize** — `phantom mat cs-0001` runs a three-way semantic merge against trunk. If the symbols an agent touched are disjoint from what changed on trunk, it auto-merges. If the same symbol was modified by two agents, it reports a conflict.

5. **Ripple** — After materialization, Phantom checks which other running agents might be affected by the trunk change and notifies them. Their overlays automatically see the new trunk on the next read.

### Conflict resolution

| Scenario | Result |
|----------|--------|
| Both agents add different symbols to the same file | Auto-merge |
| Both agents add different fields to the same struct | Auto-merge |
| Both agents modify the same function body | **Conflict** — re-dispatch |
| One modifies, other deletes the same symbol | **Conflict** — re-dispatch |
| Both add the same import | Auto-deduplicate |
| Additive insertions to the same collection (routes, middleware) | Auto-merge |

For files without tree-sitter grammar support, Phantom falls back to git's line-based three-way merge.

## Project Structure

```
phantom/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── phantom-cli/              # Binary — the `phantom` command
│   ├── phantom-core/             # Core types, traits, errors (zero phantom deps)
│   ├── phantom-events/           # SQLite event store (WAL mode)
│   ├── phantom-overlay/          # FUSE overlay filesystem
│   ├── phantom-semantic/         # tree-sitter parsing, semantic merge
│   └── phantom-orchestrator/     # Git ops, materialization, ripple
├── tests/                        # Workspace-level integration tests
└── docs/                         # Architecture and design documentation
```

## Development

```bash
# Build the workspace
cargo build

# Run all tests
cargo test

# Run a specific crate's tests
cargo test -p phantom-core

# Run integration tests
cargo test --test two_agents_disjoint

# Check without building
cargo check

# Lint
cargo clippy -- -D warnings
```

### Running the integration tests

The integration tests create temporary git repositories, dispatch simulated agents, and verify semantic merging behavior end-to-end:

```bash
cargo test --test two_agents_disjoint       # Disjoint files auto-merge
cargo test --test two_agents_same_file      # Same file, different symbols
cargo test --test two_agents_same_symbol    # Same symbol = conflict detected
cargo test --test materialize_and_ripple    # Trunk propagation to active agents
cargo test --test rollback_replay           # Surgical rollback and replay
cargo test --test event_log_query           # Event log querying
```

## Supported Languages

Phantom extracts symbols from source files using tree-sitter grammars:

| Language | Extracted Symbols |
|----------|-------------------|
| Rust | Functions, structs, enums, traits, impls, imports, constants, modules, tests |
| TypeScript | Classes, interfaces, functions, methods, imports |
| Python | Classes, functions, methods, imports |
| Go | Functions, structs, interfaces, methods |

Files in unsupported languages fall back to git's line-based merge.

## Roadmap

- [x] Core types and event store
- [x] FUSE overlay with copy-on-write
- [x] All CLI commands
- [x] Semantic merging (4 languages)
- [x] Integration test suite
- [ ] Agent re-dispatch automation
- [ ] macOS support (NFS overlay fallback)
- [ ] Incremental parsing for large codebases
- [ ] Agent wrapper scripts (Claude Code, Cursor, Codex)
- [ ] Configuration file (`.phantom/config.toml` expansion)
- [ ] Performance benchmarks

## Contributing

Contributions are welcome. To get started:

1. Fork the repository
2. Install system dependencies (`libfuse3-dev` on Linux)
3. Run `cargo test` to verify everything passes
4. Create a branch and make your changes
5. Submit a pull request

See the [architecture documentation](docs/architecture.md) for design details.

## License

MIT OR Apache-2.0

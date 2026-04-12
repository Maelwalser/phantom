<div align="center">
<pre>
.----. .-. .-.  .--.  .-. .-. .---.  .----. .-.   .-.
| {}  }| {_} | / {} \ |  `| |{_   _}/  {}  \|  `.'  |
| .--' | { } |/  /\  \| |\  |  | |  \      /| |\ /| |
`-'    `-' `-'`-'  `-'`-' `-'  `-'   `----' `-' ` `-'
</pre>
</div>

**A version control designed for AI coding tools**<br/>
Phantom is an extension for Git designed for simultaneous multi-agent collaboration. It binds stateful coding sessions to lightweight FUSE virtual filesystems, so feature-specific contexts can be paused, resumed, and re-entered at will.

<p align="center">
  <img src="docs/assets/demo.gif" alt="Phantom CLI demo" width="800" />
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> &middot;
  <a href="#installation">Installation</a> &middot;
  <a href="#commands">Commands</a> &middot;
  <a href="#sessions">Sessions</a> &middot;
  <a href="#how-it-works">How It Works</a> &middot;
  <a href="#contributing">Contributing</a>
</p>

---

## Why Phantom?

Git branches model human workflows — long-lived divergent lines of work reconciled later. Agentic development is different: multiple agents work on small, scoped tasks simultaneously, and their outputs must compose cleanly without manual merge resolution.

| Approach      | What happens when two agents edit the same file?                           |
| ------------- | -------------------------------------------------------------------------- |
| Git worktrees | Line-based conflict. Human must intervene.                                 |
| **Phantom**   | AST-level semantic merge. Auto-resolves if different symbols were touched. |

Phantom replaces branches with **changesets** — reorderable, atomic units of work. Two agents can add different functions to the same file and Phantom merges them automatically, because it understands code structure, not just text lines.

## Features

- **Session-aware dispatch** — Each `phantom dispatch` launches a coding session (defaults to Claude Code) inside the overlay. Sessions are automatically captured and persisted, so re-dispatching the same agent resumes exactly where it left off.
- **Semantic merging** — Conflict detection at the AST level via tree-sitter. Two agents adding different functions to the same file? No conflict.
- **FUSE overlays** — Each agent gets an isolated copy-on-write filesystem. Reads fall through to trunk; writes are captured. No git branches, no rebasing.
- **Event sourcing** — Every agent action is an immutable event in an append-only log. Full auditability, surgical rollback, and "what-if" replay.
- **Live rebase** — When one agent's work is materialized, Phantom automatically rebases other agents' overlapping files at the symbol level and notifies them of trunk changes.
- **Auto-submit and auto-materialize** — Use `--auto-submit` or `--auto-materialize` to automatically submit and merge an agent's work when the session exits.
- **Multi-language support** — Symbol extraction for Rust, TypeScript, Python, and Go via tree-sitter grammars.
- **Zero-config conflict resolution** — Disjoint symbol changes auto-merge. True conflicts (same function modified by two agents) are detected and reported clearly.

## Quick Start

```bash
# Install (requires libfuse3-dev on Linux)
cargo install --path crates/phantom-cli

# Initialize in any git repository
cd /path/to/your/repo
phantom up

# Dispatch an agent — opens an interactive Claude Code session
phantom d agent-a

# Or dispatch in the background with a task description
phantom d agent-b --background --task "add rate limiting"

# When done, submit and materialize
phantom sub agent-a
phantom mat agent-a

# Agent B's overlay automatically sees Agent A's changes.
# If Agent B touched different symbols, it merges cleanly.
phantom sub agent-b
phantom mat agent-b

# Or do it all in one shot — session exits, auto-submit and merge
phantom d agent-c --auto-materialize
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
| `phantom dispatch` | `phantom d` | Create an overlay, bind a coding session, and assign a task |
| `phantom submit` | `phantom sub` | Package an agent's changes into a changeset with semantic operations |
| `phantom materialize` | `phantom mat` | Run semantic merge and commit a changeset to trunk |
| `phantom status` | `phantom st` | Show active overlays, pending changesets, and trunk state |
| `phantom rollback` | `phantom rb` | Drop a changeset and revert its changes from trunk |
| `phantom log` | `phantom l` | Query the event log with filters |
| `phantom destroy` | `phantom rm` | Tear down an agent's overlay and FUSE mount |

### `phantom up`

Initialize Phantom in an existing git repository. Creates the `.phantom/` directory with an event store, overlay root, and configuration. Adds `.phantom/` to `.gitignore` automatically.

```bash
cd /path/to/your/repo
phantom up
```

### `phantom dispatch` / `d`

Create an overlay and launch a coding session for an agent. Each dispatch binds a coding session to the overlay — if the agent already has an active overlay, the existing session is resumed instead of creating a new one.

```bash
# Interactive — launches Claude Code inside the overlay (default)
phantom d agent-a

# Resume an existing agent's session (automatic if overlay exists)
phantom d agent-a

# Background — create overlay without launching a session (--task required)
phantom d agent-b --background --task "implement caching layer"

# Auto-submit when the session exits
phantom d agent-a --auto-submit

# Auto-submit AND auto-materialize when the session exits
phantom d agent-a --auto-materialize

# Use a custom CLI instead of Claude Code
phantom d agent-a --command aider

# Skip FUSE mounting (agent works via upper layer only)
phantom d agent-a --no-fuse
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--background` / `-b` | Create overlay without launching a session (requires `--task`) |
| `--task` | Task description (required with `--background`, used in context file) |
| `--auto-submit` | Automatically submit the changeset when the session exits |
| `--auto-materialize` | Auto-submit and auto-materialize on session exit |
| `--command` | Custom CLI command to run instead of `claude` |
| `--no-fuse` | Skip FUSE mounting; agent writes to the upper layer directly |

The agent works in `.phantom/overlays/<agent>/mount/` (FUSE merged view) or `.phantom/overlays/<agent>/upper/` (with `--no-fuse`). A `.phantom-task.md` context file is placed in the working directory with agent metadata and instructions.

### `phantom submit` / `sub`

When an agent finishes, submit its work. Phantom parses the modified files with tree-sitter, extracts semantic operations (functions added, structs modified, imports changed), and records everything in the event log.

```bash
phantom sub agent-a
```

### `phantom materialize` / `mat`

Apply a submitted changeset to trunk. Accepts either a changeset ID or an agent name (resolves to the agent's latest submitted changeset).

Phantom runs a three-way semantic merge: if the trunk has advanced since the changeset's base commit, it checks for symbol-level conflicts rather than line-level conflicts.

```bash
# By changeset ID
phantom mat cs-0001-123456

# By agent name (resolves to their latest submitted changeset)
phantom mat agent-a

# With a custom commit message
phantom mat agent-a -m "feat: add user authentication"
```

**Possible outcomes:**
- **Success** — Changes committed to trunk. Other agents' overlays are live-rebased if they touch the same files, and notified of trunk changes.
- **Conflict** — Two agents modified the same symbol. The changeset is marked conflicted; re-dispatch the agent.

### `phantom rollback` / `rb`

Surgically remove a changeset. Phantom marks the changeset's events as dropped and creates a git revert commit. Identifies any downstream changesets that may need re-dispatch.

```bash
phantom rb cs-0001-123456
```

### `phantom log` / `l`

Query the append-only event log. Every agent action, every materialization, every conflict, and every live rebase is recorded.

```bash
# All recent events (default limit: 50)
phantom l

# Filter by agent
phantom l agent-b

# Filter by changeset
phantom log cs-0042-789012

# Filter by time
phantom l --since 2h

# Filter by symbol
phantom log --symbol "handlers::handle_login"

# Limit results
phantom l --limit 20
```

Duration units: `s` (seconds), `m` (minutes), `h` (hours), `d` (days).

### `phantom status` / `st`

View the current state: active agents, pending changesets, trunk HEAD, and event count.

```bash
phantom st
```

### `phantom destroy` / `rm`

Remove an agent's overlay, unmount its FUSE filesystem, and clean up its resources (including persisted session data).

```bash
phantom rm agent-a
```

## Sessions

Each dispatch binds a **coding session** to the agent's overlay. This is a core concept in Phantom — agents don't just get isolated filesystems, they get persistent, resumable coding contexts.

### How sessions work

1. **First dispatch** — `phantom d agent-a` creates the overlay, spawns a FUSE mount, and launches an interactive coding CLI (Claude Code by default) inside the overlay directory. The CLI process runs in a PTY so Phantom can capture its session ID from the terminal output.

2. **Session capture** — When the coding CLI exits, Phantom extracts the session ID (e.g., Claude Code's `--resume` UUID) and persists it to `.phantom/overlays/<agent>/cli_session.json`.

3. **Resume dispatch** — Running `phantom d agent-a` again detects the existing overlay, loads the saved session ID, and launches the CLI with `--resume <session-id>`. The agent picks up exactly where it left off — same conversation context, same file state.

4. **Session lifecycle** — The session persists as long as the overlay exists. Submitting or materializing the changeset ends the session. Destroying the overlay (`phantom rm agent-a`) clears the session data.

### Supported CLIs

| CLI | Session Resume | How |
|-----|---------------|-----|
| Claude Code | Yes | Captures `--resume <UUID>` from output, passes it on next dispatch |
| Custom (`--command`) | No | Generic adapter; no session extraction |

### Environment variables

The following environment variables are set in the coding session:

| Variable | Description |
|----------|-------------|
| `PHANTOM_AGENT_ID` | The agent's identifier |
| `PHANTOM_CHANGESET_ID` | The bound changeset ID |
| `PHANTOM_OVERLAY_DIR` | Path to the agent's working directory |
| `PHANTOM_REPO_ROOT` | Path to the underlying git repository |
| `PHANTOM_INTERACTIVE` | Set to `1` during interactive sessions |

### Context file

A `.phantom-task.md` file is placed in the overlay with agent metadata:

```markdown
# Phantom Agent Session

You are working inside a Phantom overlay. Your changes are isolated from
trunk and other agents.

## Task
<task description if provided>

## Agent Info
- Agent: agent-a
- Changeset: cs-0001-123456
- Base commit: abc123def456

## Commands
- `phantom submit agent-a` — submit your changes
- `phantom materialize cs-0001-123456` — merge to trunk
- `phantom status` — view all agents and changesets
```

## How It Works

Phantom sits between your AI agents and the Git repository. Each agent gets an isolated FUSE overlay with a bound coding session. When an agent finishes, Phantom analyzes the changes at the AST level and merges them into trunk automatically.

```
                     ┌─────────────────┐
                     │  Orchestrator   │
                     └────────┬────────┘
                          dispatch
            ┌─────────────────┼─────────────────┐
            ▼                 ▼                 ▼
      ┌───────────┐    ┌───────────┐    ┌───────────┐
      │  Agent A  │    │  Agent B  │    │  Agent C  │
      │  Session  │    │  Session  │    │  Session  │
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

### Lifecycle of a dispatch

1. **Dispatch** — `phantom d claude-a` creates a FUSE overlay, spawns the FUSE daemon in the background, and launches a Claude Code session inside the overlay mount point. The agent sees the full repository through the merged view (trunk + its own writes). A `.phantom-task.md` context file is written with agent metadata.

2. **Work** — The agent writes code inside the overlay. Reads fall through to trunk; writes are captured in the upper layer. Other agents can't see or interfere with its work. The session ID is captured from the CLI output for later resume.

3. **Pause / Resume** — The agent can exit the session at any time. Running `phantom d claude-a` again resumes the same coding session with the same overlay state. No work is lost.

4. **Submit** — `phantom sub claude-a` parses the modified files with tree-sitter, extracts what changed at the symbol level (functions added, structs modified, imports changed), and records a changeset in the event log.

5. **Materialize** — `phantom mat claude-a` runs a three-way semantic merge against trunk. If the symbols the agent touched are disjoint from what changed on trunk, it auto-merges. If the same symbol was modified by two agents, it reports a conflict.

6. **Ripple** — After materialization, Phantom performs a live rebase on other active agents whose files overlap with the materialized changes. Files that were modified by both the agent and trunk are semantically merged in place. All agents are notified of the trunk change via a notification file.

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
│   │   └── src/
│   │       ├── main.rs           # CLI entry point (clap)
│   │       ├── cli_adapter.rs    # Session capture per coding CLI
│   │       ├── context.rs        # PhantomContext loader
│   │       └── commands/         # Subcommand implementations
│   ├── phantom-core/             # Core types, traits, errors (zero phantom deps)
│   ├── phantom-events/           # SQLite event store (WAL mode), replay engine
│   ├── phantom-overlay/          # FUSE overlay filesystem, copy-on-write layer
│   ├── phantom-semantic/         # tree-sitter parsing, semantic merge engine
│   └── phantom-orchestrator/     # Git ops, materialization, ripple, live rebase
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
- [x] Session-aware dispatch with resume support
- [x] Auto-submit and auto-materialize flags
- [x] Live rebase on materialization
- [x] PTY-based session capture (Claude Code)
- [ ] Agent re-dispatch automation
- [ ] Incremental parsing for large codebases
- [ ] Additional CLI adapters (Aider, Cursor, Codex)
- [ ] macOS NFS overlay fallback
- [ ] Configuration file (`.phantom/config.toml` expansion)

## Contributing

Contributions are welcome. To get started:

1. Fork the repository
2. Install system dependencies (`libfuse3-dev` on Linux)
3. Run `cargo test` to verify everything passes
4. Create a branch and make your changes
5. Submit a pull request

See the [architecture documentation](docs/architecture.md) for design details.

## License

MIT

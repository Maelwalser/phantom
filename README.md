<div align="center">
<pre>
.----. .-. .-.  .--.  .-. .-. .---.  .----. .-.   .-.
| {}  }| {_} | / {} \ |  `| |{_   _}/  {}  \|  `.'  |
| .--' | { } |/  /\  \| |\  |  | |  \      /| |\ /| |
`-'    `-' `-'`-'  `-'`-' `-'  `-'   `----' `-' ` `-'
</pre>
</div>

**A version control designed for AI coding tools**<br/>
Phantom is an event-sourced, semantic-aware version control layer for agentic AI development, built on top of Git. It enables multiple AI coding agents to work on the same codebase simultaneously with automatic symbol-level conflict detection, FUSE-based filesystem isolation, and instant propagation of finished work.

<p align="center">
  <img src="docs/assets/demo.gif" alt="Phantom CLI demo" width="800" />
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> &middot;
  <a href="#installation">Installation</a> &middot;
  <a href="#commands">Commands</a> &middot;
  <a href="#sessions">Sessions</a> &middot;
  <a href="#how-it-works">How It Works</a> &middot;
  <a href="#supported-languages">Languages</a> &middot;
  <a href="#development">Development</a> &middot;
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

- **Session-aware tasking** — Each `phantom <agent>` launches a coding session (defaults to Claude Code) inside the overlay. Sessions are automatically captured and persisted, so re-entering the same agent resumes exactly where it left off.
- **Semantic merging** — Conflict detection at the AST level via tree-sitter. Two agents adding different functions to the same file? No conflict.
- **FUSE overlays** — Each agent gets an isolated copy-on-write filesystem. Reads fall through to trunk; writes are captured. No git branches, no rebasing.
- **Event sourcing** — Every agent action is an immutable event in an append-only log. Full auditability, surgical rollback, and "what-if" replay.
- **Live rebase** — When one agent's work is materialized, Phantom automatically rebases other agents' overlapping files at the symbol level and notifies them of trunk changes.
- **Auto-submit** — Use `--auto-submit` (or its alias `--auto-materialize`) to automatically submit and merge an agent's work when the session exits.
- **AI-driven planning** — `phantom plan` decomposes a feature request into parallel agent tasks using an AI planner, then dispatches background agents.
- **AI-driven conflict resolution** — `phantom resolve` launches a background AI agent with three-way conflict context to automatically resolve merge conflicts.
- **Multi-language support** — Symbol extraction for Rust, TypeScript, JavaScript, Python, Go (including JSX/TSX), plus config formats (YAML, TOML, JSON, Bash, CSS, HCL/Terraform, Dockerfile, Makefile) via tree-sitter grammars.
- **Zero-config conflict resolution** — Disjoint symbol changes auto-merge. True conflicts (same function modified by two agents) are detected and reported clearly.

## Quick Start

```bash
# Install (requires libfuse3-dev on Linux)
cargo install --path crates/phantom-cli

# Initialize in any git repository
cd /path/to/your/repo
phantom init

# Launch an agent — opens an interactive Claude Code session
phantom agent-a

# Or launch in the background with a task description
phantom agent-b --background --task "add rate limiting"

# When done, submit (automatically merges to trunk)
phantom sub agent-a

# Agent B's overlay automatically sees Agent A's changes.
# If Agent B touched different symbols, it merges cleanly.
phantom sub agent-b

# Or auto-submit when the session exits
phantom agent-c --auto-submit

# Decompose a feature into parallel agents
phantom plan "add caching layer"

# Resolve conflicts automatically
phantom resolve agent-a
```

## Installation

### Prerequisites

- **Rust** toolchain (edition 2024, rust-version 1.85+)
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

| Command | Alias | Description |
|---------|-------|-------------|
| `phantom init` | | Initialize Phantom in the current git repository |
| `phantom <agent>` | | Create an overlay, bind a coding session, and assign a task |
| `phantom submit` | `sub` | Submit an agent's work: semantic merge and commit to trunk |
| `phantom plan` | | Decompose a feature into parallel agent tasks via AI planner |
| `phantom resolve` | `res` | Auto-resolve merge conflicts by launching a background AI agent |
| `phantom status` | `st` | Show active overlays, pending changesets, and trunk state |
| `phantom log` | `l` | Query the event log with filters |
| `phantom changes` | `c` | Show recent submits and materializations |
| `phantom rollback` | `rb` | Drop a changeset and revert its changes from trunk |
| `phantom background` | `b` | Watch background agents in real-time |
| `phantom destroy` | `rm` | Tear down an agent's overlay and FUSE mount |
| `phantom down` | | Unmount all FUSE overlays and remove `.phantom/` |

### `phantom init`

Initialize Phantom in an existing git repository. Creates the `.phantom/` directory with an event store, overlay root, and configuration. Adds `.phantom/` to `.gitignore` automatically.

```bash
cd /path/to/your/repo
phantom init
```

### `phantom <agent>`

Create an overlay and launch a coding session for an agent. Any unrecognized subcommand is treated as an agent name — there is no separate `task` keyword required.

If the agent already has an active overlay, the existing session is resumed instead of creating a new one.

```bash
# Interactive — launches Claude Code inside the overlay (default)
phantom agent-a

# Resume an existing agent's session (automatic if overlay exists)
phantom agent-a

# Background — create overlay without launching a session (--task required)
phantom agent-b --background --task "implement caching layer"

# Auto-submit and merge when the session exits
phantom agent-a --auto-submit

# Use a custom CLI instead of Claude Code
phantom agent-a --command aider

# Skip FUSE mounting (agent works via upper layer only)
phantom agent-a --no-fuse
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--background` / `-b` | Create overlay without launching a session (requires `--task`) |
| `--task` | Task description (required with `--background`, used in context file) |
| `--auto-submit` | Automatically submit and merge the changeset when the session exits (alias: `--auto-materialize`) |
| `--command` | Custom CLI command to run instead of `claude` |
| `--no-fuse` | Skip FUSE mounting; agent writes to the upper layer directly |

The agent works in `.phantom/overlays/<agent>/mount/` (FUSE merged view) or `.phantom/overlays/<agent>/upper/` (with `--no-fuse`). A `.phantom-task.md` context file is placed in the working directory with agent metadata and instructions.

### `phantom submit` / `sub`

Submit an agent's work and merge it to trunk in a single step. Phantom parses the modified files with tree-sitter, extracts semantic operations (functions added, structs modified, imports changed), records a changeset in the event log, and runs a three-way semantic merge against trunk.

```bash
phantom sub agent-a

# With a custom commit message
phantom sub agent-a -m "feat: add user authentication"
```

**Possible outcomes:**
- **Success** — Changes committed to trunk. Other agents' overlays are live-rebased if they touch the same files, and notified of trunk changes.
- **Conflict** — Two agents modified the same symbol. The changeset is marked conflicted; use `phantom resolve` or re-task the agent.

### `phantom plan`

Decompose a feature request into parallel agent tasks. An AI planner analyzes the codebase, breaks the work into independent domains, creates overlays for each, and dispatches background agents.

```bash
# Interactive — opens an editor for the description
phantom plan

# Inline description
phantom plan "add caching layer with Redis backend"

# Show the plan without dispatching
phantom plan "add caching" --dry-run

# Skip confirmation prompt
phantom plan "add caching" -y

# Don't auto-submit (agents wait for manual submit)
phantom plan "add caching" --no-submit
```

### `phantom resolve` / `res`

Auto-resolve merge conflicts by launching a background AI agent with three-way conflict context (base/ours/theirs). The agent receives a specialized `.phantom-task.md` with conflict resolution instructions.

```bash
phantom resolve agent-a
```

Guards against infinite resolution loops — if a resolution is already in progress, the command will tell you to wait or drop the changeset.

### `phantom rollback` / `rb`

Surgically remove a changeset. Phantom marks the changeset's events as dropped and creates a git revert commit. Identifies any downstream changesets that may need re-tasking.

```bash
# By changeset ID
phantom rb cs-0001-123456

# By agent name (interactive selection of their changesets)
phantom rb agent-a

# Interactive — menu of all materialized changesets
phantom rb
```

### `phantom log` / `l`

Query the append-only event log. Every agent action, every materialization, every conflict, and every live rebase is recorded.

```bash
# All recent events (default limit: 50)
phantom l

# Filter by agent
phantom l agent-b

# Filter by changeset
phantom l cs-0042-789012

# Filter by symbol
phantom l --symbol "handlers::handle_login"

# Filter by time
phantom l --since 2h

# Show full event details
phantom l -v

# Limit results
phantom l --limit 20

# Trace causal chain from a specific event
phantom l --trace 42
```

Duration units: `s` (seconds), `m` (minutes), `h` (hours), `d` (days).

### `phantom changes` / `c`

Show recent submits and materializations.

```bash
# Default: last 25 entries
phantom c

# Filter by agent
phantom c agent-a

# Custom limit
phantom c -n 10
```

### `phantom status` / `st`

View the current state: active agents, pending changesets, trunk HEAD, and event count.

```bash
# Overview of all agents
phantom st

# Detailed status for a specific agent
phantom st agent-a
```

### `phantom background` / `b`

Real-time watch view of all background agents. Shows each agent's run state, elapsed time, and task description. Refreshes on a configurable interval.

```bash
# Default refresh (1s)
phantom b

# Custom refresh interval
phantom b -n 5
```

### `phantom destroy` / `rm`

Remove an agent's overlay, unmount its FUSE filesystem, and clean up its resources (including persisted session data).

```bash
phantom rm agent-a
```

### `phantom down`

Tear down Phantom entirely: unmount all active FUSE overlays, kill all agent and monitor processes, and remove the `.phantom/` directory. This is the safe way to remove Phantom — running `rm -rf .phantom` while FUSE overlays are mounted is dangerous.

```bash
# With confirmation prompt
phantom down

# Skip confirmation
phantom down -f
```

## Sessions

Each task binds a **coding session** to the agent's overlay. This is a core concept in Phantom — agents don't just get isolated filesystems, they get persistent, resumable coding contexts.

### How sessions work

1. **First task** — `phantom agent-a` creates the overlay, spawns a FUSE mount, and launches an interactive coding CLI (Claude Code by default) inside the overlay directory. The CLI process runs in a PTY so Phantom can capture its session ID from the terminal output.

2. **Session capture** — When the coding CLI exits, Phantom extracts the session ID (e.g., Claude Code's `--resume` UUID) and persists it to `.phantom/overlays/<agent>/cli_session.json`.

3. **Resume task** — Running `phantom agent-a` again detects the existing overlay, loads the saved session ID, and launches the CLI with `--resume <session-id>`. The agent picks up exactly where it left off — same conversation context, same file state.

4. **Session lifecycle** — The session persists as long as the overlay exists. Submitting the changeset ends the session. Destroying the overlay (`phantom rm agent-a`) clears the session data.

### Supported CLIs

| CLI | Session Resume | How |
|-----|---------------|-----|
| Claude Code | Yes | Captures `--resume <UUID>` from output, passes it on next task |
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
- `phantom submit agent-a` — submit your changes and merge to trunk
- `phantom status` — view all agents and changesets
```

## How It Works

Phantom sits between your AI agents and the Git repository. Each agent gets an isolated FUSE overlay with a bound coding session. When an agent finishes, Phantom analyzes the changes at the AST level and merges them into trunk automatically.

```
                     ┌─────────────────┐
                     │  Orchestrator   │
                     └────────┬────────┘
                            task
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

### Lifecycle of a task

1. **Task** — `phantom agent-a` creates a FUSE overlay, spawns the FUSE daemon in the background, and launches a Claude Code session inside the overlay mount point. The agent sees the full repository through the merged view (trunk + its own writes). A `.phantom-task.md` context file is written with agent metadata.

2. **Work** — The agent writes code inside the overlay. Reads fall through to trunk; writes are captured in the upper layer. Other agents can't see or interfere with its work. The session ID is captured from the CLI output for later resume.

3. **Pause / Resume** — The agent can exit the session at any time. Running `phantom agent-a` again resumes the same coding session with the same overlay state. No work is lost.

4. **Submit** — `phantom sub agent-a` parses the modified files with tree-sitter, extracts what changed at the symbol level (functions added, structs modified, imports changed), records a changeset in the event log, and runs a three-way semantic merge against trunk. If the symbols the agent touched are disjoint from what changed on trunk, it auto-merges. If the same symbol was modified by two agents, it reports a conflict.

5. **Ripple** — After materialization, Phantom performs a live rebase on other active agents whose files overlap with the materialized changes. Files that were modified by both the agent and trunk are semantically merged in place. All agents are notified of the trunk change via a notification file.

### Conflict resolution

| Scenario | Result |
|----------|--------|
| Both agents add different symbols to the same file | Auto-merge |
| Both agents add different fields to the same struct | Auto-merge |
| Both agents modify the same function body | **Conflict** — re-task or `phantom resolve` |
| One modifies, other deletes the same symbol | **Conflict** — re-task or `phantom resolve` |
| Both add the same import | Auto-deduplicate |
| Additive insertions to the same collection (routes, middleware) | Auto-merge |

For files without tree-sitter grammar support, Phantom falls back to text-level three-way merge via `diffy`.

## Project Structure

```
phantom/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── phantom-cli/              # Binary — the `phantom` command
│   │   └── src/
│   │       ├── main.rs           # CLI entry point (clap)
│   │       ├── context.rs        # PhantomContext loader
│   │       └── commands/         # Subcommand implementations
│   ├── phantom-core/             # Core types, traits, errors (zero phantom deps)
│   ├── phantom-events/           # SQLite event store (WAL mode), projection, replay
│   ├── phantom-overlay/          # FUSE overlay filesystem, copy-on-write layer
│   ├── phantom-semantic/         # tree-sitter parsing, semantic merge engine
│   ├── phantom-orchestrator/     # Git ops, materialization, ripple, live rebase
│   ├── phantom-session/          # PTY management, CLI adapters, context files, post-session automation
│   └── phantom-testkit/          # Shared test utilities (builders, mocks, test repos)
├── tests/integration/            # End-to-end tests with real git repos
└── docs/                         # Architecture and design documentation
```

## Supported Languages

Phantom extracts symbols from source files using tree-sitter grammars:

**Programming Languages:**

| Language | Extensions | Extracted Symbols |
|----------|-----------|-------------------|
| Rust | `.rs` | Functions, structs, enums, traits, impls, imports, constants, modules, tests, macros, type aliases |
| TypeScript | `.ts`, `.js` | Functions, classes, interfaces, methods, imports, enums, type aliases |
| TSX/JSX | `.tsx`, `.jsx` | Functions, classes, interfaces, methods, imports, enums, type aliases |
| Python | `.py` | Functions, classes, methods, imports |
| Go | `.go` | Functions, methods (with receiver type), structs, interfaces, type declarations, imports |

**Config & Infrastructure Files:**

| Language | Extensions / Filenames | Extracted Symbols |
|----------|----------------------|-------------------|
| YAML | `.yml`, `.yaml` | Top-level keys |
| TOML | `.toml` | Top-level tables and keys |
| JSON | `.json` | Top-level keys |
| Bash | `.sh`, `.bash`, `.zsh` | Functions, aliases, variables |
| CSS | `.css` | Selectors, at-rules, custom properties |
| HCL/Terraform | `.tf`, `.hcl` | Resources, variables, outputs, modules |
| Dockerfile | `Dockerfile` | Stages, instructions |
| Makefile | `.mk`, `Makefile` | Targets, variables |

Files without a matching grammar fall back to text-level three-way merge via `diffy`.

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

The integration tests create temporary git repositories, task simulated agents, and verify semantic merging behavior end-to-end:

```bash
cargo test --test two_agents_disjoint       # Disjoint files auto-merge
cargo test --test two_agents_same_file      # Same file, different symbols
cargo test --test two_agents_same_symbol    # Same symbol = conflict detected
cargo test --test materialize_and_ripple    # Trunk propagation to active agents
cargo test --test rollback_replay           # Surgical rollback and replay
cargo test --test event_log_query           # Event log querying
```

## Roadmap

- [x] Core types and event store
- [x] FUSE overlay with copy-on-write
- [x] All CLI commands (init, task, submit, status, log, changes, rollback, destroy, background, down)
- [x] Semantic merging (Rust, TypeScript/JavaScript, Python, Go, YAML, TOML, JSON, Bash, CSS, HCL, Dockerfile, Makefile)
- [x] Integration test suite
- [x] Session-aware task with resume support
- [x] Auto-submit and auto-materialize flags
- [x] Live rebase on materialization
- [x] PTY-based session capture (Claude Code)
- [x] `phantom plan` — AI-driven multi-agent task decomposition
- [x] `phantom resolve` — AI-driven conflict resolution
- [ ] Agent re-task automation
- [ ] Incremental parsing for large codebases
- [ ] Additional CLI adapters (Aider, Cursor, Codex)
- [ ] macOS NFS overlay fallback
- [ ] Configuration file (`.phantom/config.toml`)
- [ ] Benchmarks

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

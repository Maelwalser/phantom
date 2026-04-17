<div align="center">
<pre>
.----. .-. .-.  .--.  .-. .-. .---.  .----. .-.   .-.
| {}  }| {_} | / {} \ |  `| |{_   _}/  {}  \|  `.'  |
| .--' | { } |/  /\  \| |\  |  | |  \      /| |\ /| |
`-'    `-' `-'`-'  `-'`-' `-'  `-'   `----' `-' ` `-'
</pre>
</div>

**A version control designed for AI coding tools**<br/>
Built on top of Git, Phantom allows multiple AI tools to safely edit a single codebase at the same time. It uses event-sourcing and isolated file systems to track and separate active work, while automatically resolving conflicts at the logic level before instantly syncing finished code.

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

## Goals

Phantom is a proof of concept for a new kind of version control, one designed for how AI code development actually works.

### Problems Phantom is trying to solve

- **Git branches are the wrong primitive for agents.** Branches were designed for long-lived, human-driven divergence with manual reconciliation at the end. Agents produce short-lived, fine-grained units of work that should compose automatically.
- **Line-based merges don't understand code.** Two agents adding different functions to the same file shouldn't conflict, but text-level diffing says they do. The merge should happen at the level of *symbols*, not *lines*.
- **Working trees don't isolate.** When multiple agents share a checkout, one agent's in-progress write becomes another agent's unstable read. Each agent needs its own view of the repository without the overhead of full clones.
- **There's no audit trail for autonomous work.** When an agent rewrites a codebase unattended, you need to know exactly what happened, when, and why — and you need to be able to roll any single change back cleanly.
- **Agent context gets lost between sessions.** Restarting an agent shouldn't mean rebuilding its mental model from scratch. Sessions should be first-class and resumable.

### What Phantom is exploring

- **Changesets instead of branches** — atomic, reorderable, auditable units of work keyed by symbols rather than lines.
- **FUSE overlays instead of worktrees** — per-agent copy-on-write filesystems with read-through to trunk.
- **Semantic merging instead of textual merging** — tree-sitter-powered AST diffing so disjoint symbol changes compose automatically.
- **Event sourcing instead of reflog** — every action is an immutable event, enabling surgical rollback, replay, and "what-if" analysis.
- **Session-aware tasking** — each agent has a persistent, resumable coding session bound to its overlay.

Phantom is not a replacement for Git. It sits on top of Git and uses it as the durable source of truth. The goal is to find out which of these ideas are worth keeping if we ever build a version control system from scratch for a world where most code is written by agents.

## Features

- **Session-aware tasking** — Each `ph <agent>` launches a coding session (defaults to Claude Code) inside the overlay. Sessions are automatically captured and persisted, so re-entering the same agent resumes exactly where it left off.
- **Semantic merging** — Conflict detection at the AST level via tree-sitter. Two agents adding different functions to the same file? No conflict.
- **FUSE overlays** — Each agent gets an isolated copy-on-write filesystem. Reads fall through to trunk; writes are captured. No git branches, no rebasing.
- **Event sourcing** — Every agent action is an immutable event in an append-only SQLite log. Full auditability, surgical rollback, and "what-if" replay.
- **Live rebase** — When one agent's work is submitted, Phantom automatically rebases other agents' overlapping files at the symbol level and notifies them of trunk changes.
- **Auto-submit** — Use `--auto-submit` (or its alias `--auto-materialize`) to automatically submit and merge an agent's work when the session exits. Always on for background agents.
- **Background agents** — Run agents headless with `--background --task "..."`. A monitor process waits for completion and (optionally) auto-submits. Watch progress with `ph background`.
- **Agent dependencies** — Background agents can be configured to wait for upstream agents to submit and materialize to trunk before they start. The monitor polls the event log for upstream `ChangesetMaterialized` events, bails if any upstream is conflicted or dropped, and once deps resolve refreshes the dependent agent's base commit and context file so it sees the upstream work. A waiting agent surfaces in `ph status` and `ph background` with its upstream list, and emits an `AgentWaitingForDependencies` event for audit. Used by `ph plan` to sequence waves (e.g. a scaffold domain that owns shared config runs first, feature domains wait on it).
- **AI-driven planning** *(experimental)* — `ph plan` decomposes a feature request into parallel agent tasks using an AI planner, then dispatches background agents — including dependency edges between domains so conflicting waves run sequentially while disjoint work runs in parallel.
- **AI-driven conflict resolution** *(experimental)* — `ph resolve` launches a background AI agent with three-way conflict context to automatically resolve merge conflicts.
- **Multi-language support** — Symbol extraction for Rust, TypeScript, JavaScript (including JSX/TSX), Python, and Go, plus config formats (YAML, TOML, JSON, Bash, CSS, HCL/Terraform, Dockerfile, Makefile) via tree-sitter grammars.
- **Zero-config conflict resolution** — Disjoint symbol changes auto-merge. True conflicts (same function modified by two agents) are detected and reported clearly.

## Quick Start

```bash
# Install (requires libfuse3-dev on Linux)
cargo install --path crates/phantom-cli

# Initialize in any git repository
cd /path/to/your/repo
ph init

# Launch an agent — opens an interactive Claude Code session
ph agent-a

# Or launch in the background with a task description
ph agent-b --background --task "add rate limiting"

# When done, submit (automatically merges to trunk)
ph sub agent-a

# Agent B's overlay automatically sees Agent A's changes.
# If Agent B touched different symbols, it merges cleanly.
ph sub agent-b

# Or auto-submit when the session exits
ph agent-c --auto-submit

# Decompose a feature into parallel agents (experimental)
ph plan "add caching layer"

# Resolve conflicts automatically (experimental)
ph resolve agent-a
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
ph --help
```

## Commands

| Command | Description |
|---------|-------------|
| `ph init` | Initialize Phantom in the current git repository |
| `ph <agent>` | Create an overlay, bind a coding session, and assign a task |
| `ph submit/sub` | Submit an agent's work: semantic merge and commit to trunk |
| `ph tasks/t` | List all agent task overlays |
| `ph resume/re` | Select and resume an interactive agent session |
| `ph plan` | Decompose a feature into parallel agent tasks via AI planner **(experimental)** |
| `ph resolve/res` | Auto-resolve merge conflicts by launching a background AI agent **(experimental)** |
| `ph status/st` | Show active overlays, pending changesets, and trunk state |
| `ph log/l` | Query the event log with filters |
| `ph changes/c` | Show recent submits and materializations |
| `ph rollback/rb` | Drop a changeset and revert its changes from trunk |
| `ph background/b` | Watch background agents in real-time |
| `ph exec/x` | Run an arbitrary command inside an agent's overlay view |
| `ph destroy/rm` | Tear down an agent's overlay and FUSE mount |
| `ph down` | Unmount all FUSE overlays and remove `.phantom/` |

### `ph init`

Initialize Phantom in an existing git repository. Creates the `.phantom/` directory with an event store, overlay root, and configuration. Adds `.phantom/` to `.gitignore` automatically.

```bash
cd /path/to/your/repo
ph init
```

### `ph <agent>`

Create an overlay and launch a coding session for an agent. Any unrecognized subcommand is treated as an agent name — there is no separate `task` keyword required.

If the agent already has an active overlay, the existing session is resumed instead of creating a new one.

```bash
# Interactive — launches Claude Code inside the overlay (default)
ph agent-a

# Resume an existing agent's session (automatic if overlay exists)
ph agent-a

# Background — create overlay without launching a session (--task required)
ph agent-b --background --task "implement caching layer"

# Auto-submit and merge when the session exits
ph agent-a --auto-submit

# Use a custom CLI instead of Claude Code
ph agent-a --command aider

# Skip FUSE mounting (agent works via upper layer only)
ph agent-a --no-fuse
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

### `ph submit` / `sub`

Submit an agent's work and merge it to trunk in a single step. Phantom parses the modified files with tree-sitter, extracts semantic operations (functions added, structs modified, imports changed), records a changeset in the event log, and runs a three-way semantic merge against trunk.

```bash
ph sub agent-a

# With a custom commit message
ph sub agent-a -m "feat: add user authentication"
```

**Possible outcomes:**
- **Success** — Changes committed to trunk. Other agents' overlays are live-rebased if they touch the same files, and notified of trunk changes.
- **Conflict** — Two agents modified the same symbol. The changeset is marked conflicted; use `ph resolve` or re-task the agent.

### `ph tasks` / `t`

List all active agent task overlays with their status, changeset, and task description.

```bash
ph t
```

### `ph resume` / `re`

Select and resume an interactive agent session. Presents a menu of idle (non-background) agents and relaunches the coding session with the saved session ID.

```bash
ph re
```

### `ph plan` *(experimental)*

Decompose a feature request into parallel agent tasks. An AI planner analyzes the codebase, breaks the work into independent domains, creates overlays for each, and dispatches background agents. Domains can declare `depends_on` relationships — agents in later waves wait for their upstream agents to submit and materialize to trunk before starting, and refresh their base commit onto the updated trunk once upstream work lands. If any upstream agent fails, dependent agents abort rather than starting against stale state.

> **Note:** This command is experimental and under active development.

```bash
# Interactive — opens an editor for the description
ph plan

# Inline description
ph plan "add caching layer with Redis backend"

# Show the plan without dispatching
ph plan "add caching" --dry-run

# Skip confirmation prompt
ph plan "add caching" -y

# Don't auto-submit (agents wait for manual submit)
ph plan "add caching" --no-submit
```

### `ph resolve` / `res` *(experimental)*

Auto-resolve merge conflicts by launching a background AI agent with three-way conflict context (base/ours/theirs). The agent receives a specialized `.phantom-task.md` with conflict resolution instructions.

> **Note:** This command is experimental and under active development.

```bash
ph resolve agent-a
```

Guards against infinite resolution loops, if a resolution is already in progress, the command will tell you to wait or drop the changeset.

### `ph rollback` / `rb`

Surgically remove a changeset. Phantom marks the changeset's events as dropped and creates a git revert commit. Identifies any downstream changesets that may need re-tasking.

```bash
# By changeset ID
ph rb cs-0001-123456

# By agent name (interactive selection of their changesets)
ph rb agent-a

# Interactive — menu of all materialized changesets
ph rb
```

### `ph log` / `l`

Query the append-only event log. Every agent action, every materialization, every conflict, and every live rebase is recorded.

```bash
# All recent events (default limit: 50)
ph l

# Filter by agent
ph l agent-b

# Filter by changeset
ph l cs-0042-789012

# Filter by symbol
ph l --symbol "handlers::handle_login"

# Filter by time
ph l --since 2h

# Show full event details
ph l -v

# Limit results
ph l --limit 20

# Trace causal chain from a specific event
ph l --trace 42
```

Duration units: `s` (seconds), `m` (minutes), `h` (hours), `d` (days).

### `ph changes` / `c`

Show recent submits and materializations.

```bash
# Default: last 25 entries
ph c

# Filter by agent
ph c agent-a

# Custom limit
ph c -n 10
```

### `ph status` / `st`

View the current state: active agents, pending changesets, trunk HEAD, and event count.

```bash
# Overview of all agents
ph st

# Detailed status for a specific agent
ph st agent-a
```

### `ph background` / `b`

Real-time watch view of all background agents. Shows each agent's run state, elapsed time, and task description. Refreshes on a configurable interval.

```bash
# Default refresh (1s)
ph b

# Custom refresh interval
ph b -n 5
```

### `ph exec` / `x`

Run an arbitrary command inside an agent's overlay, seeing the merged trunk + agent view. If the overlay's FUSE mount is not already active, it is mounted temporarily for the duration of the command and unmounted afterwards.

```bash
# Run a build inside the agent's view
ph exec agent-a -- cargo build

# Inspect a file the agent modified (without touching trunk)
ph x agent-b -- cat src/lib.rs

# Run a script with the agent's working tree as cwd
ph x agent-c -- ./scripts/lint.sh
```

Environment variables (`PHANTOM_AGENT_ID`, `PHANTOM_OVERLAY_DIR`, `PHANTOM_REPO_ROOT`) are exported into the spawned process.

### `ph destroy` / `rm`

Remove an agent's overlay, unmount its FUSE filesystem, and clean up its resources (including persisted session data).

```bash
ph rm agent-a
```

> **Note:** `ph destroy` is destructive and immediate — there is no confirmation
> prompt. The overlay, FUSE mount, and persisted session data are removed as
> soon as the command runs. If you want a prompted teardown of the entire
> `.phantom/` directory, use `ph down` instead.

### `ph down`

**Prompts for confirmation unless `-f` is passed.** Tears down Phantom entirely: unmount all active FUSE overlays, kill all agent and monitor processes, and remove the `.phantom/` directory. This is the safe way to remove Phantom — running `rm -rf .phantom` while FUSE overlays are mounted is dangerous.

```bash
# With confirmation prompt
ph down

# Skip confirmation
ph down -f
```

## Sessions

Each task binds a **coding session** to the agent's overlay. This is a core concept in Phantom — agents don't just get isolated filesystems, they get persistent, resumable coding contexts.

### How sessions work

1. **First task** — `ph agent-a` creates the overlay, spawns a FUSE mount, and launches an interactive coding CLI (Claude Code by default) inside the overlay directory. The CLI process runs in a PTY so Phantom can capture its session ID from the terminal output.

2. **Session capture** — When the coding CLI exits, Phantom extracts the session ID (e.g., Claude Code's `--resume` UUID) and persists it to `.phantom/overlays/<agent>/cli_session.json`.

3. **Resume task** — Running `ph agent-a` again detects the existing overlay, loads the saved session ID, and launches the CLI with `--resume <session-id>`. The agent picks up exactly where it left off — same conversation context, same file state.

4. **Session lifecycle** — The session persists as long as the overlay exists. Submitting the changeset ends the session. Destroying the overlay (`ph rm agent-a`) clears the session data.

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
- `ph submit agent-a` — submit your changes and merge to trunk
- `ph status` — view all agents and changesets
```

## How It Works

Phantom sits between your AI agents and the Git repository. Each agent gets an isolated FUSE overlay with a bound coding session. When an agent finishes, Phantom analyzes the changes at the AST level and merges them into trunk automatically.

```
   ┌──────────┐   ┌──────────┐   ┌──────────┐
   │ Agent A  │   │ Agent B  │   │ Agent C  │    ← Claude / Aider / any CLI
   └────┬─────┘   └────┬─────┘   └────┬─────┘
        │              │              │
        ▼              ▼              ▼
   ┌──────────┐   ┌──────────┐   ┌──────────┐
   │ FUSE COW │   │ FUSE COW │   │ FUSE COW │    ← per-agent isolated fs
   │  overlay │   │  overlay │   │  overlay │       upper = writes
   └────┬─────┘   └────┬─────┘   └────┬─────┘       lower = trunk
        │              │              │
        │ ph sub       │              │
        ▼              ▼              ▼
   ╔══════════════════════════════════════╗
   ║        Submit + Materialize          ║
   ║  parse → symbols → 3-way semantic    ║    ← tree-sitter + semantic
   ║  merge → git commit → ripple         ║       merge engine
   ╚════════════════╦═════════════════════╝
                    │
            live rebase + notification
            (to other agents' overlays
             whose files overlap)
                    │
                    ▼
         ┌────────────────────┐
         │  Trunk (git main)  │ ────append───►  ┌────────────────────┐
         └────────────────────┘                 │ Event Log (SQLite) │
                                                │  append-only WAL   │
                                                └────────────────────┘
                                                auditable, replayable,
                                                rollback-ready
```

### The merge problem Phantom solves

```
  Git sees text lines:                Phantom sees symbols:

  handlers.rs                         handlers.rs
  ─────────────────────────           ─────────────────────────────────────
  <<<<<<< claude-a                     fn handle_login()     [unchanged]
  + fn handle_register()               fn handle_register()  [claude-a added]
  =======                              fn handle_admin()     [claude-b added]
  + fn handle_admin()                 ─────────────────────────────────────
  >>>>>>> claude-b                    → Disjoint symbol set. Auto-merge.
  ─────────────────────────
  CONFLICT — manual fix needed
```

### Lifecycle of a task

1. **Task** — `ph agent-a` creates a FUSE overlay, spawns the FUSE daemon in the background, and launches a Claude Code session inside the overlay mount point. The agent sees the full repository through the merged view (trunk + its own writes). A `.phantom-task.md` context file is written with agent metadata.

2. **Work** — The agent writes code inside the overlay. Reads fall through to trunk; writes are captured in the upper layer. Other agents can't see or interfere with its work. The session ID is captured from the CLI output for later resume.

3. **Pause / Resume** — The agent can exit the session at any time. Running `ph agent-a` again resumes the same coding session with the same overlay state. No work is lost.

4. **Submit** — `ph sub agent-a` parses the modified files with tree-sitter, extracts what changed at the symbol level (functions added, structs modified, imports changed), records a `ChangesetSubmitted` event, and runs a three-way semantic merge against trunk in a single step. If the symbols the agent touched are disjoint from what changed on trunk, it auto-merges and commits. If the same symbol was modified by two agents, the changeset is marked **Conflicted** and can be re-tasked or resolved with `ph resolve`.

5. **Ripple** — Immediately after a successful submit, Phantom performs a live rebase on other active agents whose files overlap with the newly merged changes. Files that were modified by both the agent and trunk are semantically merged in place in the other agents' upper layers. All active agents are notified of the trunk change via a `.phantom/overlays/<agent>/trunk-notifications/` file, so their running sessions can discover trunk updates.

### Conflict resolution

| Scenario | Result |
|----------|--------|
| Both agents add different symbols to the same file | Auto-merge |
| Both agents add different fields to the same struct | Auto-merge |
| Both agents modify the same function body | **Conflict** — re-task or `ph resolve` |
| One modifies, other deletes the same symbol | **Conflict** — re-task or `ph resolve` |
| Both add the same import | Auto-deduplicate |
| Additive insertions to the same collection (routes, middleware) | Auto-merge |

For files without tree-sitter grammar support, Phantom falls back to text-level three-way merge via `diffy`.

## Project Structure

```
phantom/
├── Cargo.toml                    # Workspace root (9 crates + integration tests)
├── crates/
│   ├── phantom-cli/              # Binary — the `ph` command (clap-based)
│   │   └── src/
│   │       ├── main.rs           # CLI entry point
│   │       ├── cli.rs            # Argument definitions and subcommand routing
│   │       ├── context.rs        # PhantomContext loader (.phantom/ discovery)
│   │       ├── fs/               # FUSE mount helpers
│   │       ├── services/         # Validation and shared service logic
│   │       ├── ui/               # Terminal styling helpers
│   │       └── commands/         # One module per subcommand
│   ├── phantom-core/             # Core types, traits, errors (zero phantom deps)
│   ├── phantom-git/              # Git operations (git2 wrapper, tree building, text merge)
│   ├── phantom-events/           # SQLite event store (WAL mode), projection, replay
│   ├── phantom-overlay/          # FUSE overlay filesystem, copy-on-write layer
│   ├── phantom-semantic/         # tree-sitter parsing, semantic merge engine
│   ├── phantom-orchestrator/     # Materialization, ripple, live rebase, submit service
│   ├── phantom-session/          # PTY management, CLI adapters, context files, post-session automation
│   └── phantom-testkit/          # Shared test utilities (builders, mocks, test repos)
├── tests/integration/            # End-to-end tests with real git repos
└── docs/                         # Architecture, event model, semantic merge design
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
cargo test --test two_agents_disjoint            # Disjoint files auto-merge
cargo test --test two_agents_same_file           # Same file, different symbols
cargo test --test two_agents_same_symbol         # Same symbol = conflict detected
cargo test --test materialize_and_ripple         # Trunk propagation to active agents
cargo test --test rollback_replay                # Surgical rollback and replay
cargo test --test event_log_query                # Event log querying
cargo test --test semantic_merge_fallback        # Text fallback for unsupported languages
cargo test --test empty_submit_is_noop           # Zero-change submits are no-ops
cargo test --test concurrent_submit_overlay_write # Race between submit and agent writes
cargo test --test corrupted_event_store          # Recovery from corrupted event DB
cargo test --test materialize_append_crash       # Crash mid-materialization
```

## Roadmap

- [x] Core types and event store
- [x] FUSE overlay with copy-on-write
- [x] All CLI commands (init, task, submit, status, log, changes, rollback, destroy, background, exec, down)
- [x] Semantic merging (Rust, TypeScript/JavaScript/TSX/JSX, Python, Go, YAML, TOML, JSON, Bash, CSS, HCL, Dockerfile, Makefile)
- [x] Integration test suite
- [x] Session-aware tasking with resume support (PTY-based capture for Claude Code)
- [x] Auto-submit / auto-materialize flag
- [x] Live rebase and ripple notifications on submit
- [x] `ph plan` — AI-driven multi-agent task decomposition *(experimental)*
- [x] `ph resolve` — AI-driven conflict resolution *(experimental)*
- [x] Minimal `.phantom/config.toml` (`default_cli` only)
- [ ] Full configuration schema (`.phantom/config.toml`)
- [ ] Agent re-task automation
- [ ] Incremental parsing for large codebases
- [ ] Additional CLI adapters (Aider, Cursor, Codex)
- [ ] macOS NFS overlay fallback
- [ ] Benchmarks

## Contributing

Contributions are welcome. To get started:

1. Fork the repository
2. Install system dependencies (`libfuse3-dev` on Linux)
3. Run `cargo test` to verify everything passes
4. Create a branch and make your changes
5. Submit a pull request

See the design documentation for details:
- [architecture.md](docs/architecture.md) — crate layout and data flow
- [event-model.md](docs/event-model.md) — event schema and replay semantics
- [semantic-merge.md](docs/semantic-merge.md) — symbol extraction and three-way merge
- [manual-tests.md](docs/manual-tests.md) — manual test scenarios

## License

MIT

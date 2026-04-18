<div align="center">
<pre>
.----. .-. .-.  .--.  .-. .-. .---.  .----. .-.   .-.
| {}  }| {_} | / {} \ |  `| |{_   _}/  {}  \|  `.'  |
| .--' | { } |/  /\  \| |\  |  | |  \      /| |\ /| |
`-'    `-' `-'`-'  `-'`-' `-'  `-'   `----' `-' ` `-'
</pre>
</div>

A version control layer for AI coding tools. Built on Git, Phantom lets multiple AI agents edit the same repository in parallel, with semantic (AST-level) merging instead of line-based merging.

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
  <a href="#development">Development</a>
</p>

---

## How it differs from Git

- **Changesets** instead of branches — atomic units keyed by symbols.
- **FUSE overlays** instead of worktrees — per-agent copy-on-write filesystems.
- **Semantic merge** instead of textual merge — tree-sitter AST diffing.
- **Event log** instead of reflog — append-only SQLite, replayable and rollback-ready.
- **Resumable sessions** bound to each overlay.

Phantom sits on top of Git. Git remains the source of truth.

## Quick Start

```bash
# Install (Linux requires libfuse3-dev)
cargo install --path crates/phantom-cli

# Initialize in a git repo
cd /path/to/your/repo
ph init

# Launch an interactive agent (Claude Code by default)
ph agent-a

# Or run in the background
ph agent-b --background --task "add rate limiting"

# Submit and merge to trunk
ph sub agent-a

# Decompose a feature into parallel agents
ph plan "add caching layer"

# Auto-resolve conflicts
ph resolve agent-a
```

## Installation

### Prerequisites

- Rust toolchain (edition 2024, 1.88+)
- Git
- Linux: `libfuse3-dev` and `pkg-config`

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
cargo install --path crates/phantom-cli
```

## Commands

| Command | Description |
|---------|-------------|
| `ph init` | Initialize Phantom in the current git repo |
| `ph <agent>` | Create or resume an agent overlay and session |
| `ph submit` / `sub` | Submit an agent's work and merge to trunk |
| `ph tasks` / `t` | List all agent overlays |
| `ph resume` / `re` | Resume an interactive agent session |
| `ph plan` | Decompose a feature into parallel agents *(experimental)* |
| `ph resolve` / `res` | Auto-resolve merge conflicts via AI agent *(experimental)* |
| `ph status` / `st` | Show overlays, changesets, trunk state |
| `ph log` / `l` | Query the event log |
| `ph changes` / `c` | Show recent submits and materializations |
| `ph rollback` / `rb` | Drop a changeset and revert it |
| `ph background` / `b` | Watch background agents |
| `ph exec` / `x` | Run a command inside an agent's overlay view |
| `ph remove` / `rm` | Remove an agent's overlay (immediate, no prompt) |
| `ph down` | Unmount everything and remove `.phantom/` |

### `ph <agent>`

Any unrecognized subcommand is treated as an agent name. If the overlay already exists, the session is resumed.

```bash
ph agent-a                                       # interactive
ph agent-b --background --task "implement caching"
ph agent-a --auto-submit                         # submit on session exit
ph agent-a --command aider                       # use a different CLI
ph agent-a --no-fuse                             # write directly to upper layer
```

| Flag | Description |
|------|-------------|
| `--background` / `-b` | Create overlay without launching a session (requires `--task`) |
| `--task` | Task description |
| `--auto-submit` | Submit and merge when the session exits |
| `--command` | CLI command to run instead of `claude` |
| `--no-fuse` | Skip FUSE mounting |

### `ph submit` / `sub`

Parses modified files, extracts semantic operations, runs three-way semantic merge against trunk, and commits.

```bash
ph sub agent-a
ph sub agent-a -m "feat: add user auth"
```

Outcomes: **Success** (committed, ripple to other agents) or **Conflict** (use `ph resolve` or re-task).

### `ph plan` *(experimental)*

Splits a feature request into parallel agents via an AI planner. Domains can declare `depends_on` so dependent agents wait for upstream materialization before starting.

```bash
ph plan                                  # opens editor
ph plan "add caching layer"
ph plan "add caching" --dry-run          # preview only
ph plan "add caching" -y                 # skip confirmation
ph plan "add caching" --no-submit        # disable auto-submit
```

### `ph resolve` / `res` *(experimental)*

Launches a background AI agent with three-way conflict context (base/ours/theirs).

```bash
ph resolve agent-a
```

### `ph rollback` / `rb`

Marks a changeset's events as dropped and creates a git revert. Reports downstream changesets that may need re-tasking.

```bash
ph rb cs-0001-123456
ph rb agent-a            # interactive selection
ph rb                    # menu of all materialized changesets
```

### `ph log` / `l`

```bash
ph l                              # last 50 events
ph l agent-b                      # filter by agent
ph l cs-0042-789012               # filter by changeset
ph l --symbol "handlers::handle_login"
ph l --since 2h                   # s, m, h, d
ph l -v                           # full details
ph l --limit 20
ph l --trace 42                   # causal chain
```

### `ph exec` / `x`

Runs a command inside an agent's overlay view. Mounts FUSE temporarily if needed.

```bash
ph exec agent-a -- cargo build
ph x agent-b -- cat src/lib.rs
```

Sets `PHANTOM_AGENT_ID`, `PHANTOM_OVERLAY_DIR`, `PHANTOM_REPO_ROOT` in the spawned process.

### `ph remove` / `rm`

Removes an overlay, FUSE mount, and persisted session data. **No confirmation prompt.** Use `ph down` to remove `.phantom/` entirely with a prompt.

### `ph down`

Unmounts all overlays, kills agent and monitor processes, and removes `.phantom/`. Prompts unless `-f` is passed.

## Sessions

Each overlay binds a coding session (Claude Code by default). The session ID is captured from the CLI's output via PTY and persisted to `.phantom/overlays/<agent>/cli_session.json`. Re-running `ph <agent>` resumes the session with `--resume <id>`.

| CLI | Resume |
|-----|--------|
| Claude Code | Yes — captures `--resume <UUID>` |
| Custom (`--command`) | No |

A `.phantom-task.md` is placed in the overlay with agent metadata and available commands.

Environment variables in the session: `PHANTOM_AGENT_ID`, `PHANTOM_CHANGESET_ID`, `PHANTOM_OVERLAY_DIR`, `PHANTOM_REPO_ROOT`, `PHANTOM_INTERACTIVE`.

## How It Works

### Pipeline

```
   ┌──────────┐   ┌──────────┐   ┌──────────┐
   │ Agent A  │   │ Agent B  │   │ Agent C  │    ← Claude / Aider / any CLI
   └────┬─────┘   └────┬─────┘   └────┬─────┘
        ▼              ▼              ▼
   ┌──────────┐   ┌──────────┐   ┌──────────┐
   │ FUSE COW │   │ FUSE COW │   │ FUSE COW │    ← per-agent overlays
   │  overlay │   │  overlay │   │  overlay │       upper = writes
   └────┬─────┘   └────┬─────┘   └────┬─────┘       lower = trunk
        │ ph sub       │              │
        ▼              ▼              ▼
   ╔══════════════════════════════════════╗
   ║               Submit                 ║
   ║  parse → symbols → 3-way semantic    ║
   ║  merge → git commit → ripple ▼       ║
   ╚════════════════╦═════════════════════╝
                    ▼
         ┌────────────────────┐          ┌────────────────────┐
         │  Trunk (git main)  │ ─append→ │ Event Log (SQLite) │
         └────────────────────┘          └────────────────────┘
```

### Ripple — trunk updates propagate to every active agent

When `agent-a` lands a changeset on trunk, Phantom walks every other live overlay and
refreshes it in place. Agents whose upper layer has touched the same file get a
live rebase and a notification file dropped next to their work.

```
                     ph sub agent-a ──▶ trunk: commit X ──▶ commit Y
                                                │
                                                │ ripple
             ┌──────────────────────────────────┼──────────────────────────────────┐
             ▼                                  ▼                                  ▼
       ┌───────────┐                      ┌───────────┐                      ┌───────────┐
       │  agent-b  │                      │  agent-c  │                      │  agent-d  │
       │           │                      │           │                      │           │
       │ upper: ∅  │                      │ upper:    │                      │ upper:    │
       │           │                      │  src/a.rs │                      │  src/z.rs │
       │ lower: X  │                      │ lower: X  │                      │ lower: X  │
       └─────┬─────┘                      └─────┬─────┘                      └─────┬─────┘
             │                                  │                                  │
             │ no overlap                       │ overlap on src/a.rs              │ no overlap
             ▼                                  ▼                                  ▼
       ┌───────────┐                      ┌──────────────────┐                ┌───────────┐
       │ upper: ∅  │                      │ 3-way live rebase│                │ upper:    │
       │ lower: Y ✓│                      │  base   = X:a.rs │                │  src/z.rs │
       │           │                      │  ours   = upper  │                │ lower: Y ✓│
       │ silent    │                      │  theirs = Y:a.rs │                │           │
       │ refresh   │                      │      ↓           │                │ silent    │
       │           │                      │ upper: a.rs      │                │ refresh   │
       │           │                      │ lower: Y ✓       │                │           │
       │           │                      │                  │                │           │
       │           │                      │ ⚠ .phantom-      │                │           │
       │           │                      │   trunk.md drop  │                │           │
       └───────────┘                      └──────────────────┘                └───────────┘
             ▲                                  ▲                                  ▲
             │                                  │                                  │
        TrunkVisible                     RebaseMerged                        TrunkVisible
                                          (or RebaseConflict
                                           if merge fails)
```

Per-file outcomes recorded in each agent's `.phantom-trunk.md`:

| Scenario in the agent's upper layer         | Result                                  | Status              |
|---------------------------------------------|-----------------------------------------|---------------------|
| File not touched locally                    | Lower refreshes silently                | `TrunkVisible`      |
| File edited, clean 3-way merge against trunk | Upper rewritten with merged bytes       | `RebaseMerged`      |
| File edited, trunk change is non-overlapping | Upper kept, both changes reconcilable   | `Shadowed`          |
| File edited, merge can't auto-resolve        | Upper kept, agent notified to resolve   | `RebaseConflict`    |

### Conflict resolution

| Scenario | Result |
|----------|--------|
| Different symbols, same file | Auto-merge |
| Different fields, same struct | Auto-merge |
| Same function body modified twice | **Conflict** |
| Modify vs delete same symbol | **Conflict** |
| Same import added twice | Auto-deduplicate |
| Additive insertions to same collection | Auto-merge |

Files without a tree-sitter grammar fall back to text-level three-way merge via `diffy`.

## Project Structure

```
crates/
├── phantom-cli/              # `ph` binary
├── phantom-core/             # types, traits, errors (zero phantom deps)
├── phantom-git/              # git2 wrapper
├── phantom-events/           # SQLite WAL event store
├── phantom-overlay/          # FUSE overlay
├── phantom-semantic/         # tree-sitter + semantic merge
├── phantom-orchestrator/     # materialize, ripple, live rebase, submit
├── phantom-session/          # PTY, CLI adapters, context files
└── phantom-testkit/          # test utilities
tests/integration/            # end-to-end tests
```

## Supported Languages

**Programming:**

| Language | Extensions |
|----------|-----------|
| Rust | `.rs` |
| TypeScript / JavaScript | `.ts`, `.js`, `.tsx`, `.jsx` |
| Python | `.py` |
| Go | `.go` |

**Config:**

| Format | Extensions |
|--------|-----------|
| YAML | `.yml`, `.yaml` |
| TOML | `.toml` |
| JSON | `.json` |
| Bash | `.sh`, `.bash`, `.zsh` |
| CSS | `.css` |
| HCL / Terraform | `.tf`, `.hcl` |
| Dockerfile | `Dockerfile` |
| Makefile | `.mk`, `Makefile` |

Other files use text-level three-way merge.

## Development

```bash
cargo build
cargo test
cargo test -p phantom-core
cargo clippy -- -D warnings
```

Integration tests live in `tests/integration/` and create temporary git repos with simulated agents.

## Roadmap

- [ ] Full `.phantom/config.toml` schema
- [ ] Agent re-task automation
- [ ] Incremental parsing
- [ ] Aider, Cursor, Codex adapters
- [ ] macOS NFS overlay fallback
- [ ] Benchmarks

## Contributing

1. Fork the repo
2. Install system deps (`libfuse3-dev` on Linux)
3. `cargo test`
4. Open a PR

Design docs:
- [architecture.md](docs/architecture.md)
- [event-model.md](docs/event-model.md)
- [semantic-merge.md](docs/semantic-merge.md)
- [manual-tests.md](docs/manual-tests.md)

## License

MIT

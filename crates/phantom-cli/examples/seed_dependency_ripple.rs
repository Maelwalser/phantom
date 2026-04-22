//! Seed an on-disk Phantom repo that exercises the semantic dependency
//! graph notification path.
//!
//! Scenario:
//!   * Trunk defines `crate::auth::login(u32) -> bool` in `src/auth.rs`.
//!   * Trunk also has `src/handlers.rs` with `handle_request()` which calls
//!     `crate::auth::login(42)`.
//!   * `agent-a` has modified `src/auth.rs` in its overlay — it changed
//!     `login`'s signature to `login(user_id: u32, token: &str) -> bool`.
//!   * `agent-b` is working on `src/handlers.rs` — adding a new
//!     `handle_admin_request()` helper and a small logging wrapper around
//!     the existing `handle_request`, which still calls
//!     `crate::auth::login(42)`. Agent-b does not touch `src/auth.rs`, so
//!     there is no file-level overlap with agent-a's work — the only
//!     thing that links them is the call edge into `login`.
//!
//! After the seeder runs, `agent-b` is an active agent (TaskCreated +
//! FileWritten events applied). When you then run `ph submit agent-a`
//! from inside the generated repo, the materialize-and-ripple pipeline
//! should:
//!   1. Materialize agent-a's change to trunk.
//!   2. Notice that agent-b's upper-layer `handlers.rs` references the
//!      symbol `login` whose signature just changed on trunk — even
//!      though the two agents touched disjoint files.
//!   3. Write a `DependencyImpact` entry with
//!      `ImpactChange::SignatureChanged` into
//!      `.phantom/overlays/agent-b/trunk-updated.json`, and render the
//!      "Impacted Dependencies" section into
//!      `.phantom/overlays/agent-b/upper/.phantom-trunk-update.md`.
//!
//! Usage:
//!   cargo run --example seed_dependency_ripple -- /tmp/phantom-dep-graph-demo
//!
//! Then:
//!   cd /tmp/phantom-dep-graph-demo
//!   ph status
//!   ph submit agent-a
//!   cat .phantom/overlays/agent-b/trunk-updated.json
//!   cat .phantom/overlays/agent-b/upper/.phantom-trunk-update.md

use std::path::{Path, PathBuf};

use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;
use phantom_git::GitOps;

const AUTH_FILE: &str = "src/auth.rs";
const HANDLERS_FILE: &str = "src/handlers.rs";

const TRUNK_AUTH_V1: &str = "\
pub mod auth {
    pub fn login(user_id: u32) -> bool {
        user_id > 0
    }
}
";

const AGENT_A_AUTH_SIG_CHANGED: &str = "\
pub mod auth {
    pub fn login(user_id: u32, token: &str) -> bool {
        user_id > 0 && !token.is_empty()
    }
}
";

/// Trunk version of `src/handlers.rs`: one caller of `auth::login`.
const HANDLERS_TRUNK: &str = "\
pub mod handlers {
    pub fn handle_request() -> bool {
        crate::auth::login(42)
    }
}
";

/// Agent-b's in-progress version of `src/handlers.rs`. Adds a logging
/// wrapper around the original `handle_request` and a new admin-path
/// helper (a second, independent call site into `auth::login`). The
/// original call into `login(42)` is preserved so the ripple pipeline
/// still has a concrete reference to match against the trunk-side
/// signature change.
const AGENT_B_HANDLERS_WIP: &str = "\
pub mod handlers {
    /// New: structured entry point agent-b is building. Wraps the
    /// existing request handler so every incoming call is logged.
    pub fn handle_request_logged() -> bool {
        let outcome = handle_request();
        log_outcome(\"user\", outcome);
        outcome
    }

    /// New: admin variant agent-b is introducing alongside the
    /// regular handler. Still goes through auth::login.
    pub fn handle_admin_request(admin_id: u32) -> bool {
        let ok = crate::auth::login(admin_id);
        log_outcome(\"admin\", ok);
        ok
    }

    /// Unchanged from trunk — still calls auth::login(42).
    pub fn handle_request() -> bool {
        crate::auth::login(42)
    }

    fn log_outcome(channel: &str, ok: bool) {
        eprintln!(\"[{channel}] login -> {ok}\");
    }
}
";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let target = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: seed_dependency_ripple <target-dir>"))?;
    let target = PathBuf::from(target);

    if target.exists() {
        anyhow::bail!(
            "{} already exists — pick a fresh path or remove it first",
            target.display()
        );
    }
    std::fs::create_dir_all(&target)?;

    println!("→ creating git repo at {}", target.display());
    let repo = git2::Repository::init(&target)?;
    seed_initial_commit(&repo)?;

    println!("→ initializing .phantom/ layout");
    let phantom_dir = target.join(".phantom");
    std::fs::create_dir_all(phantom_dir.join("overlays"))?;
    std::fs::write(
        phantom_dir.join("config.toml"),
        format!(
            "phantom_version = \"{}\"\ncreated_at = \"{}\"\ndefault_cli = \"claude\"\n",
            env!("CARGO_PKG_VERSION"),
            Utc::now().to_rfc3339(),
        ),
    )?;
    std::fs::write(target.join(".gitignore"), ".phantom/\n")?;

    let git = GitOps::open(&target)?;
    let events = SqliteEventStore::open(&phantom_dir.join("events.db")).await?;

    println!("→ committing trunk sources ({AUTH_FILE}, {HANDLERS_FILE})");
    commit_file(
        &git,
        AUTH_FILE,
        TRUNK_AUTH_V1,
        "trunk: add auth::login(u32)",
    )?;
    let base = commit_file(
        &git,
        HANDLERS_FILE,
        HANDLERS_TRUNK,
        "trunk: add handlers::handle_request",
    )?;

    // Agent A: modifies src/auth.rs — changes login's signature.
    let agent_a = AgentId("agent-a".into());
    make_agent_file(&phantom_dir, &agent_a, AUTH_FILE, AGENT_A_AUTH_SIG_CHANGED)?;

    // Agent B: actively rewriting src/handlers.rs — adding a logging
    // wrapper and a new admin handler. Its upper-layer content diverges
    // from trunk, but still contains two live references to
    // crate::auth::login, which is exactly what the ripple pipeline will
    // flag once agent-a's signature change lands.
    let agent_b = AgentId("agent-b".into());
    make_agent_file(&phantom_dir, &agent_b, HANDLERS_FILE, AGENT_B_HANDLERS_WIP)?;

    // Emit events so both agents show up as "active" in the projection
    // and have the right files_touched in their changeset state.
    emit_agent_events(
        &events,
        &agent_a,
        "cs-a",
        base,
        AUTH_FILE,
        "change login signature",
    )
    .await?;
    emit_agent_events(
        &events,
        &agent_b,
        "cs-b",
        base,
        HANDLERS_FILE,
        "add logging wrapper + admin handler",
    )
    .await?;

    println!();
    println!("✓ done");
    println!("  cd {}", target.display());
    println!(
        "  ph status                                    # shows agent-a and agent-b as active"
    );
    println!(
        "  ph submit agent-a                            # triggers the ripple → dependency impact"
    );
    println!("  cat .phantom/overlays/agent-b/trunk-updated.json");
    println!("  cat .phantom/overlays/agent-b/upper/.phantom-trunk-update.md");
    Ok(())
}

fn make_agent_file(
    phantom_dir: &Path,
    agent: &AgentId,
    rel_path: &str,
    content: &str,
) -> anyhow::Result<()> {
    let upper = phantom_dir.join("overlays").join(&agent.0).join("upper");
    let full = upper.join(rel_path);
    std::fs::create_dir_all(full.parent().unwrap())?;
    std::fs::write(&full, content)?;
    Ok(())
}

/// Emit the minimal event sequence that puts `agent` in the "active" set
/// with `file` in its `files_touched` list:
///   * `TaskCreated` — flips the changeset to `InProgress` and anchors it
///     at `base`.
///   * `FileWritten` — populates `files_touched` in the projection, which
///     is what the ripple pipeline's dependency-graph path reads to know
///     which files of the agent's upper layer to parse.
async fn emit_agent_events(
    events: &SqliteEventStore,
    agent: &AgentId,
    cs_id: &str,
    base: GitOid,
    file: &str,
    task: &str,
) -> anyhow::Result<()> {
    events
        .append(Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: ChangesetId(cs_id.into()),
            agent_id: agent.clone(),
            causal_parent: None,
            kind: EventKind::TaskCreated {
                base_commit: base,
                task: task.into(),
            },
        })
        .await?;

    events
        .append(Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: ChangesetId(cs_id.into()),
            agent_id: agent.clone(),
            causal_parent: None,
            kind: EventKind::FileWritten {
                path: PathBuf::from(file),
                content_hash: phantom_core::id::ContentHash([0u8; 32]),
            },
        })
        .await?;
    Ok(())
}

fn seed_initial_commit(repo: &git2::Repository) -> anyhow::Result<()> {
    let sig = git2::Signature::now("phantom-demo", "demo@phantom")?;
    let mut index = repo.index()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])?;
    Ok(())
}

fn commit_file(
    git: &GitOps,
    rel_path: &str,
    content: &str,
    message: &str,
) -> anyhow::Result<GitOid> {
    let workdir = git
        .repo()
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("repo has no workdir"))?
        .to_path_buf();
    let full = workdir.join(rel_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&full, content)?;

    let mut index = git.repo().index()?;
    index.add_path(Path::new(rel_path))?;
    index.write()?;
    let tree_oid = index.write_tree()?;
    let tree = git.repo().find_tree(tree_oid)?;
    let sig = git2::Signature::now("phantom-demo", "demo@phantom")?;
    let head = git.repo().head()?.peel_to_commit()?;
    let new_oid = git
        .repo()
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head])?;
    Ok(phantom_git::oid_to_git_oid(new_oid))
}

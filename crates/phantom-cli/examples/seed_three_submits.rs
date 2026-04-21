//! Seed an on-disk Phantom repo with three real semantic submits:
//! one clean materialization and two genuine semantic conflicts on the
//! same Rust function. After this runs, point `ph conflicts` at the
//! generated repo to exercise the conflict inspector against real events.
//!
//! Usage:
//!   cargo run --example seed_three_submits -- /tmp/phantom-conflicts-demo
//!
//! Then:
//!   cd /tmp/phantom-conflicts-demo
//!   ph conflicts
//!   ph conflicts agent-c

use std::path::{Path, PathBuf};

use chrono::Utc;
use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;
use phantom_git::GitOps;
use phantom_orchestrator::materializer::{MaterializeResult, Materializer};
use phantom_semantic::SemanticMerger;

const FILE: &str = "src/lib.rs";

const TRUNK_SOURCE: &str = "\
fn compute() -> i32 {
    42
}

fn helper() -> i32 {
    7
}
";

// All three agents rewrite `compute()` differently. Agent-a wins the race
// to trunk; agent-b and agent-c then collide with trunk's new compute() body
// — a real semantic conflict on the same symbol, not a text-level overlap on
// disjoint regions.
const AGENT_A_SOURCE: &str = "\
fn compute() -> i32 {
    let base = 10;
    base * 5
}

fn helper() -> i32 {
    7
}
";

const AGENT_B_SOURCE: &str = "\
fn compute() -> i32 {
    let mut total = 0;
    for i in 1..=10 {
        total += i;
    }
    total
}

fn helper() -> i32 {
    7
}
";

const AGENT_C_SOURCE: &str = "\
fn compute() -> i32 {
    (1..=20).sum::<i32>() + 99
}

fn helper() -> i32 {
    7
}
";

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let target = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: seed_three_submits <target-dir>"))?;
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
    let gitignore = target.join(".gitignore");
    std::fs::write(&gitignore, ".phantom/\n")?;

    let git = GitOps::open(&target)?;
    let events = SqliteEventStore::open(&phantom_dir.join("events.db")).await?;
    let merger = SemanticMerger::new();

    println!("→ committing trunk source ({FILE})");
    let base = commit_file(&git, FILE, TRUNK_SOURCE, "trunk: initial compute() and helper()")?;

    let agent_a = make_agent(&phantom_dir, "agent-a", FILE, AGENT_A_SOURCE)?;
    let agent_b = make_agent(&phantom_dir, "agent-b", FILE, AGENT_B_SOURCE)?;
    let agent_c = make_agent(&phantom_dir, "agent-c", FILE, AGENT_C_SOURCE)?;

    let cs_a = build_changeset("cs-a", &agent_a, base, "rewrite compute() with base*5");
    let cs_b = build_changeset("cs-b", &agent_b, base, "rewrite compute() with for-loop sum");
    let cs_c = build_changeset("cs-c", &agent_c, base, "rewrite compute() with iterator sum");

    let upper_a = phantom_dir.join("overlays").join(&agent_a.0).join("upper");
    let upper_b = phantom_dir.join("overlays").join(&agent_b.0).join("upper");
    let upper_c = phantom_dir.join("overlays").join(&agent_c.0).join("upper");

    submit(
        &git,
        &events,
        &merger,
        &cs_a,
        &upper_a,
        "agent-a",
        ExpectedOutcome::Success,
    )
    .await?;
    submit(
        &git,
        &events,
        &merger,
        &cs_b,
        &upper_b,
        "agent-b",
        ExpectedOutcome::Conflict,
    )
    .await?;
    submit(
        &git,
        &events,
        &merger,
        &cs_c,
        &upper_c,
        "agent-c",
        ExpectedOutcome::Conflict,
    )
    .await?;

    println!();
    println!("✓ done");
    println!("  cd {}", target.display());
    println!("  ph conflicts          # menu with agent-b and agent-c");
    println!("  ph conflicts agent-c  # detail view directly");
    Ok(())
}

#[derive(Clone, Copy)]
enum ExpectedOutcome {
    Success,
    Conflict,
}

async fn submit(
    git: &GitOps,
    events: &SqliteEventStore,
    merger: &SemanticMerger,
    cs: &Changeset,
    upper_dir: &Path,
    label: &str,
    expect: ExpectedOutcome,
) -> anyhow::Result<()> {
    println!("→ submitting {label} ({})", cs.id);

    let task_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: cs.id.clone(),
        agent_id: cs.agent_id.clone(),
        causal_parent: None,
        kind: EventKind::TaskCreated {
            base_commit: cs.base_commit,
            task: cs.task.clone(),
        },
    };
    events.append(task_event).await?;

    let submit_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: cs.id.clone(),
        agent_id: cs.agent_id.clone(),
        causal_parent: None,
        kind: EventKind::ChangesetSubmitted { operations: vec![] },
    };
    events.append(submit_event).await?;

    let mat = Materializer::new(git);
    let result = mat
        .materialize(cs, upper_dir, events, merger, &cs.task, None)
        .await?;

    match (&result, expect) {
        (MaterializeResult::Success { .. }, ExpectedOutcome::Success) => {
            println!("    ✓ materialized cleanly");
        }
        (MaterializeResult::Conflict { details }, ExpectedOutcome::Conflict) => {
            println!("    ✗ conflicted ({} detail(s))", details.len());
            for d in details {
                println!(
                    "      - {} [{:?}] {}",
                    d.file.display(),
                    d.kind,
                    d.description
                );
            }
        }
        (MaterializeResult::Success { .. }, ExpectedOutcome::Conflict) => {
            anyhow::bail!("expected {label} to conflict but it materialized cleanly");
        }
        (MaterializeResult::Conflict { details }, ExpectedOutcome::Success) => {
            anyhow::bail!("expected {label} to materialize but got conflicts: {details:?}");
        }
    }

    Ok(())
}

fn make_agent(
    phantom_dir: &Path,
    name: &str,
    file: &str,
    content: &str,
) -> anyhow::Result<AgentId> {
    let upper = phantom_dir.join("overlays").join(name).join("upper");
    let full = upper.join(file);
    std::fs::create_dir_all(full.parent().unwrap())?;
    std::fs::write(&full, content)?;
    Ok(AgentId(name.into()))
}

fn build_changeset(id: &str, agent: &AgentId, base: GitOid, task: &str) -> Changeset {
    Changeset {
        id: ChangesetId(id.into()),
        agent_id: agent.clone(),
        task: task.into(),
        base_commit: base,
        files_touched: vec![PathBuf::from(FILE)],
        operations: vec![],
        test_result: None,
        created_at: Utc::now(),
        status: ChangesetStatus::Submitted,
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
    }
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

//! E2E CLI tests for merge conflict detection and resolution.
//!
//! Test 1 (`conflict_and_resolve_workflow`):
//! Two agents modify the same symbol → submit (which includes materialization)
//! conflicts on the second agent → `phantom resolve` recognizes the conflict.
//!
//! Test 2 (`resolve_updates_base_and_materialize_succeeds`):
//! After `phantom resolve` updates the base commit, re-submitting the resolved
//! changeset succeeds instead of re-conflicting.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Create a temporary directory with an initialized git repo and an initial commit
/// that includes a Rust source file with a function both agents will modify.
fn init_repo_with_source() -> TempDir {
    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();

    // Create a Rust file with a function that both agents will modify.
    let src_dir = dir.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("lib.rs"),
        "pub fn compute() -> i32 {\n    42\n}\n",
    )
    .unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new("src/lib.rs")).unwrap();
    index.write().unwrap();

    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("test", "test@phantom").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
        .unwrap();

    dir
}

/// Build a `Command` for the `phantom` binary with working dir set.
fn phantom(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("ph").unwrap();
    cmd.current_dir(dir).env("RUST_LOG", "");
    // Integration tests stub the CLI with `echo`; the allowlist in
    // `validate_cli_name` blocks non-known commands unless this test-only
    // flag is set.
    cmd.env("PHANTOM_ALLOW_ANY_CLI", "1");
    cmd
}

#[test]
fn conflict_and_resolve_workflow() {
    let dir = init_repo_with_source();

    // 1. Initialize phantom
    phantom(dir.path()).arg("init").assert().success();

    // 2. Create two agents using --command echo (exits immediately, no changes)
    phantom(dir.path())
        .args(["agent-a", "--no-fuse", "--command", "echo"])
        .assert()
        .success();

    phantom(dir.path())
        .args(["agent-b", "--no-fuse", "--command", "echo"])
        .assert()
        .success();

    // 3. Write conflicting changes to both agents' upper dirs.
    //    Both agents modify the same function `compute()` differently.
    let upper_a = dir.path().join(".phantom/overlays/agent-a/upper/src");
    let upper_b = dir.path().join(".phantom/overlays/agent-b/upper/src");

    fs::create_dir_all(&upper_a).unwrap();
    fs::create_dir_all(&upper_b).unwrap();

    fs::write(
        upper_a.join("lib.rs"),
        "pub fn compute() -> i32 {\n    100\n}\n",
    )
    .unwrap();

    fs::write(
        upper_b.join("lib.rs"),
        "pub fn compute() -> i32 {\n    200\n}\n",
    )
    .unwrap();

    // 4. Submit agent-a → should succeed (first to merge, no conflict)
    phantom(dir.path())
        .args(["submit", "agent-a"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("submitted").and(predicate::str::contains("Materialized")),
        );

    // 5. Submit agent-b → should fail with conflict (same symbol modified)
    phantom(dir.path())
        .args(["submit", "agent-b"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("conflict").or(predicate::str::contains("Conflict")));

    // 6. Verify `phantom resolve agent-b` finds the conflicted changeset.
    //    The resolve command prints conflict info before spawning the background
    //    agent. The spawn may fail in tests (no `claude` binary), but we verify
    //    the conflict was detected and the output references the right agent.
    let resolve_output = phantom(dir.path())
        .args(["resolve", "agent-b"])
        .output()
        .expect("failed to run phantom resolve");

    let stdout = String::from_utf8_lossy(&resolve_output.stdout);
    let stderr = String::from_utf8_lossy(&resolve_output.stderr);
    let combined = format!("{stdout}{stderr}");

    // The resolve command should find the conflict for agent-b.
    assert!(
        combined.contains("Resolving")
            || combined.contains("conflict")
            || combined.contains("agent-b"),
        "resolve should reference the conflict for agent-b.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // 7. Verify that resolve for an agent with no conflicts fails cleanly.
    phantom(dir.path())
        .args(["resolve", "agent-a"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no conflicted changeset"));

    // 8. Verify status still works after the conflict scenario.
    phantom(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Total events:"));

    // 9. Verify the log captured all the events including the conflict.
    phantom(dir.path()).arg("log").assert().success().stdout(
        predicate::str::contains("submitted").and(
            predicate::str::contains("materialized").or(predicate::str::contains("conflicted")),
        ),
    );
}

/// After `phantom resolve` updates the changeset's base_commit to current
/// trunk HEAD, re-submitting the (now resolved) overlay should succeed — the
/// materializer sees base == trunk and applies cleanly.
#[test]
fn resolve_updates_base_and_materialize_succeeds() {
    let dir = init_repo_with_source();

    // 1. Initialize phantom
    phantom(dir.path()).arg("init").assert().success();

    // 2. Create two agents
    phantom(dir.path())
        .args(["agent-a", "--no-fuse", "--command", "echo"])
        .assert()
        .success();
    phantom(dir.path())
        .args(["agent-b", "--no-fuse", "--command", "echo"])
        .assert()
        .success();

    // 3. Write conflicting changes (same function, different bodies)
    let upper_a = dir.path().join(".phantom/overlays/agent-a/upper/src");
    let upper_b = dir.path().join(".phantom/overlays/agent-b/upper/src");
    fs::create_dir_all(&upper_a).unwrap();
    fs::create_dir_all(&upper_b).unwrap();

    fs::write(
        upper_a.join("lib.rs"),
        "pub fn compute() -> i32 {\n    100\n}\n",
    )
    .unwrap();
    fs::write(
        upper_b.join("lib.rs"),
        "pub fn compute() -> i32 {\n    200\n}\n",
    )
    .unwrap();

    // 4. Submit agent-a (succeeds — includes materialization)
    phantom(dir.path())
        .args(["submit", "agent-a"])
        .assert()
        .success();

    // 5. Submit agent-b (conflicts — same symbol modified on trunk)
    phantom(dir.path())
        .args(["submit", "agent-b"])
        .assert()
        .failure();

    // 6. Run `phantom resolve agent-b` — this updates the changeset's
    //    base_commit to current trunk HEAD via ConflictResolutionStarted event.
    //    The resolve command may fail at the spawn step (no claude binary),
    //    but the event is emitted before that.
    let _ = phantom(dir.path())
        .args(["resolve", "agent-b"])
        .output()
        .expect("failed to run phantom resolve");

    // 7. Simulate the resolve agent's work: write a merged version that
    //    incorporates agent-a's changes (already on trunk) with agent-b's
    //    intent. In this case, the resolved version is what agent-b wanted
    //    but acknowledges agent-a's work is already there.
    fs::write(
        upper_b.join("lib.rs"),
        "pub fn compute() -> i32 {\n    // merged: agent-a set 100, agent-b wanted 200\n    300\n}\n",
    )
    .unwrap();

    // 8. Re-submit agent-b — should succeed because base_commit was updated
    //    by the ConflictResolutionStarted event to current trunk HEAD.
    phantom(dir.path())
        .args(["submit", "agent-b"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("submitted").and(predicate::str::contains("Materialized")),
        );

    // 9. Verify the merged content landed on trunk.
    let trunk_content = fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    assert!(
        trunk_content.contains("300"),
        "trunk should contain the resolved version, got: {trunk_content}"
    );
}

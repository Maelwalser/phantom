//! Integration tests for the `phantom` CLI binary.
//!
//! Each test creates a temporary git repository and runs CLI commands via
//! `assert_cmd`, verifying output and filesystem side effects.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Create a temporary directory with an initialized git repo (with an initial commit).
fn init_git_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();

    let mut index = repo.index().unwrap();
    let readme_path = dir.path().join("README.md");
    fs::write(&readme_path, "# Test repo\n").unwrap();
    index.add_path(Path::new("README.md")).unwrap();
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
    let mut cmd = Command::cargo_bin("phantom").unwrap();
    cmd.current_dir(dir).env("RUST_LOG", "");
    cmd
}

#[test]
fn phantom_help_lists_all_subcommands() {
    let dir = TempDir::new().unwrap();
    phantom(dir.path())
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("up"))
        .stdout(predicate::str::contains("dispatch"))
        .stdout(predicate::str::contains("submit"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("materialize"))
        .stdout(predicate::str::contains("rollback"))
        .stdout(predicate::str::contains("log"))
        .stdout(predicate::str::contains("destroy"));
}

#[test]
fn phantom_up_creates_directory_structure() {
    let dir = init_git_repo();

    phantom(dir.path())
        .arg("up")
        .assert()
        .success()
        .stdout(predicate::str::contains("Phantom initialized"));

    assert!(dir.path().join(".phantom").is_dir());
    assert!(dir.path().join(".phantom/overlays").is_dir());
    assert!(dir.path().join(".phantom/events.db").is_file());
    assert!(dir.path().join(".phantom/config.toml").is_file());

    let gitignore = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains(".phantom/"));
}

#[test]
fn phantom_up_fails_outside_git_repo() {
    let dir = TempDir::new().unwrap();

    phantom(dir.path())
        .arg("up")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a git repository"));
}

#[test]
fn phantom_up_fails_if_already_initialized() {
    let dir = init_git_repo();

    phantom(dir.path()).arg("up").assert().success();

    phantom(dir.path())
        .arg("up")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already initialized"));
}

#[test]
fn phantom_dispatch_background_and_status() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("up").assert().success();

    phantom(dir.path())
        .args([
            "dispatch",
            "agent-a",
            "--background",
            "--task",
            "add rate limiting",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Agent 'agent-a' dispatched"))
        .stdout(predicate::str::contains("cs-0001"))
        .stdout(predicate::str::contains("add rate limiting"));

    phantom(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Trunk HEAD:"))
        .stdout(predicate::str::contains("Total events:"));
}

#[test]
fn phantom_dispatch_interactive_with_echo() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("up").assert().success();

    // Use `echo` as a stand-in for claude — it exits immediately
    phantom(dir.path())
        .args(["dispatch", "agent-b", "--command", "echo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Agent 'agent-b' dispatched"))
        .stdout(predicate::str::contains("Interactive session ended"))
        .stdout(predicate::str::contains("No changes detected"));
}

#[test]
fn phantom_log_empty() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("up").assert().success();

    phantom(dir.path()).arg("log").assert().success().stdout(
        predicate::str::contains("event(s) shown").or(predicate::str::contains("No events")),
    );
}

#[test]
fn full_workflow_smoke_test() {
    let dir = init_git_repo();

    // 1. Initialize
    phantom(dir.path()).arg("up").assert().success();

    // 2. Dispatch in background mode
    phantom(dir.path())
        .args([
            "dispatch",
            "agent-a",
            "--background",
            "--task",
            "add feature X",
        ])
        .assert()
        .success();

    // 3. Simulate agent work by writing a file to the upper dir
    let upper_dir = dir.path().join(".phantom/overlays/agent-a/upper");
    assert!(upper_dir.is_dir(), "upper dir should exist after dispatch");

    let src_dir = upper_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("feature.rs"),
        "pub fn feature_x() { todo!() }\n",
    )
    .unwrap();

    // 4. Submit
    phantom(dir.path())
        .args(["submit", "agent-a"])
        .assert()
        .success()
        .stdout(predicate::str::contains("submitted"));

    // 5. Log should show events
    phantom(dir.path())
        .arg("log")
        .assert()
        .success()
        .stdout(predicate::str::contains("OverlayCreated"))
        .stdout(predicate::str::contains("ChangesetSubmitted"));

    // 6. Status should show the changeset
    phantom(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Total events:"));
}

#[test]
fn phantom_dispatch_background_conflicts_with_auto_submit() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("up").assert().success();

    phantom(dir.path())
        .args([
            "dispatch",
            "agent-a",
            "--background",
            "--task",
            "test",
            "--auto-submit",
        ])
        .assert()
        .failure();
}

#[test]
fn phantom_dispatch_background_requires_task() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("up").assert().success();

    // --background without --task should fail
    phantom(dir.path())
        .args(["dispatch", "agent-a", "--background"])
        .assert()
        .failure();
}

#[test]
fn phantom_dispatch_interactive_auto_submit_with_no_changes() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("up").assert().success();

    // echo exits immediately with no changes — auto-submit should report "no changes"
    phantom(dir.path())
        .args(["dispatch", "agent-c", "--command", "echo", "--auto-submit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No changes detected"));
}

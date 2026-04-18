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

/// Build a `Command` for the `ph` binary with working dir set.
fn phantom(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("ph").unwrap();
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
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("submit"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("rollback"))
        .stdout(predicate::str::contains("log"))
        .stdout(predicate::str::contains("remove"));
}

#[test]
fn phantom_init_creates_directory_structure() {
    let dir = init_git_repo();

    phantom(dir.path())
        .arg("init")
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
fn phantom_init_fails_outside_git_repo() {
    let dir = TempDir::new().unwrap();

    phantom(dir.path())
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a git repository"));
}

#[test]
fn phantom_init_fails_if_already_initialized() {
    let dir = init_git_repo();

    phantom(dir.path()).arg("init").assert().success();

    phantom(dir.path())
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already initialized"));
}

#[test]
fn phantom_task_background_and_status() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    phantom(dir.path())
        .args([
            "agent-a",
            "--no-fuse",
            "--background",
            "--task",
            "add rate limiting",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Agent 'agent-a' tasked"))
        .stdout(predicate::str::contains("cs-"))
        .stdout(predicate::str::contains("add rate limiting"));

    phantom(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Trunk HEAD:"))
        .stdout(predicate::str::contains("Total events:"));
}

#[test]
fn phantom_task_interactive_with_echo() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    // Use `echo` as a stand-in for claude — it exits immediately
    phantom(dir.path())
        .args(["agent-b", "--no-fuse", "--command", "echo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Agent 'agent-b' tasked"))
        .stdout(predicate::str::contains("Interactive session ended"))
        .stdout(predicate::str::contains("No changes detected"));
}

#[test]
fn phantom_log_empty() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    phantom(dir.path()).arg("log").assert().success().stdout(
        predicate::str::contains("event(s) shown").or(predicate::str::contains("No events")),
    );
}

#[test]
fn full_workflow_smoke_test() {
    let dir = init_git_repo();

    // 1. Initialize
    phantom(dir.path()).arg("init").assert().success();

    // 2. Dispatch in background mode
    phantom(dir.path())
        .args([
            "agent-a",
            "--no-fuse",
            "--background",
            "--task",
            "add feature X",
        ])
        .assert()
        .success();

    // 3. Simulate agent work by writing a file to the upper dir
    let upper_dir = dir.path().join(".phantom/overlays/agent-a/upper");
    assert!(upper_dir.is_dir(), "upper dir should exist after task");

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
        .stdout(predicate::str::contains("task created"))
        .stdout(predicate::str::contains("submitted"));

    // 6. Status should show the changeset
    phantom(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Total events:"));
}

#[test]
fn phantom_task_background_conflicts_with_auto_submit() {
    // --auto-submit is now allowed with --background (background agents always
    // auto-submit, so the flag is accepted but redundant).
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    phantom(dir.path())
        .args([
            "agent-a",
            "--no-fuse",
            "--background",
            "--task",
            "test",
            "--auto-submit",
        ])
        .assert()
        .success();
}

#[test]
fn phantom_task_background_requires_task() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    // --background without --task should fail
    phantom(dir.path())
        .args(["agent-a", "--background"])
        .assert()
        .failure();
}

#[test]
fn phantom_task_interactive_auto_submit_with_no_changes() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    // echo exits immediately with no changes — auto-submit should report "no changes"
    phantom(dir.path())
        .args(["agent-c", "--no-fuse", "--command", "echo", "--auto-submit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No changes detected"));
}

#[test]
fn phantom_task_category_builtin_writes_rules_file_and_shows_label() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    phantom(dir.path())
        .args([
            "agent-cat-builtin",
            "--no-fuse",
            "--background",
            "--task",
            "fix bug",
            "--category",
            "corrective",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Category"))
        .stdout(predicate::str::contains("corrective"));

    for name in ["corrective", "perfective", "preventive", "adaptive"] {
        assert!(
            dir.path()
                .join(format!(".phantom/rules/{name}.md"))
                .is_file(),
            "missing rules file for {name}"
        );
    }
}

#[test]
fn phantom_task_cat_alias_resolves_same_as_category() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    phantom(dir.path())
        .args([
            "agent-cat-alias",
            "--no-fuse",
            "--background",
            "--task",
            "refactor",
            "--cat",
            "perfective",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("perfective"));
}

#[test]
fn phantom_task_category_with_external_md_path_succeeds() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    let rules_path = dir.path().join("my-rules.md");
    fs::write(&rules_path, "# My Rules\n\nBe careful.\n").unwrap();

    phantom(dir.path())
        .args([
            "agent-cat-file",
            "--no-fuse",
            "--background",
            "--task",
            "anything",
            "--category",
            rules_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("file: my-rules.md"));
}

#[test]
fn phantom_task_category_missing_path_fails() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    phantom(dir.path())
        .args([
            "agent-cat-miss",
            "--background",
            "--task",
            "x",
            "--category",
            "./does-not-exist.md",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does-not-exist.md"));
}

#[test]
fn phantom_task_category_non_md_extension_fails() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    let txt = dir.path().join("rules.txt");
    fs::write(&txt, "x").unwrap();

    phantom(dir.path())
        .args([
            "agent-cat-ext",
            "--background",
            "--task",
            "x",
            "--category",
            txt.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(".md"));
}

#[test]
fn phantom_task_category_and_custom_conflict() {
    let dir = init_git_repo();
    phantom(dir.path()).arg("init").assert().success();

    // clap's `conflicts_with` fires before we even reach the resolver.
    phantom(dir.path())
        .args([
            "agent-conflict",
            "--background",
            "--task",
            "x",
            "--category",
            "corrective",
            "--custom",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

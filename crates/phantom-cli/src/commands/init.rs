//! `phantom init` — initialize Phantom in an existing git repository.

use std::fs;
use std::path::Path;

use anyhow::{Context, bail};
use phantom_events::SqliteEventStore;

/// Initialize Phantom in the current git repository.
pub async fn run() -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("failed to determine current directory")?;

    // Use git2::Repository::discover to handle worktrees (.git file) and bare repos.
    let Ok(repo) = git2::Repository::discover(&cwd) else {
        bail!("not a git repository: no .git/ found in {}", cwd.display());
    };

    // Phantom's overlay + materialization pipeline requires at least one commit
    // to anchor `current_base` against. If HEAD is unborn, create an empty
    // initial commit so `ph <agent>` and `ph plan` work immediately.
    ensure_initial_commit(&repo)?;

    let phantom_dir = cwd.join(".phantom");
    if phantom_dir.exists() {
        bail!(
            "Phantom is already initialized in {}. Remove .phantom/ to reinitialize.",
            cwd.display()
        );
    }

    fs::create_dir_all(phantom_dir.join("overlays"))
        .context("failed to create .phantom/overlays/")?;

    SqliteEventStore::open(&phantom_dir.join("events.db"))
        .await
        .context("failed to create event store")?;

    let config = format!(
        "phantom_version = \"{}\"\ncreated_at = \"{}\"\ndefault_cli = \"claude\"\n",
        env!("CARGO_PKG_VERSION"),
        chrono::Utc::now().to_rfc3339(),
    );
    fs::write(phantom_dir.join("config.toml"), config).context("failed to write config.toml")?;

    ensure_gitignore(&cwd)?;

    println!(
        "  {} Phantom initialized in {}",
        console::style("✓").green(),
        cwd.display()
    );
    crate::ui::action_hint("ph <agent>", "to create your first task.");
    Ok(())
}

/// Create an empty initial commit if `HEAD` is unborn.
///
/// Phantom's overlay model stores each agent's base commit as `current_base`,
/// and materialization reads that commit to compute the tree for the new
/// commit. A null OID (unborn HEAD) triggers a libgit2 `odb: cannot read
/// object: null OID` error deep in the pipeline. We pre-empt that by seeding
/// the repository with an empty commit whose tree captures whatever the user
/// had already staged (usually nothing).
fn ensure_initial_commit(repo: &git2::Repository) -> anyhow::Result<()> {
    match repo.head() {
        Ok(_) => return Ok(()),
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => {}
        Err(e) => return Err(anyhow::Error::new(e).context("failed to read HEAD")),
    }

    let sig = repo.signature().or_else(|_| {
        git2::Signature::now("phantom", "phantom@phantom")
    }).context("failed to build commit signature (configure user.name / user.email or rely on the phantom fallback)")?;

    let mut index = repo.index().context("failed to open git index")?;
    let tree_id = index.write_tree().context("failed to write empty tree")?;
    let tree = repo
        .find_tree(tree_id)
        .context("failed to find empty tree")?;

    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "phantom: initial commit",
        &tree,
        &[],
    )
    .context("failed to create initial commit")?;

    println!(
        "  {} created initial commit (repository had no HEAD)",
        console::style("✓").green()
    );

    Ok(())
}

/// Add `.phantom/` to `.gitignore` if it's not already there.
fn ensure_gitignore(repo_root: &Path) -> anyhow::Result<()> {
    let gitignore = repo_root.join(".gitignore");
    let entry = ".phantom/";

    if gitignore.exists() {
        let content = fs::read_to_string(&gitignore).context("failed to read .gitignore")?;
        if content.lines().any(|line| line.trim() == entry) {
            return Ok(());
        }
        let separator = if content.ends_with('\n') { "" } else { "\n" };
        fs::write(&gitignore, format!("{content}{separator}{entry}\n"))
            .context("failed to update .gitignore")?;
    } else {
        fs::write(&gitignore, format!("{entry}\n")).context("failed to create .gitignore")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ensure_initial_commit_seeds_unborn_head() {
        let dir = TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        assert!(matches!(
            repo.head().map_err(|e| e.code()),
            Err(git2::ErrorCode::UnbornBranch)
        ));

        ensure_initial_commit(&repo).unwrap();

        let head = repo.head().unwrap();
        let commit = head.peel_to_commit().unwrap();
        assert_eq!(commit.parent_count(), 0);
        assert_eq!(commit.message().unwrap(), "phantom: initial commit");
    }

    #[test]
    fn ensure_initial_commit_is_noop_when_head_exists() {
        let dir = TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("test", "test@test").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let first_oid = repo
            .commit(Some("HEAD"), &sig, &sig, "real initial", &tree, &[])
            .unwrap();
        drop(tree);

        ensure_initial_commit(&repo).unwrap();

        let head_oid = repo.head().unwrap().target().unwrap();
        assert_eq!(head_oid, first_oid);
    }
}

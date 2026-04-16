//! `phantom init` — initialize Phantom in an existing git repository.

use std::fs;
use std::path::Path;

use anyhow::{Context, bail};
use phantom_events::SqliteEventStore;

/// Initialize Phantom in the current git repository.
pub async fn run() -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("failed to determine current directory")?;

    // Use git2::Repository::discover to handle worktrees (.git file) and bare repos.
    if git2::Repository::discover(&cwd).is_err() {
        bail!("not a git repository: no .git/ found in {}", cwd.display());
    }

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
    super::ui::action_hint("phantom <agent>", "to create your first task.");
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

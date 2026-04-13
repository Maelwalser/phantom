//! `.phantom-task.md` generation and cleanup for agent overlays.
//!
//! The context file provides agents with metadata about their session:
//! agent ID, changeset ID, base commit, and available commands.

use std::path::Path;

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use tracing::warn;

/// Name of the generated context file placed in the overlay.
pub const CONTEXT_FILE: &str = ".phantom-task.md";

/// Write a context file into the overlay with agent metadata and optional task.
pub fn write_context_file(
    upper_dir: &Path,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    task: Option<&str>,
) -> anyhow::Result<()> {
    let base_hex = base_commit.to_hex();
    let base_short = &base_hex[..12.min(base_hex.len())];

    let task_section = match task {
        Some(t) if !t.is_empty() => format!("\n## Task\n{t}\n"),
        _ => String::new(),
    };

    let content = format!(
        r#"# Phantom Agent Session

You are working inside a Phantom overlay. Your changes are isolated from
trunk and other agents.
{task_section}
## Agent Info
- Agent: {agent_id}
- Changeset: {changeset_id}
- Base commit: {base_short}

## Commands
- `phantom submit {agent_id}` -- submit your changes
- `phantom materialize {changeset_id}` -- merge to trunk
- `phantom status` -- view all agents and changesets
"#
    );

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write context file to {}", path.display()))?;

    Ok(())
}

/// Remove the generated context file from the overlay.
pub fn cleanup_context_file(upper_dir: &Path) {
    let path = upper_dir.join(CONTEXT_FILE);
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!(path = %path.display(), error = %e, "failed to clean up context file");
    }
}

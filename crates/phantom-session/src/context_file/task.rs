//! Basic `.phantom-task.md` generation for agent overlays.

use std::path::Path;

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId, GitOid};

use super::CONTEXT_FILE;

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

## Commands
- `phantom submit {agent_id}` -- submit your changes and merge to trunk
- `phantom status` -- view all agents and changesets

## Agent Info
- Agent: {agent_id}
- Changeset: {changeset_id}
- Base commit: {base_short}
{task_section}"#
    );

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write context file to {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
#[path = "task_tests.rs"]
mod tests;

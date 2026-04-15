//! `.phantom-task.md` generation and incremental updates for agent overlays.
//!
//! The context file is structured for prompt cache efficiency: the static
//! preamble (commands, rules, agent info) occupies the top of the file and
//! never changes after creation. Dynamic updates (trunk changes, rebase
//! results) are appended at the bottom under a `## Trunk Updates` section.
//! This layout maximises prompt cache hit rates for LLM agents that re-read
//! the file periodically — the leading bytes remain byte-identical across
//! reads.

use std::path::Path;

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId, GitOid};

use super::CONTEXT_FILE;

/// Separator that marks the boundary between the static preamble and dynamic
/// updates. Everything above this line is written once at task creation;
/// everything below is append-only.
const UPDATES_SECTION: &str = "\n---\n\n## Trunk Updates\n";

/// Write a context file into the overlay with agent metadata and optional task.
///
/// The file is structured with static content first (commands, agent info,
/// task description) followed by a placeholder for dynamic updates. This
/// ordering maximises prompt cache hits when the agent re-reads the file,
/// since the static prefix remains byte-identical.
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
{task_section}{UPDATES_SECTION}"#
    );

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write context file to {}", path.display()))?;

    Ok(())
}

/// Append an incremental update to the context file's dynamic section.
///
/// Updates are appended below the `## Trunk Updates` section at the bottom
/// of the file, preserving the static preamble byte-for-byte. Each update
/// is separated by a horizontal rule for readability.
///
/// If the context file does not exist (agent was created before this feature),
/// this is a no-op — the separate `.phantom-trunk-update.md` file still
/// serves as the fallback notification mechanism.
pub fn append_context_update(upper_dir: &Path, update: &str) -> anyhow::Result<()> {
    let path = upper_dir.join(CONTEXT_FILE);
    if !path.exists() {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open context file for append: {}", path.display()))?;

    use std::io::Write;
    write!(file, "\n---\n\n{update}")
        .with_context(|| format!("failed to append to context file: {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
#[path = "task_tests.rs"]
mod tests;

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
use phantom_toolchain::{Toolchain, VerificationVerb};

use super::CONTEXT_FILE;

/// Separator that marks the boundary between the static preamble and dynamic
/// updates. Everything above this line is written once at task creation;
/// everything below is append-only.
///
/// Trunk updates primarily arrive as injected context on the next turn
/// (via Claude's `UserPromptSubmit` / `PostToolUse` hooks wired by
/// `phantom-session::hook_config`). The file mirror below is kept for
/// sessions running a CLI without hook support and as a scrollable audit
/// trail between turns.
const UPDATES_SECTION: &str = "\n---\n\n## Trunk Updates\n\n*Upstream changes arrive inline at the start of your next turn. \
     The entries below are a mirror for reference.*\n";

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
    write_context_file_with_toolchain(upper_dir, agent_id, changeset_id, base_commit, task, None)
}

/// Like [`write_context_file`] but also injects a "Verification (this repo)"
/// block populated from the detected [`Toolchain`]. Written between the Agent
/// Info and Task sections so the dynamic prefix (everything above `## Trunk
/// Updates`) remains stable per repo for prompt-cache reuse.
pub fn write_context_file_with_toolchain(
    upper_dir: &Path,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    task: Option<&str>,
    toolchain: Option<&Toolchain>,
) -> anyhow::Result<()> {
    let base_hex = base_commit.to_hex();
    let base_short = &base_hex[..12.min(base_hex.len())];

    let verification_section = toolchain
        .filter(|t| !t.is_empty())
        .map(render_verification_block)
        .unwrap_or_default();

    let task_section = match task {
        Some(t) if !t.is_empty() => format!("\n## Task\n{t}\n"),
        _ => String::new(),
    };

    let content = format!(
        r#"# Phantom Agent Session

You are working inside a Phantom overlay. Your changes are isolated from
trunk and other agents.

## Commands
- `ph submit {agent_id}` -- submit your changes and merge to trunk
- `ph status` -- view all agents and changesets

## Agent Info
- Agent: {agent_id}
- Changeset: {changeset_id}
- Base commit: {base_short}
{verification_section}{task_section}{UPDATES_SECTION}"#
    );

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write context file to {}", path.display()))?;

    Ok(())
}

/// Render the "Verification (this repo)" block. Commands are listed in the
/// canonical verb order defined by [`VerificationVerb::ALL`]. Missing verbs
/// are skipped.
pub(crate) fn render_verification_block(toolchain: &Toolchain) -> String {
    use std::fmt::Write as _;

    let mut block = String::from(
        "\n## Verification (this repo)\n\
         These commands cover the project's detected toolchain. Phantom runs \
         the pre-submit checks — fabricating success will not pass semantic \
         merge.\n",
    );
    for verb in VerificationVerb::ALL {
        if let Some(cmd) = toolchain.command_for(verb) {
            let _ = writeln!(block, "- {label}: `{cmd}`", label = verb.human_label());
        }
    }
    block
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
mod tests {
    use super::*;

    #[test]
    fn context_file_has_dynamic_sections_last() {
        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("a1".to_string());
        let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        write_context_file(
            dir.path(),
            &agent_id,
            &changeset_id,
            &base_commit,
            Some("do stuff"),
        )
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
        let commands_pos = content.find("## Commands").unwrap();
        let info_pos = content.find("## Agent Info").unwrap();
        let task_pos = content.find("## Task").unwrap();
        let updates_pos = content.find("## Trunk Updates").unwrap();
        // Order: most-static to most-dynamic for prompt cache efficiency.
        assert!(
            commands_pos < info_pos,
            "Commands should come before Agent Info"
        );
        assert!(info_pos < task_pos, "Agent Info should come before Task");
        assert!(
            task_pos < updates_pos,
            "Task should come before Trunk Updates"
        );
    }

    #[test]
    fn context_file_ends_with_updates_section() {
        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("a1".to_string());
        let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        write_context_file(dir.path(), &agent_id, &changeset_id, &base_commit, None).unwrap();

        let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
        assert!(
            content.contains("## Trunk Updates"),
            "Context file should contain Trunk Updates section"
        );
    }

    #[test]
    fn append_context_update_adds_to_bottom() {
        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("a1".to_string());
        let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        write_context_file(
            dir.path(),
            &agent_id,
            &changeset_id,
            &base_commit,
            Some("task"),
        )
        .unwrap();

        let before = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();

        append_context_update(dir.path(), "Agent `b1` submitted changeset `cs-2`.\n").unwrap();

        let after = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();

        // Static preamble is preserved byte-for-byte.
        assert!(
            after.starts_with(&before),
            "Appended content must not alter the static preamble"
        );

        // Update is present at the end.
        assert!(
            after.contains("Agent `b1` submitted changeset `cs-2`."),
            "Update should be appended"
        );
    }

    #[test]
    fn append_preserves_preamble_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("x".to_string());
        let changeset_id = phantom_core::id::ChangesetId("cs-0".to_string());
        let base_commit = phantom_core::id::GitOid([0xAB; 20]);

        write_context_file(dir.path(), &agent_id, &changeset_id, &base_commit, None).unwrap();

        let original = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();

        // Append two updates.
        append_context_update(dir.path(), "First update\n").unwrap();
        append_context_update(dir.path(), "Second update\n").unwrap();

        let final_content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();

        // The original content (including the Trunk Updates header) is an exact prefix.
        assert!(
            final_content.starts_with(&original),
            "Multiple appends must not alter the original content"
        );
        assert!(final_content.contains("First update"));
        assert!(final_content.contains("Second update"));
    }

    #[test]
    fn append_is_noop_when_no_context_file() {
        let dir = tempfile::tempdir().unwrap();
        // No context file written — append should succeed silently.
        append_context_update(dir.path(), "should not crash\n").unwrap();

        // No file should have been created.
        assert!(!dir.path().join(CONTEXT_FILE).exists());
    }

    #[test]
    fn toolchain_block_injected_before_task_section() {
        use phantom_toolchain::{DetectedLanguage, Toolchain};

        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("a".into());
        let changeset_id = phantom_core::id::ChangesetId("cs".into());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        let toolchain = Toolchain {
            language: Some(DetectedLanguage::Rust),
            test_cmd: Some("cargo test".into()),
            build_cmd: Some("cargo build".into()),
            lint_cmd: Some("cargo clippy".into()),
            typecheck_cmd: None,
            format_check_cmd: Some("cargo fmt --check".into()),
        };

        write_context_file_with_toolchain(
            dir.path(),
            &agent_id,
            &changeset_id,
            &base_commit,
            Some("do thing"),
            Some(&toolchain),
        )
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
        let verification_pos = content
            .find("## Verification (this repo)")
            .expect("verification block missing");
        let task_pos = content.find("## Task").expect("task section missing");
        let updates_pos = content.find("## Trunk Updates").unwrap();

        assert!(
            verification_pos < task_pos,
            "Verification block must appear before Task"
        );
        assert!(
            task_pos < updates_pos,
            "Task must still appear before Trunk Updates"
        );
        assert!(content.contains("`cargo test`"));
        assert!(!content.contains("Run the type checker"));
    }

    #[test]
    fn toolchain_block_absent_when_toolchain_empty() {
        use phantom_toolchain::Toolchain;

        let dir = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("a".into());
        let changeset_id = phantom_core::id::ChangesetId("cs".into());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        write_context_file_with_toolchain(
            dir.path(),
            &agent_id,
            &changeset_id,
            &base_commit,
            Some("t"),
            Some(&Toolchain::empty()),
        )
        .unwrap();

        let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
        assert!(!content.contains("## Verification (this repo)"));
    }

    #[test]
    fn write_context_file_remains_byte_identical_without_toolchain() {
        // Back-compat: write_context_file (no toolchain) must produce the
        // same bytes as write_context_file_with_toolchain(.., None).
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let agent_id = phantom_core::id::AgentId("a".into());
        let changeset_id = phantom_core::id::ChangesetId("cs".into());
        let base_commit = phantom_core::id::GitOid([0u8; 20]);

        write_context_file(
            dir_a.path(),
            &agent_id,
            &changeset_id,
            &base_commit,
            Some("x"),
        )
        .unwrap();
        write_context_file_with_toolchain(
            dir_b.path(),
            &agent_id,
            &changeset_id,
            &base_commit,
            Some("x"),
            None,
        )
        .unwrap();

        let a = std::fs::read(dir_a.path().join(CONTEXT_FILE)).unwrap();
        let b = std::fs::read(dir_b.path().join(CONTEXT_FILE)).unwrap();
        assert_eq!(a, b);
    }
}

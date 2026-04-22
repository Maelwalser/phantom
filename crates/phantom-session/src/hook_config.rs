//! Per-overlay Claude Code settings file that wires Phantom's notify hook.
//!
//! Claude Code picks up `hooks` from a project-scoped `.claude/settings.json`.
//! That location is trusted by default; settings loaded via the `--settings`
//! CLI flag merge into the session but hooks from untrusted paths do not
//! actually fire (observed empirically: the ripple runs, the queue fills,
//! but the CLI never invokes the hook). So we write directly into the
//! agent's working directory — which, for a Phantom overlay, is the FUSE
//! mount. The `.claude/` prefix is excluded from overlay modified-files so
//! this helper file cannot leak into any agent's changeset.
//!
//! We register three hooks that all point at `phantom _notify-hook --agent X`:
//!
//! - **`UserPromptSubmit`** — fires right before the user's message is sent
//!   to the model. This is the primary delivery path: any pending trunk
//!   updates are injected as `additionalContext` into the very same turn,
//!   so the model sees them alongside the user's request.
//! - **`PostToolUse`** — fires after each tool call. Lets us deliver
//!   notifications that arrived *during* a long tool-heavy turn without
//!   waiting for the next user message.
//! - **`SessionStart`** — fires on boot / resume. Flushes anything
//!   accumulated while the session was stopped.
//!
//! Written fresh every time a session starts — idempotent, safe to
//! regenerate. A marker copy is also dropped under
//! `.phantom/overlays/<agent>/claude-settings.json` for inspection.

use std::path::{Path, PathBuf};

use phantom_core::id::AgentId;
use serde::{Deserialize, Serialize};

/// Filename used for the per-overlay marker copy of the settings.
pub const CLAUDE_SETTINGS_FILE: &str = "claude-settings.json";

/// Relative path Claude Code reads project settings from. Written inside
/// the agent's working directory (the FUSE mount) so Claude picks it up
/// automatically. Filtered from overlay modified-files by the `.claude`
/// excluded prefix in `phantom-overlay::exclusion`.
pub const CLAUDE_PROJECT_SETTINGS_REL: &str = ".claude/settings.json";

/// A single hook command entry — Claude-settings shape, not ours.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct HookCommand {
    #[serde(rename = "type")]
    kind: String,
    command: String,
}

/// A hook matcher group. Claude groups hooks by an optional `matcher` pattern
/// (used for `PreToolUse`/`PostToolUse` to filter by tool name). We leave the
/// matcher empty so the hook runs for every tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct HookMatcher {
    hooks: Vec<HookCommand>,
}

/// Full settings document. Only the `hooks` section is populated — everything
/// else defaults. The real Claude settings schema has many more fields but
/// unknown keys are tolerated, so this minimal document is forward-compatible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ClaudeSettings {
    #[serde(default, skip_serializing_if = "HookSection::is_empty")]
    hooks: HookSection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
struct HookSection {
    #[serde(
        rename = "UserPromptSubmit",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    user_prompt_submit: Vec<HookMatcher>,
    #[serde(rename = "PostToolUse", default, skip_serializing_if = "Vec::is_empty")]
    post_tool_use: Vec<HookMatcher>,
    #[serde(
        rename = "SessionStart",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    session_start: Vec<HookMatcher>,
}

impl HookSection {
    fn is_empty(&self) -> bool {
        self.user_prompt_submit.is_empty()
            && self.post_tool_use.is_empty()
            && self.session_start.is_empty()
    }
}

/// Absolute path to the marker copy of the settings inside
/// `.phantom/overlays/<agent>/`. Useful for operators who want to `cat`
/// the file without chasing the overlay mount path.
#[must_use]
pub fn settings_path(phantom_dir: &Path, agent_id: &AgentId) -> PathBuf {
    phantom_dir
        .join("overlays")
        .join(&agent_id.0)
        .join(CLAUDE_SETTINGS_FILE)
}

/// Absolute path to the `.claude/settings.json` Claude Code actually reads.
/// Lives inside the agent's working directory (FUSE mount).
#[must_use]
pub fn project_settings_path(work_dir: &Path) -> PathBuf {
    work_dir.join(CLAUDE_PROJECT_SETTINGS_REL)
}

/// Write the per-agent Claude hook settings into **two** locations:
///
/// 1. `{work_dir}/.claude/settings.json` — the canonical project-settings
///    path that Claude trusts and actually loads hooks from. This is what
///    triggers the notification hook.
/// 2. `.phantom/overlays/<agent>/claude-settings.json` — a convenience
///    marker copy so operators can inspect the wiring without digging into
///    the overlay mount.
///
/// The hook command invokes the running Phantom binary via its absolute path
/// (obtained from [`std::env::current_exe`]) with a specific agent ID. Using
/// the absolute path avoids PATH-resolution issues inside Claude's hook
/// runner and preserves the identity of the binary that spawned the session.
///
/// Returns the marker path. Returns `Err` only when I/O or serialisation
/// fails. The caller should log and continue — a missing settings file only
/// degrades to the pre-existing file-based notification path.
pub fn write(phantom_dir: &Path, agent_id: &AgentId, work_dir: &Path) -> anyhow::Result<PathBuf> {
    let phantom_bin = current_exe_path()?;
    let marker = settings_path(phantom_dir, agent_id);
    let project = project_settings_path(work_dir);
    write_with_bin(&marker, Some(&project), &phantom_bin, agent_id)
}

/// Same as [`write()`], but the binary path is supplied explicitly and the
/// project-settings destination is optional. Useful for tests that do not
/// want to depend on `current_exe` or an existing FUSE mount.
pub fn write_with_bin(
    marker_path: &Path,
    project_settings_path: Option<&Path>,
    phantom_bin: &Path,
    agent_id: &AgentId,
) -> anyhow::Result<PathBuf> {
    let settings = build_settings(phantom_bin, agent_id);
    let json = serde_json::to_string_pretty(&settings)?;

    if let Some(parent) = marker_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(marker_path, &json)?;

    if let Some(target) = project_settings_path {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, &json)?;
    }

    Ok(marker_path.to_path_buf())
}

/// Build the settings document without writing it. Exposed for tests.
fn build_settings(phantom_bin: &Path, agent_id: &AgentId) -> ClaudeSettings {
    let bin = phantom_bin.display().to_string();
    // Shell out via bash -c so we can route stdout directly; Claude executes
    // the command string via sh. The agent ID is the only variable part and
    // must be quoted to survive spaces (IDs are constrained today but stay
    // defensive).
    let agent_quoted = shell_single_quote(&agent_id.0);

    let user_prompt_cmd = HookCommand {
        kind: "command".into(),
        command: format!("{bin} _notify-hook --agent {agent_quoted} --event UserPromptSubmit"),
    };
    let post_tool_cmd = HookCommand {
        kind: "command".into(),
        command: format!("{bin} _notify-hook --agent {agent_quoted} --event PostToolUse"),
    };
    let session_start_cmd = HookCommand {
        kind: "command".into(),
        command: format!("{bin} _notify-hook --agent {agent_quoted} --event SessionStart"),
    };

    ClaudeSettings {
        hooks: HookSection {
            user_prompt_submit: vec![HookMatcher {
                hooks: vec![user_prompt_cmd],
            }],
            post_tool_use: vec![HookMatcher {
                hooks: vec![post_tool_cmd],
            }],
            session_start: vec![HookMatcher {
                hooks: vec![session_start_cmd],
            }],
        },
    }
}

/// Resolve the absolute path of the currently running Phantom binary.
fn current_exe_path() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    // Best-effort canonicalise so symlinked wrappers (e.g. cargo target/)
    // resolve to a stable path.
    Ok(exe.canonicalize().unwrap_or(exe))
}

/// POSIX-safe single-quote a shell argument: `a'b` → `'a'\''b'`.
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_settings_registers_three_hooks() {
        let settings = build_settings(Path::new("/usr/local/bin/ph"), &AgentId("agent-b".into()));
        assert_eq!(settings.hooks.user_prompt_submit.len(), 1);
        assert_eq!(settings.hooks.post_tool_use.len(), 1);
        assert_eq!(settings.hooks.session_start.len(), 1);
    }

    #[test]
    fn hook_commands_include_binary_and_agent() {
        let settings = build_settings(Path::new("/opt/ph"), &AgentId("agent-b".into()));
        let cmd = &settings.hooks.user_prompt_submit[0].hooks[0].command;
        assert!(cmd.contains("/opt/ph"));
        assert!(cmd.contains("_notify-hook"));
        assert!(cmd.contains("--agent 'agent-b'"));
        assert!(cmd.contains("UserPromptSubmit"));
    }

    #[test]
    fn shell_single_quote_escapes_apostrophes() {
        assert_eq!(shell_single_quote("simple"), "'simple'");
        assert_eq!(shell_single_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_single_quote(""), "''");
    }

    #[test]
    fn write_with_bin_produces_parseable_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("overlays/agent-b/claude-settings.json");
        let written = write_with_bin(
            &path,
            None,
            Path::new("/bin/ph"),
            &AgentId("agent-b".into()),
        )
        .unwrap();
        assert_eq!(written, path);

        let json = std::fs::read_to_string(&path).unwrap();
        assert!(json.contains("UserPromptSubmit"));
        assert!(json.contains("PostToolUse"));
        assert!(json.contains("SessionStart"));
        // Parses back.
        let parsed: ClaudeSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.hooks.user_prompt_submit.len(), 1);
    }

    #[test]
    fn settings_path_lives_inside_overlay_dir() {
        let p = settings_path(Path::new("/repo/.phantom"), &AgentId("a".into()));
        assert_eq!(
            p,
            PathBuf::from("/repo/.phantom/overlays/a/claude-settings.json")
        );
    }

    #[test]
    fn project_settings_path_is_dot_claude_settings_json() {
        let p = project_settings_path(Path::new("/work"));
        assert_eq!(p, PathBuf::from("/work/.claude/settings.json"));
    }

    #[test]
    fn write_with_bin_writes_to_both_locations_when_project_provided() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("marker.json");
        let project = tmp.path().join("work/.claude/settings.json");
        write_with_bin(
            &marker,
            Some(&project),
            Path::new("/bin/ph"),
            &AgentId("agent-b".into()),
        )
        .unwrap();
        assert!(marker.exists());
        assert!(project.exists());
        let marker_json = std::fs::read_to_string(&marker).unwrap();
        let project_json = std::fs::read_to_string(&project).unwrap();
        assert_eq!(marker_json, project_json);
    }

    #[test]
    fn write_overwrites_existing_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.json");
        std::fs::write(&path, "garbage").unwrap();
        write_with_bin(
            &path,
            None,
            Path::new("/bin/ph"),
            &AgentId("agent-b".into()),
        )
        .unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(json.contains("hooks"));
        assert!(!json.contains("garbage"));
    }
}

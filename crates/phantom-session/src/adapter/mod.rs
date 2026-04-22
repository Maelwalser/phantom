//! CLI adapter trait and implementations for session resumption.
//!
//! Each coding CLI (Claude Code, Gemini, OpenCode, etc.) has its own mechanism
//! for session resumption. This module provides a trait-based abstraction so
//! that `phantom <agent>` can capture and replay session IDs regardless of
//! which CLI is being used.
//!
//! Adding support for a new CLI:
//! 1. Create `adapter/<name>.rs` with a struct implementing [`CliAdapter`].
//! 2. Declare `mod <name>;` and `pub use <name>::<Name>Adapter;` below.
//! 3. Add a match arm in [`adapter_for`] for the command basename.

mod claude;
mod gemini;
mod generic;
mod opencode;
mod session;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::process::Command;

pub use claude::ClaudeAdapter;
pub use gemini::GeminiAdapter;
pub use generic::GenericAdapter;
pub use opencode::OpenCodeAdapter;
pub use session::{CliSession, load_session, save_session};

/// Abstraction over different coding CLIs for session management.
///
/// Each implementation knows how to:
/// - Build the CLI command (with or without a resume flag)
/// - Extract a session ID from the CLI's terminal output
pub trait CliAdapter {
    /// Short name used to match against stored sessions (e.g. "claude").
    fn name(&self) -> &str;

    /// Build the `Command` to spawn. When `session_id` is `Some`, the command
    /// should include whatever flag the CLI uses to resume a prior session.
    ///
    /// When `system_prompt_file` is `Some`, the CLI should append the file's
    /// contents to its system prompt (e.g. `--append-system-prompt-file`).
    ///
    /// When `hook_settings_file` is `Some`, the CLI should load it as an
    /// extra settings/config file (e.g. Claude's `--settings <path>`). This
    /// is where Phantom registers its `_notify-hook` integration. Adapters
    /// whose CLI does not support hooks may ignore the argument.
    fn build_command(
        &self,
        work_dir: &Path,
        session_id: Option<&str>,
        env_vars: &[(&str, &str)],
        system_prompt_file: Option<&Path>,
        hook_settings_file: Option<&Path>,
    ) -> Command;

    /// Build a headless (non-interactive) command for background execution.
    ///
    /// Returns `Some(Command)` if the CLI supports headless mode, `None` otherwise.
    /// For Claude Code this uses `-p` (prompt mode) instead of interactive mode.
    ///
    /// When `system_prompt_file` is `Some`, the CLI should append the file's
    /// contents to its system prompt (e.g. `--append-system-prompt-file`).
    ///
    /// When `hook_settings_file` is `Some`, the CLI should load it as an
    /// extra settings/config file (see [`build_command`](Self::build_command)).
    fn build_headless_command(
        &self,
        _work_dir: &Path,
        _task: &str,
        _env_vars: &[(&str, &str)],
        _system_prompt_file: Option<&Path>,
        _hook_settings_file: Option<&Path>,
    ) -> Option<Command> {
        None
    }

    /// Scan the trailing output buffer for a session ID.
    ///
    /// Called with the last ~8 KB of terminal output after the process exits.
    /// Returns `Some(id)` if a resumable session ID is found.
    fn extract_session_id(&self, output_tail: &str) -> Option<String>;
}

/// Return the appropriate CLI adapter for the given command.
///
/// Recognises `claude`, `gemini`, and `opencode` by basename (so both
/// `"claude"` and `"/usr/bin/claude"` resolve to `ClaudeAdapter`).
/// Everything else falls through to `GenericAdapter`.
pub fn adapter_for(command: &str) -> Box<dyn CliAdapter> {
    let basename = Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(command);

    match basename {
        "claude" => Box::new(ClaudeAdapter),
        "gemini" => Box::new(GeminiAdapter),
        "opencode" => Box::new(OpenCodeAdapter),
        _ => Box::new(GenericAdapter {
            command: command.to_string(),
        }),
    }
}

//! Claude Code (`claude`) CLI adapter.

use std::path::Path;
use std::process::Command;

use regex::Regex;

use super::CliAdapter;

pub struct ClaudeAdapter;

impl CliAdapter for ClaudeAdapter {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn build_command(
        &self,
        work_dir: &Path,
        session_id: Option<&str>,
        env_vars: &[(&str, &str)],
        system_prompt_file: Option<&Path>,
    ) -> Command {
        let mut cmd = Command::new("claude");
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        // Resume a prior session if we have one.
        if let Some(id) = session_id {
            cmd.args(["--resume", id]);
        }

        cmd.args(["--allowedTools", "Edit", "Write", "Read", "Bash"]);

        if let Some(dir_str) = work_dir.to_str() {
            cmd.args(["--add-dir", dir_str]);
        }

        // Inject custom instructions while preserving built-in capabilities.
        if let Some(path) = system_prompt_file
            && let Some(path_str) = path.to_str()
        {
            cmd.args(["--append-system-prompt-file", path_str]);
        }

        cmd
    }

    fn build_headless_command(
        &self,
        work_dir: &Path,
        task: &str,
        env_vars: &[(&str, &str)],
        system_prompt_file: Option<&Path>,
    ) -> Option<Command> {
        let mut cmd = Command::new("claude");
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        // Use -p for non-interactive prompt mode.
        cmd.args(["-p", task]);
        cmd.args(["--allowedTools", "Edit", "Write", "Read", "Bash"]);

        if let Some(dir_str) = work_dir.to_str() {
            cmd.args(["--add-dir", dir_str]);
        }

        // Inject custom instructions while preserving built-in capabilities.
        if let Some(path) = system_prompt_file
            && let Some(path_str) = path.to_str()
        {
            cmd.args(["--append-system-prompt-file", path_str]);
        }

        Some(cmd)
    }

    fn extract_session_id(&self, output_tail: &str) -> Option<String> {
        // Claude Code prints: "claude --resume <UUID>" near the end of output.
        let re = Regex::new(
            r"claude --resume ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})",
        )
        .ok()?;
        re.captures(output_tail)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }
}

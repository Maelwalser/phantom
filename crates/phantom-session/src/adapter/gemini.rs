//! Gemini (`gemini`) CLI adapter.

use std::path::Path;
use std::process::Command;

use regex::Regex;

use super::CliAdapter;

pub struct GeminiAdapter;

impl CliAdapter for GeminiAdapter {
    fn name(&self) -> &'static str {
        "gemini"
    }

    fn build_command(
        &self,
        work_dir: &Path,
        session_id: Option<&str>,
        env_vars: &[(&str, &str)],
        system_prompt_file: Option<&Path>,
    ) -> Command {
        let mut cmd = Command::new("gemini");
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        // Resume a prior session if we have one.
        if let Some(id) = session_id {
            cmd.args(["--resume", id]);
        }

        // Make the overlay directory visible to Gemini.
        if let Some(dir_str) = work_dir.to_str() {
            cmd.args(["--include-directories", dir_str]);
        }

        // Gemini has no --append-system-prompt-file equivalent.
        // The .phantom-task.md file is in the working directory and will be
        // discoverable by the agent. If the system prompt file is outside
        // work_dir, include its parent directory so Gemini can find it.
        if let Some(path) = system_prompt_file
            && let Some(parent) = path.parent()
            && parent != work_dir
            && let Some(parent_str) = parent.to_str()
        {
            cmd.args(["--include-directories", parent_str]);
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
        let mut cmd = Command::new("gemini");
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        // Use -p for non-interactive prompt mode.
        cmd.args(["-p", task]);

        if let Some(dir_str) = work_dir.to_str() {
            cmd.args(["--include-directories", dir_str]);
        }

        if let Some(path) = system_prompt_file
            && let Some(parent) = path.parent()
            && parent != work_dir
            && let Some(parent_str) = parent.to_str()
        {
            cmd.args(["--include-directories", parent_str]);
        }

        Some(cmd)
    }

    fn extract_session_id(&self, output_tail: &str) -> Option<String> {
        // Gemini CLI prints: "gemini --resume <UUID>" or "gemini -r <UUID>"
        // near the end of output when a session ends.
        let re = Regex::new(
            r"gemini (?:--resume|-r) ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})",
        )
        .ok()?;
        re.captures(output_tail)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }
}

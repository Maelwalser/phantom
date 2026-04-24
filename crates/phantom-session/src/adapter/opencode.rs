//! OpenCode (`opencode`) CLI adapter.

use std::path::Path;
use std::process::Command;

use regex::Regex;

use super::CliAdapter;

pub struct OpenCodeAdapter;

impl CliAdapter for OpenCodeAdapter {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn build_command(
        &self,
        work_dir: &Path,
        session_id: Option<&str>,
        env_vars: &[(&str, &str)],
        _system_prompt_file: Option<&Path>,
        _hook_settings_file: Option<&Path>,
    ) -> Command {
        let mut cmd = Command::new("opencode");

        // OpenCode uses --cwd for explicit working directory control.
        if let Some(dir_str) = work_dir.to_str() {
            cmd.args(["--cwd", dir_str]);
        }
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        // Resume a specific session.
        if let Some(id) = session_id {
            cmd.args(["--session", id]);
        }

        // OpenCode has no system prompt file injection flag.
        // The .phantom-task.md file is in the working directory.

        cmd
    }

    fn build_headless_command(
        &self,
        work_dir: &Path,
        task: &str,
        env_vars: &[(&str, &str)],
        _system_prompt_file: Option<&Path>,
        _hook_settings_file: Option<&Path>,
    ) -> Option<Command> {
        let mut cmd = Command::new("opencode");

        if let Some(dir_str) = work_dir.to_str() {
            cmd.args(["--cwd", dir_str]);
        }
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        cmd.args(["-p", task]);

        Some(cmd)
    }

    fn extract_session_id(&self, output_tail: &str) -> Option<String> {
        use std::sync::OnceLock;

        // Strategy 1: Look for "opencode --session <id>" or "opencode -s <id>".
        static RESUME_RE: OnceLock<Regex> = OnceLock::new();
        let resume_re = RESUME_RE.get_or_init(|| {
            Regex::new(r"opencode (?:--session|-s) ([0-9a-f-]{36}|ses_[a-zA-Z0-9]+)")
                .expect("opencode resume regex is a compile-time constant")
        });
        if let Some(caps) = resume_re.captures(output_tail) {
            return caps.get(1).map(|m| m.as_str().to_string());
        }

        // Strategy 2: Look for a UUID near a "session" keyword.
        static UUID_RE: OnceLock<Regex> = OnceLock::new();
        let uuid_re = UUID_RE.get_or_init(|| {
            Regex::new(
                r"[Ss]ession[^\n]*?([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})",
            )
            .expect("opencode uuid regex is a compile-time constant")
        });
        uuid_re
            .captures(output_tail)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }
}

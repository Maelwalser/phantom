//! Generic adapter for any CLI without built-in session support.
//!
//! Used as the fallback by `adapter_for()` when the command basename does not
//! match any of the known CLIs. The command string is stored verbatim and
//! reported as the adapter's name.

use std::path::Path;
use std::process::Command;

use super::CliAdapter;

pub struct GenericAdapter {
    pub(super) command: String,
}

impl CliAdapter for GenericAdapter {
    fn name(&self) -> &str {
        &self.command
    }

    fn build_command(
        &self,
        work_dir: &Path,
        _session_id: Option<&str>,
        env_vars: &[(&str, &str)],
        _system_prompt_file: Option<&Path>,
        _hook_settings_file: Option<&Path>,
    ) -> Command {
        let mut cmd = Command::new(&self.command);
        cmd.current_dir(work_dir);

        for &(key, val) in env_vars {
            cmd.env(key, val);
        }

        cmd
    }

    fn extract_session_id(&self, _output_tail: &str) -> Option<String> {
        None
    }
}

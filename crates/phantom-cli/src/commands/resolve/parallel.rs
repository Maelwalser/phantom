//! Spawn parallel headless agent processes when conflict context is too
//! large for a single resolver.

use std::path::{Path, PathBuf};

use anyhow::Context;
use phantom_session::adapter;

/// Spawn parallel headless agent processes for independent file groups.
///
/// Each process gets its own context file and log file. All processes share
/// the same work directory (safe because file groups are disjoint).
///
/// Returns the exit code of each agent (indexed by group).
pub(super) fn spawn_parallel_resolve_agents(
    phantom_dir: &Path,
    agent: &str,
    cli_command: &str,
    work_dir: &Path,
    rules_path: &Path,
    context_files: &[PathBuf],
) -> anyhow::Result<Vec<Option<i32>>> {
    let overlay_root = phantom_dir.join("overlays").join(agent);
    let cli_adapter = adapter::adapter_for(cli_command);

    let env_vars: Vec<(&str, &str)> =
        vec![("PHANTOM_AGENT_ID", agent), ("PHANTOM_INTERACTIVE", "0")];

    let mut children = Vec::with_capacity(context_files.len());

    for (i, context_file) in context_files.iter().enumerate() {
        let log_file = overlay_root.join(format!("resolve-{i}.log"));
        let log_handle = std::fs::File::create(&log_file)
            .with_context(|| format!("failed to create resolve log at {}", log_file.display()))?;
        let log_stderr = log_handle
            .try_clone()
            .context("failed to clone log file handle")?;

        let task = format!(
            "Resolve merge conflicts described in {}",
            context_file
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );

        let mut cmd = cli_adapter
            .build_headless_command(work_dir, &task, &env_vars, Some(rules_path), None)
            .context("CLI adapter does not support headless mode")?;

        cmd.stdin(std::process::Stdio::null())
            .stdout(log_handle)
            .stderr(log_stderr);

        let child = cmd.spawn().with_context(|| {
            format!("failed to spawn resolve agent {i} — is '{cli_command}' installed and on PATH?")
        })?;

        println!(
            "    {} Agent {} spawned {}",
            console::style("→").dim(),
            console::style(i).bold(),
            console::style(format!("(PID {})", child.id())).dim()
        );

        children.push(child);
    }

    println!(
        "\n  {} Waiting for {} agents to complete...\n",
        console::style("◌").cyan(),
        console::style(children.len()).bold()
    );

    let mut exit_codes = Vec::with_capacity(children.len());
    for (i, mut child) in children.into_iter().enumerate() {
        let status = child
            .wait()
            .with_context(|| format!("failed to wait for resolve agent {i}"))?;
        let code = status.code();
        if code == Some(0) {
            println!(
                "    {} Agent {}",
                console::style("✓").green(),
                console::style(i).bold()
            );
        } else {
            let code_str = code.map_or_else(|| "signal".into(), |c: i32| format!("exit {c}"));
            println!(
                "    {} Agent {} {}",
                console::style("✗").red(),
                console::style(i).bold(),
                console::style(format!("({code_str})")).dim()
            );
        }
        exit_codes.push(code);
    }

    // Clean up resolve log files.
    for i in 0..context_files.len() {
        let _ = std::fs::remove_file(overlay_root.join(format!("resolve-{i}.log")));
    }

    Ok(exit_codes)
}

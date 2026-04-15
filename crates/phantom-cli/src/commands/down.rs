//! `phantom down` — tear down all Phantom state and unmount all FUSE overlays.
//!
//! This is the safe way to remove Phantom from a repository. It unmounts every
//! active FUSE overlay and kills all agent/monitor processes before removing the
//! `.phantom/` directory. Running `rm -rf .phantom` while FUSE overlays are
//! mounted is dangerous — `rm` will traverse into mount points and delete real
//! repository files (including `.git/`) through the passthrough layer.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use tracing::{info, warn};

#[derive(clap::Args)]
pub struct DownArgs {
    /// Skip the confirmation prompt
    #[arg(long, short = 'f')]
    pub force: bool,
}

pub fn run(args: &DownArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    let phantom_dir = find_phantom_dir(&cwd)?;
    let overlays_dir = phantom_dir.join("overlays");

    // Collect active agents before doing anything destructive.
    let agents = list_agent_dirs(&overlays_dir);

    if !args.force {
        if agents.is_empty() {
            println!(
                "  {} No active overlays. Removing .phantom/ directory.",
                console::style("·").dim()
            );
        } else {
            println!(
                "  {} Active overlays that will be torn down:",
                console::style("⚠").yellow()
            );
            for agent in &agents {
                let has_fuse = agent_has_fuse(&overlays_dir, agent);
                let has_process = agent_has_process(&overlays_dir, agent);
                let mut markers = Vec::new();
                if has_fuse {
                    markers.push("FUSE mounted");
                }
                if has_process {
                    markers.push("process running");
                }
                let suffix = if markers.is_empty() {
                    String::new()
                } else {
                    format!(
                        " {}",
                        console::style(format!("({})", markers.join(", "))).dim()
                    )
                };
                println!("    {} {agent}{suffix}", console::style("·").dim());
            }
            println!();
        }
        println!("  This will unmount all FUSE overlays, kill all agent processes,");
        println!("  and remove the .phantom/ directory. Continue? [y/N]");

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Phase 1: Kill all agent and monitor processes.
    for agent in &agents {
        kill_agent_processes(&overlays_dir, agent);
    }

    // Phase 2: Unmount all FUSE overlays.
    let mut unmount_failures = Vec::new();
    for agent in &agents {
        if !unmount_agent_fuse(&overlays_dir, agent) {
            unmount_failures.push(agent.clone());
        }
    }

    if !unmount_failures.is_empty() {
        eprintln!(
            "  {} Failed to unmount FUSE for: {}",
            console::style("⚠").yellow(),
            unmount_failures.join(", ")
        );
        eprintln!("    Attempting lazy unmount...");

        for agent in &unmount_failures {
            lazy_unmount(&overlays_dir, agent);
        }

        // Give lazy unmounts a moment to complete.
        std::thread::sleep(Duration::from_millis(500));
    }

    // Phase 3: Verify no FUSE mounts remain before removing .phantom/.
    let still_mounted = agents
        .iter()
        .filter(|a| is_fuse_mounted(&overlays_dir.join(a).join("mount")))
        .collect::<Vec<_>>();

    if !still_mounted.is_empty() {
        anyhow::bail!(
            "FUSE overlays still mounted for: {}. \
             Cannot safely remove .phantom/. \
             Manually unmount with: fusermount3 -u .phantom/overlays/<agent>/mount",
            still_mounted
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // Phase 4: Remove .phantom/ directory.
    std::fs::remove_dir_all(&phantom_dir)
        .with_context(|| format!("failed to remove {}", phantom_dir.display()))?;

    println!(
        "  {} Phantom torn down. Repository is back to plain git.",
        console::style("✓").green()
    );
    Ok(())
}

/// List agent directory names under `.phantom/overlays/`.
fn list_agent_dirs(overlays_dir: &Path) -> Vec<String> {
    let mut agents = Vec::new();
    let Ok(entries) = std::fs::read_dir(overlays_dir) else {
        return agents;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|ft| ft.is_dir())
            && let Some(name) = entry.file_name().to_str()
        {
            agents.push(name.to_string());
        }
    }
    agents.sort();
    agents
}

/// Check if an agent has a live FUSE mount.
fn is_fuse_mounted(mount_point: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Some(parent) = mount_point.parent() else {
        return false;
    };

    match (std::fs::metadata(mount_point), std::fs::metadata(parent)) {
        (Ok(m), Ok(p)) => m.dev() != p.dev(),
        _ => false,
    }
}

fn agent_has_fuse(overlays_dir: &Path, agent: &str) -> bool {
    let mount_point = overlays_dir.join(agent).join("mount");
    is_fuse_mounted(&mount_point)
}

fn agent_has_process(overlays_dir: &Path, agent: &str) -> bool {
    let overlay_dir = overlays_dir.join(agent);
    is_pid_alive(&overlay_dir.join("agent.pid")) || is_pid_alive(&overlay_dir.join("monitor.pid"))
}

fn is_pid_alive(pid_file: &Path) -> bool {
    crate::pid_guard::read_pid_file(pid_file)
        .is_some_and(|r| crate::pid_guard::is_process_alive(&r))
}

/// Kill agent and monitor processes for an agent.
fn kill_agent_processes(overlays_dir: &Path, agent: &str) {
    let overlay_dir = overlays_dir.join(agent);

    for pid_name in &["agent.pid", "monitor.pid"] {
        let pid_file = overlay_dir.join(pid_name);
        if let Some(record) = crate::pid_guard::read_pid_file(&pid_file)
            && crate::pid_guard::kill_process(&record, libc::SIGTERM)
        {
            info!(agent, pid = record.pid, file = *pid_name, "sent SIGTERM");
        }
        let _ = std::fs::remove_file(&pid_file);
    }
}

/// Unmount FUSE for an agent. Returns `true` on success.
fn unmount_agent_fuse(overlays_dir: &Path, agent: &str) -> bool {
    let overlay_dir = overlays_dir.join(agent);
    let mount_point = overlay_dir.join("mount");
    let pid_file = overlay_dir.join("fuse.pid");

    // Read the PID record once up front so we can verify process identity
    // before sending any signals. This also fixes a prior bug where the pid
    // file was removed before being read in the clean-unmount path.
    let fuse_record = crate::pid_guard::read_pid_file(&pid_file);

    if !is_fuse_mounted(&mount_point) {
        let _ = std::fs::remove_file(&pid_file);
        return true;
    }

    // Try clean unmount.
    let unmount_ok = std::process::Command::new("fusermount3")
        .arg("-u")
        .arg(&mount_point)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if unmount_ok {
        info!(agent, "FUSE unmounted cleanly");

        // Kill the FUSE daemon process.
        if let Some(ref record) = fuse_record {
            crate::pid_guard::kill_process(record, libc::SIGTERM);
        }
        let _ = std::fs::remove_file(&pid_file);

        return true;
    }

    // Kill the FUSE daemon and retry unmount.
    if let Some(ref record) = fuse_record {
        crate::pid_guard::kill_process(record, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(300));

        let retry_ok = std::process::Command::new("fusermount3")
            .arg("-u")
            .arg(&mount_point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());

        if retry_ok {
            info!(agent, "FUSE unmounted after killing daemon");
            let _ = std::fs::remove_file(&pid_file);
            return true;
        }
    }

    warn!(agent, "failed to unmount FUSE");
    false
}

/// Lazy unmount as a last resort.
fn lazy_unmount(overlays_dir: &Path, agent: &str) {
    let mount_point = overlays_dir.join(agent).join("mount");

    let _ = std::process::Command::new("fusermount3")
        .arg("-uz")
        .arg(&mount_point)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    info!(agent, "attempted lazy unmount");
}

/// Walk up from `start` looking for a `.phantom/` directory.
fn find_phantom_dir(start: &Path) -> anyhow::Result<std::path::PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let candidate = current.join(".phantom");
        if candidate.is_dir() {
            return Ok(candidate);
        }
        if !current.pop() {
            anyhow::bail!(
                "not a Phantom repository (no .phantom/ found above {}). Nothing to tear down.",
                start.display()
            );
        }
    }
}

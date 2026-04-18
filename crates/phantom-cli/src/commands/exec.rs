//! `phantom exec <agent> -- <command...>` — run an arbitrary command inside an
//! agent's FUSE overlay, seeing the merged trunk + agent view.
//!
//! If the overlay's FUSE mount is not already active, it is mounted temporarily
//! for the duration of the command and cleaned up afterwards.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;

use crate::context::PhantomContext;
use crate::fs::fuse;

#[derive(clap::Args)]
pub struct ExecArgs {
    /// Agent whose overlay to run in
    pub agent: String,

    /// Command and arguments to execute (after --)
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

/// Drop guard that unmounts a temporarily-spawned FUSE mount on exit.
struct FuseCleanupGuard {
    phantom_dir: PathBuf,
    agent: String,
    active: bool,
}

impl Drop for FuseCleanupGuard {
    fn drop(&mut self) {
        if self.active {
            super::remove::unmount_fuse(&self.phantom_dir, &self.agent);
        }
    }
}

pub fn run(args: &ExecArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;

    // Validate agent exists.
    let overlay_root = ctx.phantom_dir.join("overlays").join(&args.agent);
    let upper_dir = overlay_root.join("upper");
    if !upper_dir.exists() {
        anyhow::bail!(
            "agent '{}' does not exist. Create it with: ph {}",
            args.agent,
            args.agent
        );
    }

    let mount_point = overlay_root.join("mount");
    let already_mounted = fuse::is_mounted(&mount_point);

    // Ensure FUSE is mounted.
    let mut guard = FuseCleanupGuard {
        phantom_dir: ctx.phantom_dir.clone(),
        agent: args.agent.clone(),
        active: false,
    };

    if !already_mounted {
        // Ensure mount directory exists.
        std::fs::create_dir_all(&mount_point)
            .with_context(|| format!("failed to create mount dir {}", mount_point.display()))?;

        fuse::spawn_daemon(
            &ctx.phantom_dir,
            &ctx.repo_root,
            &args.agent,
            &mount_point,
            &upper_dir,
            &fuse::FsOverrides::default(),
            Duration::from_secs(5),
        )
        .context("failed to mount FUSE overlay")?;

        guard.active = true;
    }

    // Spawn the command inside the overlay.
    let status = std::process::Command::new(&args.command[0])
        .args(&args.command[1..])
        .current_dir(&mount_point)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env("PHANTOM_AGENT_ID", &args.agent)
        .env("PHANTOM_OVERLAY_DIR", &mount_point)
        .env("PHANTOM_REPO_ROOT", &ctx.repo_root)
        .status()
        .with_context(|| format!("failed to execute '{}'", args.command[0]))?;

    // Cleanup runs via guard drop, then propagate exit code.
    let code = status.code().unwrap_or(1);
    drop(guard);
    std::process::exit(code);
}

//! Hidden `_fuse-mount` subcommand — FUSE daemon for agent overlays.
//!
//! This is NOT meant for direct user invocation. It is spawned by
//! `phantom dispatch` as a background process. The process detaches from
//! the parent session, mounts a [`PhantomFs`] at the given mount point,
//! and blocks until the filesystem is unmounted (via `fusermount3 -u` or
//! process termination).

use std::path::PathBuf;

use anyhow::Context;

#[derive(clap::Args)]
pub struct FuseMountArgs {
    /// Agent identifier.
    #[arg(long)]
    pub agent: String,
    /// Path where the FUSE filesystem will be mounted.
    #[arg(long)]
    pub mount_point: PathBuf,
    /// Path to the agent's upper (write) layer.
    #[arg(long)]
    pub upper_dir: PathBuf,
    /// Path to the trunk (lower/read-only) layer — the git working tree root.
    #[arg(long)]
    pub lower_dir: PathBuf,
}

/// Run the FUSE mount daemon. Blocks until unmount.
///
/// This is intentionally synchronous — `fuser::mount2` blocks in the FUSE
/// event loop. The calling code in `dispatch.rs` spawns this as a detached
/// child process.
pub fn run(args: FuseMountArgs) -> anyhow::Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        anyhow::bail!("FUSE mounting is only supported on Linux");
    }

    #[cfg(target_os = "linux")]
    {
        use fuser::{Config, MountOption};
        use phantom_core::AgentId;
        use phantom_overlay::PhantomFs;
        use phantom_overlay::layer::OverlayLayer;

        // Detach from parent session so we survive CLI exit.
        // SAFETY: setsid() has no memory-safety implications; it only
        // creates a new session and process group for this process.
        unsafe {
            libc::setsid();
        }

        let layer = OverlayLayer::new(args.lower_dir.clone(), args.upper_dir.clone())
            .context("failed to create overlay layer")?;

        let agent_id = AgentId(args.agent.clone());
        let fs = PhantomFs::new(layer, agent_id);

        let mut config = Config::default();
        config.mount_options = vec![
            MountOption::FSName("phantom".to_string()),
            MountOption::DefaultPermissions,
        ];

        tracing::info!(
            mount_point = %args.mount_point.display(),
            agent = %args.agent,
            "starting FUSE mount"
        );

        fuser::mount2(fs, &args.mount_point, &config)
            .context("FUSE mount failed")?;

        tracing::info!("FUSE mount exited cleanly");
        Ok(())
    }
}

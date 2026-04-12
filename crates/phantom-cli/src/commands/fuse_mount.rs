//! Hidden `_fuse-mount` subcommand — FUSE daemon for agent overlays.
//!
//! This is NOT meant for direct user invocation. It is spawned by
//! `phantom dispatch` as a background process. The process detaches from
//! the parent session, mounts a [`PhantomFs`] at the given mount point,
//! and blocks until the filesystem is unmounted (via `fusermount3 -u` or
//! SIGTERM).

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

/// Run the FUSE mount daemon. Blocks until unmount or SIGTERM.
///
/// The FUSE event loop runs on a background thread via `fuser::spawn_mount2`.
/// The main thread waits for SIGTERM and triggers a clean unmount when received.
/// The calling code in `dispatch.rs` spawns this as a detached child process.
pub fn run(args: FuseMountArgs) -> anyhow::Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        anyhow::bail!("FUSE mounting is only supported on Linux");
    }

    #[cfg(target_os = "linux")]
    {
        use std::sync::atomic::{AtomicBool, Ordering};

        use fuser::{Config, MountOption, SessionACL};
        use phantom_core::AgentId;
        use phantom_overlay::PhantomFs;
        use phantom_overlay::layer::OverlayLayer;

        // Flag set by the SIGTERM handler to request shutdown.
        static TERM_RECEIVED: AtomicBool = AtomicBool::new(false);

        extern "C" fn handle_sigterm(_sig: libc::c_int) {
            TERM_RECEIVED.store(true, Ordering::Release);
        }

        // Detach from parent session so we survive CLI exit.
        // SAFETY: setsid() has no memory-safety implications; it only
        // creates a new session and process group for this process.
        unsafe {
            libc::setsid();
        }

        // Register SIGTERM handler before mounting so we never miss a signal.
        // SAFETY: handle_sigterm is async-signal-safe — it only performs an
        // atomic store. sigaction has no memory-safety implications.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle_sigterm as *const () as usize;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        }

        let layer = OverlayLayer::new(args.lower_dir.clone(), args.upper_dir.clone())
            .context("failed to create overlay layer")?;

        let agent_id = AgentId(args.agent.clone());
        let fs = PhantomFs::new(layer, agent_id);

        let mut config = Config::default();
        config.mount_options = vec![
            MountOption::FSName("phantom".to_string()),
            MountOption::DefaultPermissions,
            MountOption::AutoUnmount,
        ];
        // AutoUnmount requires allow_root or allow_other so fusermount3 can
        // monitor the owning process and unmount when it exits.
        config.acl = SessionACL::RootAndOwner;

        tracing::info!(
            mount_point = %args.mount_point.display(),
            agent = %args.agent,
            "starting FUSE mount"
        );

        let session = fuser::spawn_mount2(fs, &args.mount_point, &config)
            .context("FUSE mount failed")?;

        // Wait for SIGTERM. The background thread runs the FUSE event loop;
        // we just need to watch for the termination signal.
        while !TERM_RECEIVED.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        tracing::info!("SIGTERM received, unmounting gracefully");

        session
            .umount_and_join()
            .context("failed to unmount cleanly")?;

        tracing::info!("FUSE mount exited cleanly");
        Ok(())
    }
}

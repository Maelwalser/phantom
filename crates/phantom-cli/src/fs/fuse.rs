//! FUSE daemon spawn, mount detection, and unmount primitives.
//!
//! Consolidates the FUSE lifecycle operations previously duplicated across
//! `plan`, `task`, `resolve`, `remove`, `down`, and `context`. Commands
//! compose these primitives with their own policy (e.g., fallback to kill
//! on failed unmount, retry with lazy unmount).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;

/// Optional overrides for the FUSE filesystem identity and permissions.
///
/// When all fields are `None`, the FUSE daemon defaults to the current
/// process's UID/GID. For production deployments, set explicit values to
/// project a restricted identity into the overlay.
#[derive(Default)]
pub(crate) struct FsOverrides {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub file_mode: Option<u32>,
    pub dir_mode: Option<u32>,
}

/// Check whether `mount_point` currently has a filesystem mounted by
/// comparing its device ID to the parent directory. A mounted FUSE
/// filesystem always reports a different device than the directory that
/// contains it.
pub(crate) fn is_mounted(mount_point: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Some(parent) = mount_point.parent() else {
        return false;
    };

    match (std::fs::metadata(mount_point), std::fs::metadata(parent)) {
        (Ok(m), Ok(p)) => m.dev() != p.dev(),
        _ => false,
    }
}

/// Poll until a FUSE mount appears at `mount_point`, or time out.
pub(crate) fn wait_for_mount(mount_point: &Path, timeout: Duration) -> anyhow::Result<()> {
    let start = Instant::now();
    loop {
        if is_mounted(mount_point) {
            return Ok(());
        }
        if start.elapsed() > timeout {
            anyhow::bail!("timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Verify the host can mount FUSE filesystems before we try to spawn the
/// daemon. Catches the common "binary installed but libfuse3/fuse3 runtime
/// missing" case with an actionable message instead of a confusing daemon
/// crash deep in the spawn pipeline.
pub(crate) fn preflight() -> anyhow::Result<()> {
    let dev_fuse = Path::new("/dev/fuse");
    if !dev_fuse.exists() {
        anyhow::bail!(
            "/dev/fuse is not present.\n\n\
             Phantom needs the FUSE kernel module and userspace tools.\n\n\
             Install them with:\n  \
               Debian/Ubuntu: sudo apt install fuse3 libfuse3-3\n  \
               Fedora/RHEL:   sudo dnf install fuse3 fuse3-libs\n  \
               Arch:          sudo pacman -S fuse3\n\n\
             Then load the module: sudo modprobe fuse",
        );
    }
    if which_fusermount().is_none() {
        anyhow::bail!(
            "fusermount3 was not found on PATH.\n\n\
             Phantom uses fusermount3 to mount and unmount agent overlays.\n\n\
             Install it with:\n  \
               Debian/Ubuntu: sudo apt install fuse3\n  \
               Fedora/RHEL:   sudo dnf install fuse3\n  \
               Arch:          sudo pacman -S fuse3",
        );
    }
    Ok(())
}

fn which_fusermount() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|p| p.join("fusermount3"))
        .find(|p| p.is_file())
}

/// Spawn a `_fuse-mount` daemon child, write its PID file, and wait for
/// the mount to become active. Kills the child and returns an error if the
/// mount does not appear within `timeout`.
pub(crate) fn spawn_daemon(
    phantom_dir: &Path,
    repo_root: &Path,
    agent: &str,
    mount_point: &Path,
    upper_dir: &Path,
    overrides: &FsOverrides,
    timeout: Duration,
) -> anyhow::Result<()> {
    preflight()?;
    let phantom_bin = std::env::current_exe().context("failed to find phantom binary")?;
    let overlay_root = phantom_dir.join("overlays").join(agent);
    let pid_file = overlay_root.join("fuse.pid");
    let log_file = overlay_root.join("fuse.log");

    let log_handle = std::fs::File::create(&log_file)
        .with_context(|| format!("failed to create FUSE log at {}", log_file.display()))?;

    let mut cmd = Command::new(&phantom_bin);
    cmd.arg("_fuse-mount")
        .arg("--agent")
        .arg(agent)
        .arg("--mount-point")
        .arg(mount_point)
        .arg("--upper-dir")
        .arg(upper_dir)
        .arg("--lower-dir")
        .arg(repo_root);

    if let Some(uid) = overrides.uid {
        cmd.arg("--uid").arg(uid.to_string());
    }
    if let Some(gid) = overrides.gid {
        cmd.arg("--gid").arg(gid.to_string());
    }
    if let Some(mode) = overrides.file_mode {
        cmd.arg("--file-mode").arg(mode.to_string());
    }
    if let Some(mode) = overrides.dir_mode {
        cmd.arg("--dir-mode").arg(mode.to_string());
    }

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log_handle)
        .spawn()
        .context("failed to spawn FUSE daemon")?;

    crate::pid_guard::write_pid_file(&pid_file, child.id() as i32)
        .context("failed to write FUSE PID file")?;

    if let Err(e) = wait_for_mount(mount_point, timeout) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e).with_context(|| {
            format!(
                "FUSE mount did not become ready. Check {}",
                log_file.display()
            )
        });
    }

    Ok(())
}

/// Attempt a clean unmount via `fusermount3 -u`. Returns `true` on success.
pub(crate) fn unmount(mount_point: &Path) -> bool {
    Command::new("fusermount3")
        .arg("-u")
        .arg(mount_point)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Lazy unmount via `fusermount3 -uz` — removes the mount from the
/// filesystem namespace without waiting for in-flight I/O. Best-effort;
/// errors are swallowed. Use as a last resort.
pub(crate) fn lazy_unmount(mount_point: &Path) {
    let _ = Command::new("fusermount3")
        .arg("-uz")
        .arg(mount_point)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

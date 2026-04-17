//! Non-blocking I/O helpers for PTY forwarding.
//!
//! The stdin -> master fd is made non-blocking so a stalled child process
//! (which stops draining the PTY buffer) cannot deadlock the stdin forwarder
//! thread. [`nb_write_all`] polls the shutdown pipe on `WouldBlock` so the
//! thread exits promptly when the session ends.

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::io::RawFd;

/// Size of the rolling buffer that captures the tail of terminal output (bytes).
pub(super) const OUTPUT_TAIL_CAP: usize = 8192;

/// Write `data` to `fd` without blocking indefinitely.
///
/// The fd must be in non-blocking mode. On `WouldBlock`, polls `fd` and
/// `shutdown_fd` together. Returns `false` if the shutdown pipe fires or an
/// unrecoverable error occurs, signalling the caller to exit its loop.
pub(super) fn nb_write_all(fd: RawFd, shutdown_fd: RawFd, data: &[u8]) -> bool {
    use nix::poll::{PollFd, PollFlags, PollTimeout};

    let mut offset = 0;
    while offset < data.len() {
        let n = unsafe { libc::write(fd, data[offset..].as_ptr().cast(), data.len() - offset) };
        if n > 0 {
            offset += n as usize;
            continue;
        }
        if n == 0 {
            return false;
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if err.kind() != io::ErrorKind::WouldBlock {
            return false;
        }
        // WouldBlock — poll until writable or shutdown.
        let fd_borrow = unsafe { BorrowedFd::borrow_raw(fd) };
        let shutdown_borrow = unsafe { BorrowedFd::borrow_raw(shutdown_fd) };
        let mut fds = [
            PollFd::new(fd_borrow, PollFlags::POLLOUT),
            PollFd::new(shutdown_borrow, PollFlags::POLLIN),
        ];
        match nix::poll::poll(&mut fds, PollTimeout::from(5000u16)) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return false,
            Ok(_) => {}
        }
        if let Some(revents) = fds[1].revents()
            && revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
        {
            return false;
        }
    }
    true
}

/// Set a file descriptor to non-blocking mode.
pub(super) fn set_nonblocking(fd: &OwnedFd) -> anyhow::Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

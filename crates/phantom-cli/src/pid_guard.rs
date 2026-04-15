//! PID file management with process identity verification.
//!
//! Pairs each PID with its `/proc/<pid>/stat` start time to detect PID reuse.
//! Between reading a stale PID file and sending a signal, the OS may have
//! recycled the PID for an unrelated process. Verifying the start time
//! shrinks this race window from unbounded to microseconds.

use std::path::Path;

/// A recorded process identity: PID plus optional start time from procfs.
#[derive(Debug, Clone)]
pub(crate) struct PidRecord {
    pub pid: i32,
    /// Clock-tick start time from `/proc/<pid>/stat` field 22 (1-indexed).
    /// `None` for legacy PID files that predate this module.
    pub start_time: Option<u64>,
}

/// Read the `starttime` field from `/proc/<pid>/stat`.
///
/// The format is: `pid (comm) state ppid ...` where `comm` can contain
/// spaces and `)` characters. We find the *last* `)` to safely skip it,
/// then index field 19 (0-indexed) in the remainder, which is `starttime`.
fn read_start_time(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    // Fields after ')': state(0) ppid(1) pgrp(2) session(3) tty_nr(4)
    // tpgid(5) flags(6) minflt(7) cminflt(8) majflt(9) cmajflt(10)
    // utime(11) stime(12) cutime(13) cstime(14) priority(15) nice(16)
    // num_threads(17) itrealvalue(18) starttime(19)
    after_comm
        .split_whitespace()
        .nth(19) // index 0 = state, ..., index 19 = starttime
        .and_then(|s| s.parse().ok())
}

/// Write a PID file in `"pid:starttime"` format.
///
/// Falls back to plain `"pid"` if `/proc/<pid>/stat` is unreadable (e.g. the
/// child hasn't fully started yet — extremely rare on Linux).
pub(crate) fn write_pid_file(path: &Path, pid: i32) -> std::io::Result<()> {
    let content = match read_start_time(pid) {
        Some(st) => format!("{pid}:{st}"),
        None => pid.to_string(),
    };
    std::fs::write(path, content)
}

/// Read and parse a PID file. Handles both new `"pid:starttime"` and legacy
/// `"pid"` formats. Returns `None` on missing file or parse failure.
pub(crate) fn read_pid_file(path: &Path) -> Option<PidRecord> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();

    if let Some((pid_str, st_str)) = trimmed.split_once(':') {
        let pid = pid_str.parse().ok()?;
        let start_time = st_str.parse().ok()?;
        Some(PidRecord {
            pid,
            start_time: Some(start_time),
        })
    } else {
        let pid = trimmed.parse().ok()?;
        Some(PidRecord {
            pid,
            start_time: None,
        })
    }
}

/// Check whether the process described by `record` is still alive and is the
/// *same* process that was originally recorded (not a PID-reuse impostor).
pub(crate) fn is_process_alive(record: &PidRecord) -> bool {
    // SAFETY: kill(pid, 0) checks process existence without sending a signal.
    let exists = unsafe { libc::kill(record.pid, 0) } == 0;
    if !exists {
        return false;
    }

    // If we have a recorded start time, verify it still matches.
    if let Some(expected) = record.start_time {
        matches!(read_start_time(record.pid), Some(actual) if actual == expected)
    } else {
        // Legacy PID file without start time — fall back to kill-only check.
        true
    }
}

/// Send `signal` to the process described by `record`, but only after
/// verifying that it is still the original process. Returns `true` if the
/// signal was sent, `false` if the process was dead or its identity did not
/// match (PID reuse detected).
pub(crate) fn kill_process(record: &PidRecord, signal: i32) -> bool {
    if !is_process_alive(record) {
        return false;
    }
    // SAFETY: We just verified the process identity. The residual TOCTOU
    // window is microseconds — acceptable without pidfd_open.
    unsafe {
        libc::kill(record.pid, signal);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_start_time_for_self() {
        let pid = std::process::id() as i32;
        let st = read_start_time(pid);
        assert!(st.is_some(), "should read our own start time from procfs");
        assert!(st.unwrap() > 0);
    }

    #[test]
    fn read_start_time_nonexistent_pid() {
        // PID 0 is the kernel scheduler — /proc/0/stat may or may not exist
        // but PID 4_000_000 is above Linux's default pid_max (4194304 is the
        // absolute ceiling; most systems use 32768 or 4194304).
        assert!(read_start_time(i32::MAX).is_none());
    }

    #[test]
    fn roundtrip_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.pid");
        let pid = std::process::id() as i32;

        write_pid_file(&path, pid).unwrap();
        let record = read_pid_file(&path).unwrap();

        assert_eq!(record.pid, pid);
        assert!(record.start_time.is_some());
    }

    #[test]
    fn parse_legacy_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.pid");

        std::fs::write(&path, "12345").unwrap();
        let record = read_pid_file(&path).unwrap();

        assert_eq!(record.pid, 12345);
        assert!(record.start_time.is_none());
    }

    #[test]
    fn is_process_alive_for_self() {
        let pid = std::process::id() as i32;
        let st = read_start_time(pid).unwrap();
        let record = PidRecord {
            pid,
            start_time: Some(st),
        };
        assert!(is_process_alive(&record));
    }

    #[test]
    fn is_process_alive_wrong_start_time() {
        let pid = std::process::id() as i32;
        let record = PidRecord {
            pid,
            start_time: Some(1), // wrong start time
        };
        assert!(!is_process_alive(&record));
    }

    #[test]
    fn is_process_alive_nonexistent() {
        let record = PidRecord {
            pid: i32::MAX,
            start_time: Some(999),
        };
        assert!(!is_process_alive(&record));
    }
}

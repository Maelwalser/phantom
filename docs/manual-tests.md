# Manual Tests

Some failure modes are difficult to automate in CI because they require
real FUSE mounts, subprocess lifecycles, or kernel-level behavior.  The
scenarios listed here are verified by hand before each release.  Each
section includes repro steps and the expected outcome.

## FUSE mount failure recovery

**Goal:** verify that a failed FUSE mount does not leave orphan daemons,
does not corrupt the event log, and surfaces a clear error to the user.

```bash
# Setup a phantom workspace.
cd /tmp && rm -rf phantom-fuse-fail && git init phantom-fuse-fail \
  && cd phantom-fuse-fail \
  && git commit --allow-empty -m init \
  && phantom init

# Force a mount failure by pointing the mount at a read-only parent.
sudo mkdir -p /mnt/ro && sudo chmod 0555 /mnt/ro
PHANTOM_OVERLAYS_DIR=/mnt/ro phantom agent-a --background 2>&1 \
  | tee /tmp/phantom-fuse-fail.log

# Expected:
#   - Command exits with a non-zero status.
#   - The log contains "FUSE mount failed" (or similar) — not a panic.
#   - `phantom status` shows agent-a as not running.
#   - `pgrep -af phantom _fuse-mount` is empty (no orphan daemon).

sudo chmod 0755 /mnt/ro
```

## PTY orphan cleanup

**Goal:** verify that killing the phantom parent process during an
interactive PTY session does not leave a zombie Claude Code child.

```bash
# Launch an interactive session in the background.
cd /tmp/phantom-fuse-fail
phantom agent-b &
PHANTOM_PID=$!
sleep 3

# Identify the child PTY process.
CHILD_PID=$(pgrep -P $PHANTOM_PID claude || pgrep -P $PHANTOM_PID)

# Kill the parent with SIGKILL.
kill -9 $PHANTOM_PID

# Expected:
#   - Within ~2 seconds, `ps -p $CHILD_PID` returns "no such process".
#   - No zombie processes: `ps -eo state,pid,comm | grep " Z "` is empty
#     for the relevant PIDs.
#   - `phantom status` recovers cleanly on next run (overlay still exists,
#     agent_pid is reset).

phantom status
```

## Kernel-side inode churn

**Goal:** exercise `readdir` pagination against a directory containing
more entries than a single FUSE page holds.

```bash
cd /tmp/phantom-fuse-fail
phantom stress --background
ssh $(hostname) "cd /tmp/phantom-fuse-fail/.phantom/overlays/stress/mount \
  && for i in $(seq 1 5000); do touch f$i; done \
  && ls | wc -l"

# Expected:
#   - `ls | wc -l` prints 5000 (no dropped or duplicated entries).
```

## Graceful shutdown under SIGTERM

**Goal:** verify the FUSE daemon unmounts cleanly when the parent
receives SIGTERM.

```bash
phantom agent-c &
sleep 2
kill -TERM %1
wait

# Expected:
#   - `findmnt .phantom/overlays/agent-c/mount` returns no output.
#   - The overlay directory is intact (upper/ still has agent work).
```

---

Failures of any of these checks should be filed as issues before the
release is cut. See the automated counterparts in
`tests/integration/tests/` for the scenarios that CI does cover.

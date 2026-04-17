# Manual Tests

Some failure modes are difficult to automate in CI because they require
real FUSE mounts, subprocess lifecycles, or kernel-level behavior. The
scenarios listed here are verified by hand before each release. Each
section includes repro steps and the expected outcome.

Binary name: `ph`. Integration-test counterparts for automated coverage
live in `tests/integration/tests/`.

## FUSE mount failure recovery

**Goal:** verify that a failed FUSE mount does not leave orphan daemons,
does not corrupt the event log, and surfaces a clear error to the user.

```bash
# Set up a phantom workspace.
cd /tmp && rm -rf phantom-fuse-fail && git init phantom-fuse-fail \
  && cd phantom-fuse-fail \
  && git commit --allow-empty -m init \
  && ph init

# Force a mount failure by pointing the overlay root at a read-only parent.
sudo mkdir -p /mnt/ro && sudo chmod 0555 /mnt/ro
PHANTOM_OVERLAYS_DIR=/mnt/ro ph agent-a --background --task "noop" 2>&1 \
  | tee /tmp/phantom-fuse-fail.log

# Expected:
#   - Command exits with a non-zero status.
#   - The log contains "FUSE mount failed" (or similar) — not a panic.
#   - `ph status` shows agent-a as not running.
#   - `pgrep -af 'ph _fuse-mount'` is empty (no orphan daemon).

sudo chmod 0755 /mnt/ro
```

## PTY orphan cleanup

**Goal:** verify that killing the `ph` parent process during an
interactive PTY session does not leave a zombie Claude Code child.

```bash
# Launch an interactive session in the background.
cd /tmp/phantom-fuse-fail
ph agent-b &
PH_PID=$!
sleep 3

# Identify the child PTY process (Claude Code or generic CLI).
CHILD_PID=$(pgrep -P $PH_PID claude || pgrep -P $PH_PID)

# Kill the parent with SIGKILL.
kill -9 $PH_PID

# Expected:
#   - Within ~2 seconds, `ps -p $CHILD_PID` returns "no such process".
#   - No zombie processes: `ps -eo state,pid,comm | grep " Z "` is empty
#     for the relevant PIDs.
#   - `ph status` recovers cleanly on next run (overlay still exists,
#     agent_pid is reset).

ph status
```

## Kernel-side inode churn

**Goal:** exercise `readdir` pagination against a directory containing
more entries than a single FUSE page holds.

```bash
cd /tmp/phantom-fuse-fail
ph stress --background --task "inode churn"
(
  cd /tmp/phantom-fuse-fail/.phantom/overlays/stress/mount
  for i in $(seq 1 5000); do touch "f$i"; done
  ls | wc -l
)

# Expected:
#   - `ls | wc -l` prints 5000 (no dropped or duplicated entries).
#   - `ph submit stress` succeeds and records 5000 AddFile operations.
```

## Graceful shutdown under SIGTERM

**Goal:** verify the FUSE daemon unmounts cleanly when the parent
receives SIGTERM.

```bash
cd /tmp/phantom-fuse-fail
ph agent-c &
sleep 2
kill -TERM %1
wait

# Expected:
#   - `findmnt .phantom/overlays/agent-c/mount` returns no output.
#   - The overlay directory is intact (upper/ still has agent work).
#   - `ph resume` still lists agent-c as resumable.
```

## `ph down` under active mounts

**Goal:** verify the safe teardown path unmounts every overlay before
removing `.phantom/`.

```bash
cd /tmp/phantom-fuse-fail
ph agent-d --background --task "pending"
ph agent-e --background --task "pending"

ph down -f

# Expected:
#   - Both FUSE mounts are unmounted before .phantom/ is removed.
#   - `findmnt | grep .phantom` returns empty.
#   - `/tmp/phantom-fuse-fail/.phantom/` is gone.
#   - No orphaned `ph _fuse-mount` or `ph _agent-monitor` processes.
```

## Live rebase with concurrent agent writes

**Goal:** verify live rebase does not race with an agent actively
writing to a shadowed file.

```bash
cd /tmp/phantom-fuse-fail

# Agent A modifies handlers.rs (adds handle_register).
ph agent-a --background --task "add handle_register to handlers.rs"

# Agent B starts writing to handlers.rs as well.
ph agent-b
# (inside the session, add handle_admin to handlers.rs, leave idle)

# Agent A submits.
ph sub agent-a

# Expected:
#   - Trunk now contains handle_register.
#   - Agent B's upper/handlers.rs has been live-rebased to include
#     BOTH handle_register (from trunk) AND handle_admin (agent B).
#   - `ph log agent-b` shows a LiveRebased event with handlers.rs in
#     merged_files.
#   - A trunk-notifications/*.json file appeared under
#     .phantom/overlays/agent-b/.
```

## `ph exec` on a stopped overlay

**Goal:** verify `ph exec` transparently mounts the overlay when it is
not already mounted and unmounts cleanly on exit.

```bash
cd /tmp/phantom-fuse-fail
ph agent-f --background --task "some edits"
# Simulate a reboot — just unmount by hand.
fusermount -u .phantom/overlays/agent-f/mount

ph exec agent-f -- ls -la

# Expected:
#   - The command lists the merged view (trunk + agent-f's writes).
#   - After exit, `findmnt .phantom/overlays/agent-f/mount` is empty
#     again (the guard re-unmounted it).
```

---

Failures of any of these checks should be filed as issues before the
release is cut. See the automated counterparts in
`tests/integration/tests/` for the scenarios that CI does cover:

- `two_agents_disjoint`, `two_agents_same_file`, `two_agents_same_symbol`
- `materialize_and_ripple`
- `rollback_replay`
- `event_log_query`
- `semantic_merge_fallback`
- `empty_submit_is_noop`
- `concurrent_submit_overlay_write`
- `corrupted_event_store`
- `materialize_append_crash`

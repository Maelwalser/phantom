#!/usr/bin/env bash
# End-to-end CLI test for Phantom.
#
# Creates a temp repo, tasks 2 agents, makes changes,
# submits and materializes both, then rolls back the first
# materialization to verify the CODE is actually reverted
# (not just the event log).
#
# Usage: bash tests/e2e_cli_test.sh

set -euo pipefail

PHANTOM="$(cd "$(dirname "$0")/.." && pwd)/target/debug/phantom"
PASS=0
FAIL=0
TOTAL=0

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

assert_ok() {
    TOTAL=$((TOTAL + 1))
    local desc="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        PASS=$((PASS + 1))
        echo -e "  ${GREEN}PASS${RESET} $desc"
    else
        FAIL=$((FAIL + 1))
        echo -e "  ${RED}FAIL${RESET} $desc"
    fi
}

assert_contains() {
    TOTAL=$((TOTAL + 1))
    local desc="$1"
    local haystack="$2"
    local needle="$3"
    if echo "$haystack" | grep -qF "$needle"; then
        PASS=$((PASS + 1))
        echo -e "  ${GREEN}PASS${RESET} $desc"
    else
        FAIL=$((FAIL + 1))
        echo -e "  ${RED}FAIL${RESET} $desc — expected to find: '$needle'"
    fi
}

assert_not_contains() {
    TOTAL=$((TOTAL + 1))
    local desc="$1"
    local haystack="$2"
    local needle="$3"
    if echo "$haystack" | grep -qF "$needle"; then
        FAIL=$((FAIL + 1))
        echo -e "  ${RED}FAIL${RESET} $desc — should NOT contain: '$needle'"
    else
        PASS=$((PASS + 1))
        echo -e "  ${GREEN}PASS${RESET} $desc"
    fi
}

assert_file_contains() {
    TOTAL=$((TOTAL + 1))
    local desc="$1"
    local file="$2"
    local needle="$3"
    if grep -qF "$needle" "$file" 2>/dev/null; then
        PASS=$((PASS + 1))
        echo -e "  ${GREEN}PASS${RESET} $desc"
    else
        FAIL=$((FAIL + 1))
        echo -e "  ${RED}FAIL${RESET} $desc — '$file' should contain: '$needle'"
    fi
}

assert_file_not_contains() {
    TOTAL=$((TOTAL + 1))
    local desc="$1"
    local file="$2"
    local needle="$3"
    if grep -qF "$needle" "$file" 2>/dev/null; then
        FAIL=$((FAIL + 1))
        echo -e "  ${RED}FAIL${RESET} $desc — '$file' should NOT contain: '$needle'"
    else
        PASS=$((PASS + 1))
        echo -e "  ${GREEN}PASS${RESET} $desc"
    fi
}

# ────────────────────────────────────────────────────────────
# Setup: create temp repo with initial content
# ────────────────────────────────────────────────────────────
TMPDIR=$(mktemp -d /tmp/phantom-e2e-XXXXXX)
trap 'rm -rf "$TMPDIR"' EXIT

echo -e "${BOLD}${CYAN}=== Phantom E2E CLI Test ===${RESET}"
echo "  Temp dir: $TMPDIR"
echo ""

cd "$TMPDIR"
git init -q .
git config user.email "test@phantom.dev"
git config user.name "Phantom Test"

# Create initial source files
mkdir -p src
cat > src/lib.rs << 'RUST'
/// Core library
pub fn hello() -> &'static str {
    "hello"
}
RUST

cat > src/utils.rs << 'RUST'
/// Utility helpers
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
RUST

git add -A
git commit -q -m "initial commit"

echo -e "${BOLD}1. Initialize Phantom${RESET}"
OUTPUT=$($PHANTOM init 2>&1)
assert_contains "phantom init succeeds" "$OUTPUT" "Phantom initialized"
assert_ok ".phantom/ directory exists" test -d .phantom
assert_ok ".phantom/events.db exists" test -f .phantom/events.db
assert_ok ".phantom/config.toml exists" test -f .phantom/config.toml
assert_ok ".phantom/overlays/ exists" test -d .phantom/overlays

# Commit the .gitignore that phantom init creates
git add .gitignore && git commit -q -m "add .gitignore"
echo ""

# ────────────────────────────────────────────────────────────
# 2. Task two agents in background mode
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}2. Task two agents${RESET}"

OUTPUT_A=$($PHANTOM task agent-a --background --task "Add a greeting function to lib.rs" 2>&1)
assert_contains "agent-a tasked" "$OUTPUT_A" "Agent 'agent-a' tasked"
assert_ok "agent-a overlay dir exists" test -d .phantom/overlays/agent-a/upper

OUTPUT_B=$($PHANTOM task agent-b --background --task "Add a multiply function to utils.rs" 2>&1)
assert_contains "agent-b tasked" "$OUTPUT_B" "Agent 'agent-b' tasked"
assert_ok "agent-b overlay dir exists" test -d .phantom/overlays/agent-b/upper

# Extract changeset IDs from task output
CS_A=$(echo "$OUTPUT_A" | grep "Changeset:" | awk '{print $2}')
CS_B=$(echo "$OUTPUT_B" | grep "Changeset:" | awk '{print $2}')
echo "  Agent-a changeset: $CS_A"
echo "  Agent-b changeset: $CS_B"
echo ""

# ────────────────────────────────────────────────────────────
# 3. Simulate agent work (write files to upper layers)
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}3. Simulate agent work${RESET}"

# Agent-a: adds a new function to lib.rs
mkdir -p .phantom/overlays/agent-a/upper/src
cat > .phantom/overlays/agent-a/upper/src/lib.rs << 'RUST'
/// Core library
pub fn hello() -> &'static str {
    "hello"
}

/// Greet someone by name
pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}
RUST

assert_ok "agent-a wrote src/lib.rs" test -f .phantom/overlays/agent-a/upper/src/lib.rs
echo "  Agent-a: added greet() to src/lib.rs"

# Agent-b: adds a new function to utils.rs
mkdir -p .phantom/overlays/agent-b/upper/src
cat > .phantom/overlays/agent-b/upper/src/utils.rs << 'RUST'
/// Utility helpers
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Multiply two numbers
pub fn multiply(a: i32, b: i32) -> i32 {
    a * b
}
RUST

assert_ok "agent-b wrote src/utils.rs" test -f .phantom/overlays/agent-b/upper/src/utils.rs
echo "  Agent-b: added multiply() to src/utils.rs"
echo ""

# ────────────────────────────────────────────────────────────
# 4. Check status before submit
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}4. Status check (pre-submit)${RESET}"
STATUS=$($PHANTOM status 2>&1)
assert_contains "status shows agent-a" "$STATUS" "agent-a"
assert_contains "status shows agent-b" "$STATUS" "agent-b"
echo ""

# ────────────────────────────────────────────────────────────
# 5. Submit agent-a, then materialize
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}5. Submit & materialize agent-a${RESET}"

SUB_A=$($PHANTOM submit agent-a 2>&1)
assert_contains "agent-a submitted" "$SUB_A" "submitted"

MAT_A=$($PHANTOM materialize agent-a 2>&1)
assert_contains "agent-a materialized" "$MAT_A" "Materialized"

# Verify trunk has the greet function IN THE ACTUAL FILE
assert_file_contains "trunk file has greet()" src/lib.rs "pub fn greet"
assert_file_contains "trunk file still has hello()" src/lib.rs "pub fn hello"

# Also verify via git show
GIT_LIB=$(git show HEAD:src/lib.rs)
assert_contains "git HEAD has greet()" "$GIT_LIB" "pub fn greet"
echo ""

# ────────────────────────────────────────────────────────────
# 6. Submit agent-b, then materialize (different file, no conflict)
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}6. Submit & materialize agent-b${RESET}"

SUB_B=$($PHANTOM submit agent-b 2>&1)
assert_contains "agent-b submitted" "$SUB_B" "submitted"

MAT_B=$($PHANTOM materialize agent-b 2>&1)
assert_contains "agent-b materialized" "$MAT_B" "Materialized"

# Verify trunk has both agents' work IN THE ACTUAL FILES
assert_file_contains "trunk lib.rs has greet()" src/lib.rs "pub fn greet"
assert_file_contains "trunk utils.rs has multiply()" src/utils.rs "pub fn multiply"
assert_file_contains "trunk utils.rs still has add()" src/utils.rs "pub fn add"

# Also verify via git show
GIT_UTILS=$(git show HEAD:src/utils.rs)
assert_contains "git HEAD has multiply()" "$GIT_UTILS" "pub fn multiply"
echo ""

# ────────────────────────────────────────────────────────────
# 7. Check event log
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}7. Event log${RESET}"
LOG=$($PHANTOM log 2>&1)
assert_contains "log has TaskCreated" "$LOG" "TaskCreated"
assert_contains "log has ChangesetSubmitted" "$LOG" "ChangesetSubmitted"
assert_contains "log has submitted" "$LOG" "submitted"

LOG_A=$($PHANTOM log agent-a 2>&1)
assert_contains "agent-a log has events" "$LOG_A" "agent-a"

LOG_B=$($PHANTOM log agent-b 2>&1)
assert_contains "agent-b log has events" "$LOG_B" "agent-b"
echo ""

# ────────────────────────────────────────────────────────────
# 8. Rollback agent-a's materialization — THE CRITICAL TEST
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}8. Rollback agent-a's changeset ($CS_A)${RESET}"

# Record state before rollback
echo "  Before rollback:"
echo "    src/lib.rs contains greet(): $(grep -c 'pub fn greet' src/lib.rs) occurrences"
echo "    src/utils.rs contains multiply(): $(grep -c 'pub fn multiply' src/utils.rs) occurrences"

RB=$($PHANTOM rollback "$CS_A" 2>&1)
assert_contains "rollback dropped events" "$RB" "Dropped"
assert_contains "rollback reverted git commit" "$RB" "Reverted commit"
echo "$RB" | sed 's/^/    /'

echo ""
echo "  After rollback:"

# THE CRITICAL ASSERTIONS: verify the CODE is actually reverted
assert_file_not_contains "greet() REMOVED from src/lib.rs" src/lib.rs "pub fn greet"
assert_file_contains "hello() still in src/lib.rs" src/lib.rs "pub fn hello"

# Agent-b's work should still be there (different file, independent)
assert_file_contains "multiply() still in src/utils.rs" src/utils.rs "pub fn multiply"
assert_file_contains "add() still in src/utils.rs" src/utils.rs "pub fn add"

# Verify via git too
GIT_LIB_AFTER=$(git show HEAD:src/lib.rs)
assert_not_contains "git HEAD: greet() removed" "$GIT_LIB_AFTER" "pub fn greet"
assert_contains "git HEAD: hello() preserved" "$GIT_LIB_AFTER" "pub fn hello"

GIT_UTILS_AFTER=$(git show HEAD:src/utils.rs)
assert_contains "git HEAD: multiply() preserved" "$GIT_UTILS_AFTER" "pub fn multiply"

# Events should also be dropped
LOG_AFTER=$($PHANTOM log 2>&1)
assert_not_contains "CS_A events dropped from log" "$LOG_AFTER" "$CS_A"
echo ""

# ────────────────────────────────────────────────────────────
# 9. Verify the revert commit exists in git history
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}9. Verify git history${RESET}"

GIT_LOG=$(git log --oneline 2>&1)
assert_contains "revert commit in history" "$GIT_LOG" "rollback"
echo "  Git log:"
echo "$GIT_LOG" | sed 's/^/    /'
echo ""

# ────────────────────────────────────────────────────────────
# 10. Test rollback of a non-materialized changeset
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}10. Edge case: task + rollback without materializing${RESET}"

OUTPUT_C=$($PHANTOM task agent-c --background --task "This will be cancelled" 2>&1)
CS_C=$(echo "$OUTPUT_C" | grep "Changeset:" | awk '{print $2}')
assert_contains "agent-c tasked" "$OUTPUT_C" "tasked"

# Write some work
mkdir -p .phantom/overlays/agent-c/upper/src
echo "pub fn cancelled() {}" > .phantom/overlays/agent-c/upper/src/cancelled.rs

# Submit but don't materialize
SUB_C=$($PHANTOM submit agent-c 2>&1)
assert_contains "agent-c submitted" "$SUB_C" "submitted"

# Rollback before materialize — should NOT touch git
HEAD_BEFORE=$(git rev-parse HEAD)
RB_C=$($PHANTOM rollback "$CS_C" 2>&1)
HEAD_AFTER=$(git rev-parse HEAD)

assert_contains "non-materialized rollback drops events" "$RB_C" "Dropped"
assert_contains "notes no git changes" "$RB_C" "not materialized"

TOTAL=$((TOTAL + 1))
if [ "$HEAD_BEFORE" = "$HEAD_AFTER" ]; then
    PASS=$((PASS + 1))
    echo -e "  ${GREEN}PASS${RESET} HEAD unchanged (no spurious revert commit)"
else
    FAIL=$((FAIL + 1))
    echo -e "  ${RED}FAIL${RESET} HEAD changed unexpectedly"
fi
echo ""

# ────────────────────────────────────────────────────────────
# 11. Final state verification
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}11. Final state verification${RESET}"

FINAL_STATUS=$($PHANTOM status 2>&1)
echo "  Final status:"
echo "$FINAL_STATUS" | grep -v "^\[" | sed 's/^/    /'

# The working tree should be clean
GIT_STATUS=$(git status --porcelain)
TOTAL=$((TOTAL + 1))
if [ -z "$GIT_STATUS" ]; then
    PASS=$((PASS + 1))
    echo -e "  ${GREEN}PASS${RESET} git working tree is clean"
else
    FAIL=$((FAIL + 1))
    echo -e "  ${RED}FAIL${RESET} git working tree has uncommitted changes: $GIT_STATUS"
fi
echo ""

# ────────────────────────────────────────────────────────────
# Summary
# ────────────────────────────────────────────────────────────
echo -e "${BOLD}${CYAN}=== Results ===${RESET}"
echo -e "  Total: $TOTAL  ${GREEN}Pass: $PASS${RESET}  ${RED}Fail: $FAIL${RESET}"
echo ""

if [ "$FAIL" -gt 0 ]; then
    echo -e "${RED}Some tests failed.${RESET}"
    exit 1
else
    echo -e "${GREEN}All tests passed.${RESET}"
    exit 0
fi

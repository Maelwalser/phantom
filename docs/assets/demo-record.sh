#!/usr/bin/env bash
# Records the full Phantom demo workflow.
# The VHS tape calls this script and shows its output.
# This avoids issues with dynamic changeset IDs in the tape.
set -euo pipefail

export RUST_LOG=off
DEMO_DIR="/tmp/phantom-demo"
cd "$DEMO_DIR"

echo -e "\033[1;36m# Initialize phantom in a git repo\033[0m"
phantom init
echo ""
sleep 0.3

echo -e "\033[1;36m# Dispatch two agents to work in parallel\033[0m"
phantom task agent-a --background --task "add user registration"
echo ""
phantom task agent-b --background --task "add rate limiting"
echo ""
sleep 0.3

echo -e "\033[1;36m# Check system status\033[0m"
phantom status
echo ""
sleep 0.3

# Simulate agent work (write to overlays)
mkdir -p .phantom/overlays/agent-a/upper/src
cat > .phantom/overlays/agent-a/upper/src/handlers.rs << 'RUST'
pub fn handle_login(user: &str) -> bool { !user.is_empty() }

pub fn handle_register(user: &str, email: &str) -> String {
    format!("Registered {user} with {email}")
}
RUST

mkdir -p .phantom/overlays/agent-b/upper/src
cat > .phantom/overlays/agent-b/upper/src/lib.rs << 'RUST'
pub fn greet(name: &str) -> String { format!("Hello, {name}!") }

pub fn rate_limit(ip: &str, max_requests: u32) -> bool {
    max_requests > 0 && !ip.is_empty()
}
RUST

echo -e "\033[1;36m# Agent A finished — submit and materialize to trunk\033[0m"
SUBMIT_A=$(phantom submit agent-a)
echo "$SUBMIT_A"
# Extract changeset ID from submit output
CS_A=$(echo "$SUBMIT_A" | grep -oP 'cs-\S+' | head -1)
phantom materialize "$CS_A"
echo ""
sleep 0.3

echo -e "\033[1;36m# Agent B finished — different file, auto-merges cleanly\033[0m"
SUBMIT_B=$(phantom submit agent-b)
echo "$SUBMIT_B"
CS_B=$(echo "$SUBMIT_B" | grep -oP 'cs-\S+' | head -1)
phantom materialize "$CS_B"
echo ""
sleep 0.3

echo -e "\033[1;36m# Full audit trail in the event log\033[0m"
phantom log

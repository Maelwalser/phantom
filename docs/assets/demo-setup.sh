#!/usr/bin/env bash
# Sets up the demo repo: git init + `ph init` + two tasked overlays with
# simulated agent work staged in the upper layers.
#
# Run this BEFORE: vhs docs/assets/demo.tape
#
# We stage work directly into the overlay `upper/` dirs instead of spawning
# real background agents so the demo is deterministic and doesn't depend on
# any coding CLI being installed. Each agent's overlay is created via
# `ph <agent> --command true --task "..."`, which emits a `TaskCreated` event
# (so `ph status` shows the task description) without actually running an AI.
set -euo pipefail
export RUST_LOG=off

DEMO_DIR="/tmp/phantom-demo"

# Tear down any previous demo cleanly (unmounts FUSE if still mounted).
if [ -d "$DEMO_DIR/.phantom" ]; then
    (cd "$DEMO_DIR" && ph down -f >/dev/null 2>&1) || true
fi
rm -rf "$DEMO_DIR"
mkdir -p "$DEMO_DIR"
cd "$DEMO_DIR"

# Tiny Rust project — just enough to have real symbols on trunk.
git init -b main --quiet
git config user.email "demo@phantom.dev"
git config user.name  "Phantom Demo"

mkdir src
cat > src/handlers.rs <<'RUST'
pub fn handle_login(user: &str) -> bool {
    !user.is_empty()
}
RUST
cat > src/lib.rs <<'RUST'
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}
RUST
cat > main.rs <<'RUST'
fn main() {
    println!("hello");
}
RUST

git add .
git commit -m "initial commit" --quiet

# Initialize Phantom and create two overlays with real TaskCreated events.
# --command true keeps it synchronous and CLI-free.
ph init >/dev/null
ph agent-a --command true --task "add user registration" >/dev/null
ph agent-b --command true --task "add rate limiting"      >/dev/null

# Simulate the agents writing code. In real usage Claude/Cursor/etc write
# through the FUSE mount; here we drop files straight into the upper layer
# so `ph sub` picks them up just the same.
mkdir -p .phantom/overlays/agent-a/upper/src
mkdir -p .phantom/overlays/agent-b/upper/src

cat > .phantom/overlays/agent-a/upper/src/handlers.rs <<'RUST'
pub fn handle_login(user: &str) -> bool {
    !user.is_empty()
}

pub fn handle_register(user: &str, email: &str) -> String {
    format!("Registered {user} with {email}")
}
RUST

cat > .phantom/overlays/agent-b/upper/src/lib.rs <<'RUST'
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

pub fn rate_limit(ip: &str, max_requests: u32) -> bool {
    max_requests > 0 && !ip.is_empty()
}
RUST

echo "Demo ready in $DEMO_DIR"
echo "Run: vhs docs/assets/demo.tape"

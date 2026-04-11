#!/usr/bin/env bash
# Sets up the demo: repo + phantom init + dispatched agents with staged work.
# Run BEFORE: vhs docs/assets/demo.tape
set -euo pipefail
export RUST_LOG=off

DEMO_DIR="/tmp/phantom-demo"
rm -rf "$DEMO_DIR"
mkdir -p "$DEMO_DIR"
cd "$DEMO_DIR"

# Create a small Rust project
git init -b main --quiet
mkdir src
cat > src/handlers.rs << 'RUST'
pub fn handle_login(user: &str) -> bool {
    !user.is_empty()
}
RUST
cat > src/lib.rs << 'RUST'
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}
RUST
cat > main.rs << 'RUST'
fn main() {
    println!("hello");
}
RUST
git add .
git commit -m "initial commit" --quiet

# Initialize phantom and dispatch agents
phantom up >/dev/null 2>&1
phantom dispatch --agent claude-a --task "add user registration" >/dev/null 2>&1
phantom dispatch --agent claude-b --task "add rate limiting" >/dev/null 2>&1

# Simulate agent work: write files into overlays
# (In real usage, Claude/Cursor writes through the FUSE mount)
mkdir -p .phantom/overlays/claude-a/upper/src
cat > .phantom/overlays/claude-a/upper/src/handlers.rs << 'RUST'
pub fn handle_login(user: &str) -> bool {
    !user.is_empty()
}

pub fn handle_register(user: &str, email: &str) -> String {
    format!("Registered {user} with {email}")
}
RUST

mkdir -p .phantom/overlays/claude-b/upper/src
cat > .phantom/overlays/claude-b/upper/src/lib.rs << 'RUST'
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

pub fn rate_limit(ip: &str, max_requests: u32) -> bool {
    max_requests > 0 && !ip.is_empty()
}
RUST

echo "Demo ready: phantom initialized, agents dispatched, code staged."
echo "Run: vhs docs/assets/demo.tape"

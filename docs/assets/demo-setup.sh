#!/usr/bin/env bash
# Sets up the demo repo for the VHS tape.
# Run BEFORE: vhs docs/assets/demo.tape
set -euo pipefail

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

# Pre-write agent overlay files so the demo doesn't need heredocs.
# The tape runs a one-liner to stage them after dispatch.
mkdir -p /tmp/phantom-staged/claude-a/src
cat > /tmp/phantom-staged/claude-a/src/handlers.rs << 'RUST'
pub fn handle_login(user: &str) -> bool {
    !user.is_empty()
}

pub fn handle_register(user: &str, email: &str) -> String {
    format!("Registered {user} with {email}")
}
RUST

mkdir -p /tmp/phantom-staged/claude-b/src
cat > /tmp/phantom-staged/claude-b/src/lib.rs << 'RUST'
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

pub fn rate_limit(ip: &str, max_requests: u32) -> bool {
    max_requests > 0 && !ip.is_empty()
}
RUST

echo "Demo repo ready at $DEMO_DIR"

#!/usr/bin/env bash
# Sets up a demo repo for the VHS tape recording.
# Run this BEFORE running: vhs docs/assets/demo.tape
set -euo pipefail

DEMO_DIR="/tmp/phantom-demo"
rm -rf "$DEMO_DIR"
mkdir -p "$DEMO_DIR"
cd "$DEMO_DIR"

git init -b main --quiet
mkdir src
echo 'pub fn handle_login(user: &str) -> bool { !user.is_empty() }' > src/handlers.rs
echo 'pub fn greet(name: &str) -> String { format!("Hello, {name}!") }' > src/lib.rs
echo 'fn main() { println!("hello"); }' > main.rs
git add .
git commit -m "initial commit" --quiet

echo "Demo repo ready at $DEMO_DIR"

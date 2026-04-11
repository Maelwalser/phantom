#!/usr/bin/env bash
# Sets up the demo repo and helper scripts for the VHS tape.
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

# Stage the agent work files (copied into overlays after dispatch)
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

# Helper: simulate agents writing code, with human-readable output
cat > /tmp/phantom-demo-work.sh << 'SCRIPT'
#!/usr/bin/env bash
cd /tmp/phantom-demo
cp -r /tmp/phantom-staged/claude-a/* .phantom/overlays/claude-a/upper/ 2>/dev/null
cp -r /tmp/phantom-staged/claude-b/* .phantom/overlays/claude-b/upper/ 2>/dev/null
echo "claude-a  added  handle_register()  → src/handlers.rs"
echo "claude-b  added  rate_limit()       → src/lib.rs"
SCRIPT
chmod +x /tmp/phantom-demo-work.sh

# Helper: submit + materialize in one step (looks up changeset ID automatically)
cat > /tmp/phantom-demo-land.sh << 'SCRIPT'
#!/usr/bin/env bash
export RUST_LOG=off
cd /tmp/phantom-demo
OUTPUT=$(phantom submit --agent "$1" 2>/dev/null)
echo "$OUTPUT"
CS=$(echo "$OUTPUT" | grep -oP 'cs-\S+' | head -1)
phantom materialize --changeset "$CS" 2>/dev/null
SCRIPT
chmod +x /tmp/phantom-demo-land.sh

# Shell wrapper for VHS — starts in the demo dir with logging off
cat > /tmp/phantom-demo-shell << 'SH'
#!/usr/bin/env bash
export RUST_LOG=off
export PS1='$ '
cd /tmp/phantom-demo
exec bash --norc --noprofile -i
SH
chmod +x /tmp/phantom-demo-shell

echo "Demo ready at $DEMO_DIR"

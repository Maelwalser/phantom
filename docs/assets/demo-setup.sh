#!/usr/bin/env bash
# Sets up a demo repo for the VHS tape recording.
# Run BEFORE: vhs docs/assets/demo.tape
set -euo pipefail

DEMO_DIR="/tmp/phantom-demo"
rm -rf "$DEMO_DIR"
mkdir -p "$DEMO_DIR"
cd "$DEMO_DIR"

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

# Write the helper scripts that the tape calls silently
cat > /tmp/phantom-demo-agent-a.sh << 'SH'
#!/usr/bin/env bash
cd /tmp/phantom-demo
mkdir -p .phantom/overlays/claude-a/upper/src
cat > .phantom/overlays/claude-a/upper/src/handlers.rs << 'RUST'
pub fn handle_login(user: &str) -> bool {
    !user.is_empty()
}

pub fn handle_register(user: &str, email: &str) -> String {
    format!("Registered {user} with {email}")
}
RUST
SH

cat > /tmp/phantom-demo-agent-b.sh << 'SH'
#!/usr/bin/env bash
cd /tmp/phantom-demo
mkdir -p .phantom/overlays/claude-b/upper/src
cat > .phantom/overlays/claude-b/upper/src/lib.rs << 'RUST'
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

pub fn rate_limit(ip: &str, max_requests: u32) -> bool {
    max_requests > 0 && !ip.is_empty()
}
RUST
SH

chmod +x /tmp/phantom-demo-agent-a.sh /tmp/phantom-demo-agent-b.sh

echo "Demo repo ready at $DEMO_DIR"

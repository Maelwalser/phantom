# Phantom CLI Demo
# 1. Run: bash docs/assets/demo-setup.sh
# 2. Run: vhs docs/assets/demo.tape

Output docs/assets/demo.gif

Require phantom

Set Shell "bash"
Set FontSize 15
Set Width 960
Set Height 540
Set Theme "Catppuccin Mocha"
Set Padding 16
Set Framerate 24
Set CursorBlink false

# Start in the demo repo with logging suppressed
Hide
Type "export RUST_LOG=off"
Enter
Type "cd /tmp/phantom-demo"
Enter
Sleep 300ms
Show

# ── Initialize ─────────────────────────────────────────────────
Sleep 500ms
Type "phantom up"
Sleep 500ms
Enter
Sleep 1500ms

# ── Dispatch two agents ────────────────────────────────────────
Type@50ms `phantom dispatch --agent agent-a --task "add user registration"`
Sleep 300ms
Enter
Sleep 1500ms

Type@50ms `phantom dispatch --agent agent-b --task "add rate limiting"`
Sleep 300ms
Enter
Sleep 1500ms

# ── Status check ───────────────────────────────────────────────
Type "phantom status"
Sleep 300ms
Enter
Sleep 2500ms

# ── Simulate agent work (hidden) ───────────────────────────────
Hide
Type@0 `mkdir -p .phantom/overlays/agent-a/upper/src`
Enter
Sleep 100ms
Type@0 `printf 'pub fn handle_login(user: &str) -> bool { !user.is_empty() }\n\npub fn handle_register(user: &str, email: &str) -> String {\n    format!("Registered {user} with {email}")\n}\n' > .phantom/overlays/agent-a/upper/src/handlers.rs`
Enter
Sleep 100ms
Type@0 `mkdir -p .phantom/overlays/agent-b/upper/src`
Enter
Sleep 100ms
Type@0 `printf 'pub fn greet(name: &str) -> String { format!("Hello, {name}!") }\n\npub fn rate_limit(ip: &str, max_requests: u32) -> bool {\n    max_requests > 0 && !ip.is_empty()\n}\n' > .phantom/overlays/agent-b/upper/src/lib.rs`
Enter
Sleep 200ms
Show

# ── Submit + materialize Agent A ───────────────────────────────
Type "phantom submit --agent agent-a"
Sleep 300ms
Enter
Sleep 1500ms

# Materialize — capture changeset ID dynamically
Hide
Type@0 `CS_A=$(phantom log --agent agent-a --limit 1 2>/dev/null | grep -oP 'cs-\S+' | head -1)`
Enter
Sleep 300ms
Show
Type@30ms "phantom materialize --changeset $CS_A"
Sleep 300ms
Enter
Sleep 2000ms

# ── Submit + materialize Agent B (semantic merge!) ─────────────
Type "phantom submit --agent agent-b"
Sleep 300ms
Enter
Sleep 1500ms

Hide
Type@0 `CS_B=$(phantom log --agent agent-b --limit 1 2>/dev/null | grep -oP 'cs-\S+' | head -1)`
Enter
Sleep 300ms
Show
Type@30ms "phantom materialize --changeset $CS_B"
Sleep 300ms
Enter
Sleep 2000ms

# ── Event log ──────────────────────────────────────────────────
Type "phantom log"
Sleep 300ms
Enter
Sleep 3500ms

Sleep 1500ms

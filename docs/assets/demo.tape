# Phantom CLI Demo — real user workflow
# 1. Run: bash docs/assets/demo-setup.sh
# 2. Run: vhs docs/assets/demo.tape

Output docs/assets/demo.gif

Require phantom

Set Shell "bash"
Set FontSize 15
Set Width 960
Set Height 600
Set Theme "Catppuccin Mocha"
Set Padding 16
Set Framerate 24
Set TypingSpeed 75ms
Set CursorBlink false
Env RUST_LOG "off"

# cd into demo repo
Type "cd /tmp/phantom-demo"
Sleep 300ms
Enter
Sleep 800ms

# ── Initialize phantom in a git repo ───────────────────────────
Type "phantom up"
Sleep 500ms
Enter
Sleep 2000ms

# ── Dispatch two Claude agents to work in parallel ─────────────
Type@60ms `phantom dispatch --agent claude-a --task "add user registration"`
Sleep 500ms
Enter
Sleep 2500ms

Type@60ms `phantom dispatch --agent claude-b --task "add rate limiting"`
Sleep 500ms
Enter
Sleep 2500ms

# ── Check status ───────────────────────────────────────────────
Type "phantom status"
Sleep 500ms
Enter
Sleep 3500ms

# ── Agents work in their overlays (simulated) ──────────────────
# In real usage, Claude/Cursor writes code through the overlay.
# Here we copy pre-written files to simulate that.
Type@60ms `echo "# Agents writing code in their overlays..."`
Sleep 300ms
Enter
Sleep 1000ms
Type@0 `cp -r /tmp/phantom-staged/claude-a/* .phantom/overlays/claude-a/upper/ 2>/dev/null; cp -r /tmp/phantom-staged/claude-b/* .phantom/overlays/claude-b/upper/ 2>/dev/null`
Enter
Sleep 1000ms

# ── Submit claude-a's changeset ────────────────────────────────
Type "phantom submit --agent claude-a"
Sleep 500ms
Enter
Sleep 2500ms

# ── Materialize claude-a to trunk ──────────────────────────────
Type@60ms `phantom materialize --changeset $(phantom log --agent claude-a | grep -oP 'cs-\S+' | head -1)`
Sleep 500ms
Enter
Sleep 3000ms

# ── Submit claude-b's changeset ────────────────────────────────
Type "phantom submit --agent claude-b"
Sleep 500ms
Enter
Sleep 2500ms

# ── Materialize claude-b — auto-merges! ────────────────────────
Type@60ms `phantom materialize --changeset $(phantom log --agent claude-b | grep -oP 'cs-\S+' | head -1)`
Sleep 500ms
Enter
Sleep 3000ms

# ── Full audit trail ───────────────────────────────────────────
Type "phantom log"
Sleep 500ms
Enter
Sleep 4500ms

Sleep 2000ms

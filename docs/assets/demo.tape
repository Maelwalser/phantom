# Phantom CLI Demo — real user workflow
#
# The setup script has already run:
#   phantom up, phantom dispatch (x2), and agents wrote code.
# This tape shows the commands a user types to check status,
# submit work, materialize to trunk, and query the event log.
#
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

# cd into the demo repo (already initialized by setup script)
Type "cd /tmp/phantom-demo"
Sleep 300ms
Enter
Sleep 800ms

# ── Check what's running ───────────────────────────────────────
Type "phantom status"
Sleep 500ms
Enter
Sleep 3500ms

# ── Submit claude-a's work as a changeset ──────────────────────
Type "phantom submit --agent claude-a"
Sleep 500ms
Enter
Sleep 2500ms

# ── Materialize claude-a to trunk ──────────────────────────────
Type@60ms `phantom materialize --changeset $(phantom log --agent claude-a | grep -oP 'cs-\S+' | head -1)`
Sleep 500ms
Enter
Sleep 3000ms

# ── Submit claude-b's work ─────────────────────────────────────
Type "phantom submit --agent claude-b"
Sleep 500ms
Enter
Sleep 2500ms

# ── Materialize claude-b — different file, auto-merges! ────────
Type@60ms `phantom materialize --changeset $(phantom log --agent claude-b | grep -oP 'cs-\S+' | head -1)`
Sleep 500ms
Enter
Sleep 3000ms

# ── Query the full audit trail ─────────────────────────────────
Type "phantom log"
Sleep 500ms
Enter
Sleep 4500ms

Sleep 2000ms

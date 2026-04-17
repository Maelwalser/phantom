# Phantom CLI demo — real user workflow.
#
# Prereqs:
#   1) `ph` on PATH  (cargo install --path crates/phantom-cli)
#   2) Run: bash docs/assets/demo-setup.sh
#   3) Then: vhs docs/assets/demo.tape

Output docs/assets/demo.gif

Require ph

Set Shell        "bash"
Set FontSize     15
Set Width        960
Set Height       640
Set Theme        "Catppuccin Mocha"
Set Padding      16
Set Framerate    24
Set TypingSpeed  60ms
Set CursorBlink  false
Env RUST_LOG     "off"

# Enter the pre-initialized demo repo (setup script ran first).
Type "cd /tmp/phantom-demo"
Sleep 250ms
Enter
Sleep 600ms

# ── Status: two overlays, each tasked, each with a pending file ──
Type "ph st"
Sleep 400ms
Enter
Sleep 3500ms

# ── Submit agent-a's work: semantic extract → merge → commit ────
Type "ph sub agent-a"
Sleep 400ms
Enter
Sleep 3000ms

# ── Submit agent-b's work: different symbols, auto-merges cleanly ─
Type "ph sub agent-b"
Sleep 400ms
Enter
Sleep 3000ms

# ── Materialization log: human-readable commit summaries per agent ──
Type "ph c"
Sleep 400ms
Enter
Sleep 3500ms

# ── Full audit trail: every task, every submit, every materialization ──
Type "ph l"
Sleep 400ms
Enter
Sleep 4500ms

Sleep 1500ms

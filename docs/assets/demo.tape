# Phantom CLI demo — two saved Claude sessions, per-overlay runs, then submit.
#
# Prereqs:
#   1) `ph` on PATH         (cargo install --path crates/phantom-cli)
#   2) `claude` authenticated
#   3) `python3` on PATH
#   4) Run: bash docs/assets/demo-setup.sh
#   5) Then: vhs docs/assets/demo.tape

Output docs/assets/demo.gif

Require ph
Require claude
Require python3

Set Shell          "bash"
Set FontFamily     "JetBrainsMono Nerd Font"
Set FontSize       16
Set LineHeight     1.2
Set LetterSpacing  0.5
Set Width          1080
Set Height         900
Set Theme          "Catppuccin Mocha"
Set WindowBar      "Colorful"
Set WindowBarSize  40
Set BorderRadius   10
Set Margin         20
Set MarginFill     "#11111b"
Set Padding        20
Set Framerate      30
Set TypingSpeed    60ms
Set CursorBlink    false
Env RUST_LOG       "off"
Env HOME           "/tmp/phantom-home"

# Enter the pre-initialized demo repo (setup script ran first).
Type "cd /tmp/phantom-demo"
Sleep 250ms
Enter
Sleep 600ms

# ── Status: two overlays, each tasked, each with a preserved Claude session ──
Type "ph st"
Sleep 400ms
Enter
Sleep 3500ms

# ── Re-enter agent-a's preserved Claude session (real `claude --resume`) ──
Type "ph agent-a"
Sleep 400ms
Enter
# Trust dialog is suppressed via PHANTOM_CLAUDE_EXTRA_ARGS — the resumed
# session renders directly, as if continuing an existing conversation.
Sleep 5500ms
# Exit the Claude TUI with double Ctrl+C.
Ctrl+C
Sleep 500ms
Ctrl+C
Sleep 1500ms

# ── Re-enter agent-b's preserved Claude session ──
Type "ph agent-b"
Sleep 400ms
Enter
Sleep 5500ms
Ctrl+C
Sleep 500ms
Ctrl+C
Sleep 1500ms

# ── Run the project inside each agent's overlay (isolated merged view) ──
Type "ph x agent-a -- python3 -B main.py"
Sleep 400ms
Enter
Sleep 3000ms

Type "ph x agent-b -- python3 -B main.py"
Sleep 400ms
Enter
Sleep 3000ms

# ── Submit both agents one by one: semantic merge → commit to trunk ──
Type "ph sub agent-a"
Sleep 400ms
Enter
Sleep 3000ms

Type "ph sub agent-b"
Sleep 400ms
Enter
Sleep 3000ms

# ── Run the project on trunk: both agents' prints are now live ──
Type "python3 -B main.py"
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
Sleep 5000ms

Sleep 1500ms

# Phantom CLI Demo
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
Set TypingSpeed 80ms
Set CursorBlink false
Env RUST_LOG "off"

# cd into demo repo — only hidden line
Hide
Type "cd /tmp/phantom-demo"
Enter
Sleep 500ms
Show

# ── Initialize phantom ─────────────────────────────────────────
Sleep 600ms
Type "phantom up"
Sleep 500ms
Enter
Sleep 2000ms

# ── Dispatch two Claude agents in parallel ─────────────────────
Type@65ms `phantom dispatch --agent claude-a --task "add user registration"`
Sleep 500ms
Enter
Sleep 2200ms

Type@65ms `phantom dispatch --agent claude-b --task "add rate limiting"`
Sleep 500ms
Enter
Sleep 2200ms

# ── Check system status ────────────────────────────────────────
Type "phantom status"
Sleep 500ms
Enter
Sleep 3000ms

# ── Agents write code to their overlays ────────────────────────
Type@65ms "bash /tmp/phantom-demo-work.sh"
Sleep 500ms
Enter
Sleep 2000ms

# ── Submit and materialize claude-a to trunk ───────────────────
Type@65ms "bash /tmp/phantom-demo-land.sh claude-a"
Sleep 500ms
Enter
Sleep 2500ms

# ── Submit and materialize claude-b — auto-merges! ─────────────
Type@65ms "bash /tmp/phantom-demo-land.sh claude-b"
Sleep 500ms
Enter
Sleep 2500ms

# ── Query the full audit trail ─────────────────────────────────
Type "phantom log"
Sleep 500ms
Enter
Sleep 4000ms

Sleep 2000ms

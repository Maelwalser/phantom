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

# Hidden env setup — use Sleep to let terminal settle before Show
Hide
Type "export RUST_LOG=off && cd /tmp/phantom-demo && clear"
Enter
Sleep 500ms
Show

# ── Initialize ─────────────────────────────────────────────────
Sleep 400ms
Type "phantom up"
Sleep 400ms
Enter
Sleep 1500ms

# ── Dispatch two Claude agents ─────────────────────────────────
Type@45ms `phantom dispatch --agent claude-a --task "add user registration"`
Sleep 300ms
Enter
Sleep 1500ms

Type@45ms `phantom dispatch --agent claude-b --task "add rate limiting"`
Sleep 300ms
Enter
Sleep 1500ms

# ── Status check ───────────────────────────────────────────────
Type "phantom status"
Sleep 300ms
Enter
Sleep 2500ms

# ── Agents do their work (write overlay files silently) ────────
Hide
Type "bash /tmp/phantom-demo-agent-a.sh && bash /tmp/phantom-demo-agent-b.sh && clear"
Enter
Sleep 500ms
Show

# ── Submit + materialize claude-a ──────────────────────────────
Sleep 300ms
Type "phantom submit --agent claude-a"
Sleep 300ms
Enter
Sleep 1500ms

# Capture changeset ID silently, then type the materialize command
Hide
Type `CS=$(phantom log --agent claude-a --limit 10 2>/dev/null | grep -oP 'cs-\S+' | head -1) && clear`
Enter
Sleep 500ms
Show
Sleep 200ms
Type@35ms "phantom materialize --changeset $CS"
Sleep 300ms
Enter
Sleep 2000ms

# ── Submit + materialize claude-b (auto-merge!) ────────────────
Type "phantom submit --agent claude-b"
Sleep 300ms
Enter
Sleep 1500ms

Hide
Type `CS=$(phantom log --agent claude-b --limit 10 2>/dev/null | grep -oP 'cs-\S+' | head -1) && clear`
Enter
Sleep 500ms
Show
Sleep 200ms
Type@35ms "phantom materialize --changeset $CS"
Sleep 300ms
Enter
Sleep 2000ms

# ── Event log ──────────────────────────────────────────────────
Type "phantom log"
Sleep 300ms
Enter
Sleep 3500ms

Sleep 1500ms

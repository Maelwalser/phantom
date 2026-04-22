# Phantom CLI demo — cross-agent dependency-graph notification.
#
# Flow:
#   1. Show the two trunk files that establish the dependency.
#   2. Open agent-a's resumed Claude session — the signature change Claude
#      already applied is visible in the transcript.
#   3. Submit agent-a: semantic diff detects the signature change and
#      ripples a DependencyImpact to agent-b.
#   4. `ph log agent-b` confirms the AgentNotified audit event.
#   5. Open agent-b's resumed Claude session. A `UserPromptSubmit` hook
#      fires the moment we type a question, draining the pending notification
#      and injecting the trunk-update markdown as additionalContext. Claude's
#      reply then acknowledges agent-a's signature change — proving the
#      hook path reached the model.
#
# Prereqs:
#   1) `ph`, `claude`, `python3`, `jq`, `uuidgen` on PATH
#   2) cargo install --path crates/phantom-cli
#   3) bash docs/assets/demo-setup.sh
#   4) vhs docs/assets/demo.tape

Output docs/assets/demo.gif

Require ph
Require claude
Require python3
Require jq

Set Shell          "bash"
Set FontFamily     "JetBrainsMono Nerd Font"
Set FontSize       16
Set LineHeight     1.2
Set LetterSpacing  0.5
Set Width          1200
Set Height         1000
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

# Enter the pre-initialized demo repo.
Type "cd /tmp/phantom-demo"
Sleep 250ms
Enter
Sleep 500ms

# ── Two overlays, each tasked, each with a preserved Claude session ──
Type "ph st"
Sleep 300ms
Enter
Sleep 3000ms

# ── The dependency at rest on trunk: greet() defined here… ──
Type "cat greetings.py"
Sleep 300ms
Enter
Sleep 3000ms

# ── …called from handle_welcome() here. agent-b owns this file. ──
Type "cat handlers.py"
Sleep 300ms
Enter
Sleep 3500ms

# ── Open agent-a's resumed Claude session. The transcript shows the
#    signature change Claude already applied. Exit with two Ctrl+Cs.
Type "ph agent-a"
Sleep 300ms
Enter
Sleep 6500ms
Ctrl+C
Sleep 400ms
Ctrl+C
Sleep 1500ms

# ── Submit: semantic merge to trunk, ripple out a DependencyImpact. ──
Type "ph sub agent-a"
Sleep 300ms
Enter
Sleep 4000ms

# ── Audit log records agent-b was notified. ──
Type "ph log agent-b"
Sleep 300ms
Enter
Sleep 3500ms

# ── Resume agent-b. The UserPromptSubmit hook fires on the next message
#    and injects the trunk update as additionalContext — Claude's reply
#    will reference agent-a's signature change explicitly.
Type "ph agent-b"
Sleep 300ms
Enter
Sleep 6500ms
Type "What should I know about handle_welcome before I run it? Summarize any recent upstream changes."
Sleep 400ms
Enter
Sleep 32000ms
Ctrl+C
Sleep 400ms
Ctrl+C
Sleep 2000ms

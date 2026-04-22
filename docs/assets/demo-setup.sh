#!/usr/bin/env bash
# Sets up the dependency-graph-notification demo:
#
#   * Trunk defines greetings.greet(name)
#   * Agent-A is tasked with adding a `locale` parameter to greet (signature change)
#   * Agent-B owns handlers.py which calls greet("world"); B adds a log line
#     so handlers.py lands in B's upper and the symbol reference is tracked
#
# Running the tape will:
#   1. show both overlays tasked side-by-side
#   2. submit agent-a → Phantom's semantic diff flags the signature change
#   3. ripple fires a DependencyImpact for agent-b; a pending notification
#      lands in `.phantom/overlays/agent-b/pending-notifications/`
#   4. the hook-ready JSON is previewed (a `cat | jq` of the summary_md)
#   5. agent-b is resumed — Claude's `UserPromptSubmit` hook drains the queue
#      and injects the trunk update as `additionalContext`
#
# Run this BEFORE: vhs docs/assets/demo.tape
#
# Requirements:
#   * `ph`   on PATH (cargo install --path crates/phantom-cli)
#   * `claude` authenticated
#   * `python3`, `jq`, `uuidgen` on PATH
set -euo pipefail
export RUST_LOG=off

DEMO_DIR="/tmp/phantom-demo"

# Sandbox HOME for Claude Code so the TUI header reads "phantom" instead of
# the host user's real email. Reuses real OAuth credentials so no interactive
# login is needed.
CLAUDE_HOME="/tmp/phantom-home"
mkdir -p "$CLAUDE_HOME/.claude"
cp "$HOME/.claude/.credentials.json" "$CLAUDE_HOME/.claude/.credentials.json"
chmod 600 "$CLAUDE_HOME/.claude/.credentials.json"
TRUSTED_PROJECT='{"hasTrustDialogAccepted":true,"hasClaudeMdExternalIncludesApproved":true,"hasClaudeMdExternalIncludesWarningShown":true,"projectOnboardingSeenCount":1,"allowedTools":[],"exampleFiles":[],"mcpServers":{},"mcpContextUris":[],"enabledMcpjsonServers":[],"disabledMcpjsonServers":[]}'
jq --argjson trusted "$TRUSTED_PROJECT" '{
  userID,
  hasCompletedOnboarding,
  lastOnboardingVersion,
  installMethod,
  autoUpdates,
  oauthAccount: (.oauthAccount | {
    accountUuid,
    accountCreatedAt,
    organizationUuid,
    organizationRole,
    workspaceRole,
    billingType,
    subscriptionCreatedAt,
    hasExtraUsageEnabled,
    displayName: "phantom",
    emailAddress: "demo@phantom.dev",
    organizationName: "phantom"
  }),
  projects: {
    "/tmp/phantom-demo": $trusted,
    "/tmp/phantom-demo/.phantom/overlays/agent-a/mount": $trusted,
    "/tmp/phantom-demo/.phantom/overlays/agent-b/mount": $trusted
  }
}' "$HOME/.claude.json" > "$CLAUDE_HOME/.claude.json"
chmod 600 "$CLAUDE_HOME/.claude.json"

# Tear down any previous demo cleanly (unmounts FUSE if still mounted).
if [ -d "$DEMO_DIR/.phantom" ]; then
    (cd "$DEMO_DIR" && ph down -f >/dev/null 2>&1) || true
fi
rm -rf "$DEMO_DIR"
mkdir -p "$DEMO_DIR"
cd "$DEMO_DIR"

# Minimal Python project with a clear cross-file dependency:
#   greetings.greet(name)   — definition
#   handlers.handle_welcome — calls greet("world")
#   main.py                 — calls handle_welcome
git init -b main --quiet
git config user.email "demo@phantom.dev"
git config user.name  "Phantom Demo"

cat > greetings.py <<'PY'
"""Greeting primitives. agent-a will evolve the signature here."""


def greet(name: str) -> None:
    print(f"hello, {name}")
PY

cat > handlers.py <<'PY'
"""Request handlers. agent-b owns this file — it calls greet()."""
from greetings import greet


def handle_welcome() -> None:
    greet("world")
PY

cat > main.py <<'PY'
"""Phantom demo entry point."""
from handlers import handle_welcome


def main() -> None:
    print("phantom demo")
    handle_welcome()


if __name__ == "__main__":
    main()
PY

cat > README.md <<'MD'
# phantom demo — dependency-graph notification

Two agents working in parallel. agent-a changes a symbol's signature,
agent-b holds a caller. Phantom's ripple catches the cross-file
dependency and injects a rich notification into agent-b's next turn via
Claude Code hooks.

Run with:

```
python3 -B main.py
```
MD

cat > .gitignore <<'GI'
__pycache__/
*.pyc
.phantom/
.claude/
GI

git add .
git commit -m "initial commit: greetings + handlers" --quiet

# Initialize Phantom and create two overlays with real TaskCreated events.
ph init >/dev/null
ph agent-a --command true \
    --task "Add a locale parameter to greetings.greet(name)." >/dev/null
ph agent-b --command true \
    --task "Add a print() log line before the greet() call in handlers.py." >/dev/null

# Drive real Claude sessions so each overlay holds finished work whose
# resumption the tape can display. Each session writes through its FUSE
# mount, so the overlay's upper ends up with the agent's edits.
AGENT_A_SID="$(uuidgen)"
AGENT_B_SID="$(uuidgen)"

(
    cd "$DEMO_DIR/.phantom/overlays/agent-a/mount"
    HOME="$CLAUDE_HOME" claude --session-id "$AGENT_A_SID" \
        --permission-mode bypassPermissions \
        -p "Edit greetings.py so \`greet\` takes a second parameter \`locale: str\` and prints \`f\"hello, {name} ({locale})\"\`. Keep the function name. Write the file, then reply in one sentence." \
        >/dev/null
)

(
    cd "$DEMO_DIR/.phantom/overlays/agent-b/mount"
    HOME="$CLAUDE_HOME" claude --session-id "$AGENT_B_SID" \
        --permission-mode bypassPermissions \
        -p "Edit handlers.py: inside handle_welcome, add \`print(\"[welcome] dispatching\")\` on the line BEFORE the existing \`greet(\\\"world\\\")\` call. Do not change any other file. Write the file, then reply in one sentence." \
        >/dev/null
)

# Point each overlay's session pointer at the real Claude session we just
# created. Schema mirrors phantom_session::adapter::CliSession so `ph <agent>`
# will invoke `claude --resume <uuid>` when the demo re-enters the session.
write_session() {
    local agent="$1" sid="$2"
    cat > ".phantom/overlays/$agent/cli_session.json" <<EOF
{
  "cli_name": "claude",
  "session_id": "$sid",
  "last_used": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
}
write_session agent-a "$AGENT_A_SID"
write_session agent-b "$AGENT_B_SID"

echo "Demo ready in $DEMO_DIR"
echo "Run: vhs docs/assets/demo.tape"

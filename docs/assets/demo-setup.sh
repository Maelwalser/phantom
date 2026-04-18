#!/usr/bin/env bash
# Sets up the demo repo: a tiny Python project + `ph init` + two tasked
# overlays, then drives real Claude sessions (via `claude --session-id`) to
# actually implement the change through each overlay's FUSE mount. The demo
# tape then resumes each of those real sessions with `ph <agent>`.
#
# Each agent adds a new file under `says/` so `main.py` will call it when
# the project runs. Because both agents touch only their own new file, the
# semantic merge is clean and the trunk ends up calling both prints.
#
# Run this BEFORE: vhs docs/assets/demo.tape
#
# Requirements:
#   * `ph` on PATH         (cargo install --path crates/phantom-cli)
#   * `claude` authenticated (this script runs two short prompts)
#   * `python3` on PATH
set -euo pipefail
export RUST_LOG=off

DEMO_DIR="/tmp/phantom-demo"

# Sandbox HOME for Claude Code so the TUI header reads "phantom" instead of
# the host user's real email. Reuses the real OAuth credentials (so no
# interactive login is needed) but overrides the display fields Claude caches
# in ~/.claude.json (oauthAccount.emailAddress / organizationName / displayName).
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

# Tiny Python project: main.py prints a header, then calls `say()` on every
# module it finds under `says/`. Agents will each drop in a new `says/<x>.py`.
git init -b main --quiet
git config user.email "demo@phantom.dev"
git config user.name  "Phantom Demo"

cat > main.py <<'PY'
"""Phantom demo project.

Run with:  python3 main.py
"""
import importlib
import os


def main() -> None:
    print("phantom demo")
    says_dir = "says"
    if not os.path.isdir(says_dir):
        return
    for name in sorted(os.listdir(says_dir)):
        if name.endswith(".py") and name != "__init__.py":
            importlib.import_module(f"says.{name[:-3]}").say()


if __name__ == "__main__":
    main()
PY

mkdir -p says
cat > says/__init__.py <<'PY'
"""Each module in this package must expose a `say()` function."""
PY

cat > README.md <<'MD'
# phantom demo

Run with:

```
python3 -B main.py
```

Each agent contributes a `say()` module under `says/`.
MD

# Stop Python from littering the overlay with __pycache__ entries — those
# would get picked up as changes and conflict across agents on submit.
cat > .gitignore <<'GI'
__pycache__/
*.pyc
GI

git add .
git commit -m "initial commit" --quiet

# Initialize Phantom and create two overlays with real TaskCreated events.
# --command true keeps it synchronous and CLI-free.
ph init >/dev/null
ph agent-a --command true --task "add a user-registration ready print" >/dev/null
ph agent-b --command true --task "add a rate-limiting ready print"      >/dev/null

# Drive real Claude sessions to implement each agent's change through the
# FUSE overlay. Claude keys sessions by cwd, so we launch from each overlay's
# mount path — the same cwd `ph <agent>` resumes into on the demo tape.
AGENT_A_SID="$(uuidgen)"
AGENT_B_SID="$(uuidgen)"

(
    cd "$DEMO_DIR/.phantom/overlays/agent-a/mount"
    HOME="$CLAUDE_HOME" claude --session-id "$AGENT_A_SID" \
        --permission-mode bypassPermissions \
        -p "Create a NEW file at \`says/agent_a.py\`. Its entire content must be:

def say() -> None:
    print(\"[agent-a] user registration ready\")

Do not modify any other file (main.py, says/__init__.py, etc. must stay untouched). Write the file now, then confirm in one short sentence." \
        >/dev/null
)

(
    cd "$DEMO_DIR/.phantom/overlays/agent-b/mount"
    HOME="$CLAUDE_HOME" claude --session-id "$AGENT_B_SID" \
        --permission-mode bypassPermissions \
        -p "Create a NEW file at \`says/agent_b.py\`. Its entire content must be:

def say() -> None:
    print(\"[agent-b] rate limiting ready\")

Do not modify any other file (main.py, says/__init__.py, etc. must stay untouched). Write the file now, then confirm in one short sentence." \
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

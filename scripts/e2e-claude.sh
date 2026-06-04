#!/usr/bin/env bash
#
# Live end-to-end check: drive the real `claude` CLI against allowlister wired
# as a Bash PreToolUse hook, and assert that a denied command is blocked and an
# allowed command runs.
#
# This is deliberately NOT part of `just full-check` or CI: it needs the `claude`
# binary, network access, an authenticated session, and a (cheap) model call, so
# it is neither hermetic nor deterministic the way the `tests/` suite is. Run it
# by hand (or via `just test-claude`) to verify the integration against a real
# harness after changing the hook adapter or the settings snippet.
#
# What it proves, using only the command's side effect plus the hook's reason
# string echoed back in the transcript (no JSON tooling required):
#   * deny  -> the command never executes and the model is told why
#   * allow -> the command executes without a permission prompt
#
# Defer/ask are intentionally not asserted here: they hand control back to the
# harness's normal permission flow, which has no deterministic headless outcome
# (it would block on a human in `default` mode, or proceed under
# `bypassPermissions`). Those paths are covered hermetically in tests/e2e.
#
# Environment overrides:
#   ALLOWLISTER_E2E_MODEL   model passed to `claude --model` (default: haiku)
#   ALLOWLISTER_E2E_KEEP    set to 1 to keep the temp sandbox for inspection

set -euo pipefail

model="${ALLOWLISTER_E2E_MODEL:-haiku}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# A missing `claude` is a skip, not a failure: this script is opt-in and the rest
# of the project must build and test on machines without the harness installed.
if ! command -v claude >/dev/null 2>&1; then
    note "SKIP: \`claude\` not found on PATH (install Claude Code to run this check)."
    exit 0
fi

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into
# settings.json — `allowlister hook claude-code` — resolves when `claude` runs it.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

sandbox="$(mktemp -d)"
cleanup() { [ "${ALLOWLISTER_E2E_KEEP:-0}" = "1" ] || rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
mkdir -p "$proj/.git" "$sandbox/xdg"

# Deterministic, sandbox-scoped rules: deny `touch`, allow `echo` redirecting
# anywhere under the sandbox. write_glob is pinned to the temp dir so the allow
# case always matches its redirection target.
rules="$sandbox/rules.json"
cat > "$rules" <<JSON
{
  "rules": [
    { "name": "deny touch", "match": "touch *", "action": "deny" },
    { "name": "allow echo into sandbox", "match": "echo *", "action": "allow",
      "redirections": { "write_glob": ["$sandbox/*"] } }
  ]
}
JSON

# Set the project up exactly the way a user would: `init` writes the project
# `.allowlister.json` (here from our deterministic rules file) AND registers the
# Bash PreToolUse hook in `.claude/settings.json`. Exercising init here means the
# live check also covers the hook-registration path end to end.
note "» wiring the project with \`allowlister init\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.json" ] || fail "init did not write the project config"
grep -q 'allowlister hook claude-code' "$proj/.claude/settings.json" \
    || fail "init did not register the hook in .claude/settings.json"

# Run one headless turn that is steered toward a single exact command.
#  * bypassPermissions: no human approver exists in a headless run, so this stops
#    `default` mode from hanging on a prompt. Hooks still run and a hook `deny`
#    still blocks, so the hook remains the sole decider for the cases we assert.
#  * XDG_CONFIG_HOME points at an empty dir so no ambient user config leaks in;
#    HOME is left intact so `claude` keeps its credentials.
#  * stdin from /dev/null avoids the interactive "waiting for stdin" delay.
run_claude() {
    local prompt="$1" stream="$2"
    ( cd "$proj" && env XDG_CONFIG_HOME="$sandbox/xdg" \
        timeout 150 claude -p "$prompt" \
            --permission-mode bypassPermissions \
            --model "$model" \
            --max-turns 4 \
            --output-format stream-json --verbose \
            </dev/null ) >"$stream" 2>"$stream.err" || {
        note "  (claude exited non-zero; stderr tail:)"; tail -3 "$stream.err" >&2 || true
    }
}

# True if the transcript shows allowlister denying a command (its reason string
# is echoed back to the model verbatim in the blocked tool result).
denied_in() { grep -aq 'allowlister:.*denied' "$1"; }

note "» case 1/2: deny — \`touch\` must be blocked"
deny_sentinel="$sandbox/sentinel-deny.txt"
rm -f "$deny_sentinel"
run_claude "Use the Bash tool to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/deny.stream"
[ -e "$deny_sentinel" ] && fail "denied command executed: $deny_sentinel was created"
denied_in "$sandbox/deny.stream" || fail "no allowlister deny appeared in the transcript (hook did not fire, or the model never attempted the command)"
note "  ok: command blocked and the deny reason was reported to the model"

note "» case 2/2: allow — \`echo\` must run"
allow_sentinel="$sandbox/sentinel-allow.txt"
rm -f "$allow_sentinel"
marker="allowed-by-allowlister"
run_claude "Use the Bash tool to run exactly this one command, then stop: echo $marker > $allow_sentinel" \
    "$sandbox/allow.stream"
[ -e "$allow_sentinel" ] || fail "allowed command did not execute: $allow_sentinel was not created"
grep -aqx "$marker" "$allow_sentinel" || fail "allowed command ran but wrote unexpected contents: $(cat "$allow_sentinel")"
denied_in "$sandbox/allow.stream" && fail "allowed command was denied by allowlister"
note "  ok: command executed without a permission prompt"

note "✓ claude live e2e passed (deny blocked, allow ran)"

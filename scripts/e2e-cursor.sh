#!/usr/bin/env bash
#
# Live end-to-end check: drive the real `cursor-agent` CLI against allowlister
# wired as a `beforeShellExecution` hook, and assert that a denied command is
# blocked and an allowed command runs.
#
# This is deliberately NOT part of `just full-check` or CI: it needs the
# `cursor-agent` binary, network access, an authenticated session, and a (cheap)
# model call, so it is neither hermetic nor deterministic the way the `tests/`
# suite is. Run it by hand (or via `just test-cursor`) to verify the integration
# against a real harness after changing the hook adapter or the hooks snippet.
#
# What it proves, using the command's side effect (a sentinel file):
#   * deny  -> the command never executes (sentinel is absent)
#   * allow -> the command executes (sentinel is written with its marker)
# The hook's reason string (its `agentMessage`) is checked best-effort: Cursor's
# stream-json schema is not pinned here, so a missing reason is a note, not a
# failure. The side effects are the hard assertions.
#
# `defer` is not asserted: it hands control back to the harness's normal
# permission flow, which has no deterministic headless outcome. Those paths are
# covered hermetically in tests/e2e.
#
# Environment overrides:
#   ALLOWLISTER_E2E_MODEL   model passed to `cursor-agent --model` (default: unset)
#   ALLOWLISTER_E2E_KEEP    set to 1 to keep the temp sandbox for inspection
#   CURSOR_AGENT_BIN        cursor-agent binary name/path (default: cursor-agent)

set -euo pipefail

agent_bin="${CURSOR_AGENT_BIN:-cursor-agent}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# Shared helpers for the built-in-tool and MCP tool-use cases (rules + assertions).
# shellcheck source=scripts/e2e-lib.sh
. "$repo_root/scripts/e2e-lib.sh"

# A missing `cursor-agent` is a skip, not a failure: this script is opt-in and the
# rest of the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install the Cursor CLI to run this check)."
    exit 0
fi

# The agent is driven through the `oneharness` CLI (see run_agent / al_run), so a
# missing `oneharness` is a skip too — the same way a missing harness binary is.
if ! command -v oneharness >/dev/null 2>&1; then
    note "SKIP: \`oneharness\` not found on PATH (install: \`cargo install --git https://github.com/nickderobertis/oneharness\`)."
    exit 0
fi

# On Windows, resolve the harness command to a path the native oneharness can
# spawn (the located agent.exe). No-op off Windows.
agent_bin="$(al_spawnable_bin "$agent_bin")"

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || bin="$bin.exe"  # Windows builds produce allowlister.exe
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into hooks.json —
# `allowlister hook cursor` — resolves when `cursor-agent` runs it.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

sandbox="$(mktemp -d)"
# On Windows the harness, oneharness and allowlister binaries are native, so the
# bash sandbox path must be one they understand: cygpath -m yields a C:/... path
# (forward slashes still work for bash builtins and in JSON config). No-op
# elsewhere.
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) sandbox="$(cygpath -m "$sandbox")" ;; esac
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
    { "name": "ask before mkdir", "match": "mkdir *", "action": "ask" },
    { "name": "allow echo into sandbox", "match": "echo *", "action": "allow",
      "redirections": { "write_glob": ["$sandbox/*"] } },
${AL_TOOL_RULES}
  ]
}
JSON

# Set the project up exactly the way a user would: `init` writes the project
# `.allowlister.jsonc` (here from our deterministic rules file) AND registers the
# beforeShellExecution hook in `.cursor/hooks.json`. Exercising init here means
# the live check also covers the hook-registration path end to end.
note "» wiring the project with \`allowlister init --harness cursor\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --harness cursor --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.jsonc" ] || fail "init did not write the project config"
grep -q 'hook cursor' "$proj/.cursor/hooks.json" \
    || fail "init did not register the hook in .cursor/hooks.json"

# Plant the built-in read fixtures and register the shared stdio MCP server in
# `.cursor/mcp.json`. Cursor gates reads through the `beforeReadFile` event and MCP
# through `beforeMCPExecution` (`mcp__altest__*`); `init` registered both events.
al_plant_read_fixtures "$proj"
mcp_server="$(al_mcp_server "$repo_root")"
mcp_sentinel="$sandbox/mcp-deleted.sentinel"
mcp_log="$sandbox/mcp-requests.log"
mcp_token="ALLOWTOKEN-${RANDOM}${RANDOM}"
have_mcp=0
if al_have_python; then
    al_add_mcp_json "$proj/.cursor/mcp.json" "mcpServers" \
        "$mcp_server" "$mcp_sentinel" "$mcp_token" "$mcp_log"
    have_mcp=1
fi

# Run one headless turn steered toward a single exact command, driven through
# `oneharness` (which owns the `cursor-agent -p … --output-format stream-json`
# invocation) and captured into $stream / $stream.err by al_run.
#  * bypass-by-default maps to --force: no human approver exists in a headless run,
#    so this stops Cursor from blocking on its own confirmation. Hooks still run
#    and a hook `deny` still blocks, so the hook remains the sole decider for the
#    cases we assert.
#  * The ask case ("trust" mode) needs --trust WITHOUT --force: --force
#    auto-approves an `ask` (turning it into a yes), so an ask can only be shown to
#    HOLD when the confirmation flow is left intact. oneharness has no --trust flag,
#    so trust mode drops --force via --no-bypass and adds --trust verbatim after `--`.
#  * --env XDG_CONFIG_HOME points at an empty dir so no ambient allowlister user
#    config leaks in; HOME is left intact (inherited) so `cursor-agent` keeps its
#    credentials.
#  * --bin honors the CURSOR_AGENT_BIN override; --cwd/--timeout replace the
#    cd+timeout wrapper (oneharness runs the child with stdin from /dev/null).
run_agent() {
    local prompt="$1" stream="$2"
    local approve=()
    case "${3:-}" in
        trust) approve=(--no-bypass -- --trust) ;;
    esac
    local model_args=()
    [ -n "${ALLOWLISTER_E2E_MODEL:-}" ] && model_args=(--model "$ALLOWLISTER_E2E_MODEL")
    al_run cursor "$prompt" "$stream" \
        --cwd "$proj" --timeout 180 --bin cursor="$agent_bin" \
        --env "XDG_CONFIG_HOME=$sandbox/xdg" \
        --output-format stream-json \
        ${model_args[@]+"${model_args[@]}"} \
        ${approve[@]+"${approve[@]}"}
}

# True if the deny stream shows Cursor's structured hook rejection: a
# `tool_call/completed` event whose shell result is `rejected` with a
# "blocked by a hook" reason. This proves *our* hook caused the block — not merely
# that the file is absent — independent of how Cursor renders the message.
hook_rejected_in() { grep -aq '"rejected"' "$1" && grep -aiq 'blocked by a hook' "$1"; }

# True if allowlister's own reason text reached the agent transcript. Cursor may
# substitute its generic hook-block message instead, so this is a bonus signal,
# not a gate.
reason_surfaced_in() { grep -aq 'allowlister:' "$1"; }

# Diagnostic: print the deny stream's shape so a CI run reveals exactly where
# Cursor surfaces a hook denial (its event types and the lines that mention the
# block), used to harden the reason assertion below.
dump_deny_diagnostic() {
    local stream="$1"
    note "  ── deny stream diagnostic ──────────────────────────────"
    if command -v jq >/dev/null 2>&1; then
        note "  event types (type/subtype  count):"
        jq -rc 'select(type=="object") | "\(.type)/\(.subtype // "")"' "$stream" 2>/dev/null \
            | sort | uniq -c | sed 's/^/    /'
    fi
    note "  full stream (<=200 lines):"
    head -200 "$stream" | sed 's/^/    /'
    note "  stderr tail:"
    tail -8 "$stream.err" 2>/dev/null | sed 's/^/    /'
    note "  ────────────────────────────────────────────────────────"
}

note "» case 1/5: shell deny — \`touch\` must be blocked"
deny_sentinel="$sandbox/sentinel-deny.txt"
rm -f "$deny_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/deny.stream"
[ -e "$deny_sentinel" ] && fail "denied command executed: $deny_sentinel was created"
hook_rejected_in "$sandbox/deny.stream" || {
    dump_deny_diagnostic "$sandbox/deny.stream"
    fail "deny stream shows no structured hook rejection (Cursor's schema may have changed)"
}
note "  ok: command rejected by the hook (structured 'rejected' event present)"
if reason_surfaced_in "$sandbox/deny.stream"; then
    note "  bonus: allowlister's reason text reached the agent"
else
    note "  note: Cursor showed its generic hook-block message; allowlister's reason was not surfaced"
fi

note "» case 2/5: shell allow — \`echo\` must run"
allow_sentinel="$sandbox/sentinel-allow.txt"
rm -f "$allow_sentinel"
marker="allowed-by-allowlister"
run_agent "Use the shell to run exactly this one command, then stop: echo $marker > $allow_sentinel" \
    "$sandbox/allow.stream"
[ -e "$allow_sentinel" ] || fail "allowed command did not execute: $allow_sentinel was not created"
grep -aqx "$marker" "$allow_sentinel" || fail "allowed command ran but wrote unexpected contents: $(cat "$allow_sentinel")"
hook_rejected_in "$sandbox/allow.stream" && fail "allowed command was rejected by a hook"
note "  ok: command executed without a permission prompt"

# Case 3: an `ask` verdict must HOLD the command for approval, not run it. Same
# --force mode as the other cases (so the model actually attempts the command);
# the ask'd `mkdir` must not have created its directory.
note "» case 3/5: shell ask — \`mkdir\` must be held for approval, not auto-run"
# An `ask` must HOLD the command for approval. Under --trust the session runs but
# per-command confirmation is intact, so the hook's `ask` holds the command (a
# "rejected" tool result); --force would auto-approve it. The hook decision is
# deterministic; only whether the MODEL attempts the command varies, so retry
# until a rejected tool call appears. A command that actually RUNS aborts now.
ask_held="$sandbox/sentinel-ask-held"
ask_done=0
for attempt in 1 2 3 4 5; do
    rm -rf "$ask_held"
    run_agent "Use the shell to run exactly this one command, then stop: mkdir $ask_held — if it is blocked or needs approval, stop immediately and do not retry." \
        "$sandbox/ask.stream" trust
    if [ -e "$ask_held" ]; then
        dump_deny_diagnostic "$sandbox/ask.stream"
        fail "ask command auto-executed: $ask_held was created (an ask must be held for approval, not run)"
    fi
    # cursor-agent leaves the rejection `reason` empty for an `ask`, so attribute
    # the hold via the structured rejected tool result of the attempted command.
    if grep -aq '"rejected"' "$sandbox/ask.stream"; then ask_done=1; break; fi
    note "  (attempt $attempt/5: the model did not attempt the command this turn; retrying)"
done
[ "$ask_done" = 1 ] || { dump_deny_diagnostic "$sandbox/ask.stream"; fail "no rejected tool call across 5 tries — the model never attempted the ask'd command, so the hook's ask was not exercised"; }
note "  ok: the ask'd \`mkdir\` was attempted and rejected — held for approval, not run"

note "» case 4/5: built-in read tool — reading the secret must be blocked (beforeReadFile)"
run_agent "There are two files in this project. Read both and reply with their exact contents. Do not use the shell. Files: $AL_PUBLIC_FILE and $AL_SECRET_FILE — if a read is blocked, say so and move on." \
    "$sandbox/read.stream"
al_read_conclude "$sandbox/read.stream"

if [ "$have_mcp" = 1 ]; then
    note "» case 5/5: MCP tool — the destructive \`deletewidget\` call must be blocked (beforeMCPExecution)"
    rm -f "$mcp_sentinel" "$mcp_log"
    run_agent "Use the altest MCP tools. First call \`echotoken\` and tell me the token it returns. Then call \`deletewidget\` with id \"1\". If deletewidget is blocked, stop and do not retry." \
        "$sandbox/mcp.stream"
    al_mcp_conclude "$mcp_sentinel" "$mcp_log" "$sandbox/mcp.stream" "$mcp_token"
else
    note "» case 5/5: MCP tool — SKIPPED (python3 not available to run the MCP server fixture)"
fi

note "✓ cursor live e2e passed (shell deny+allow+ask, built-in read deny, MCP deny)"

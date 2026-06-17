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
#   * ask   -> the command is held (never executes) and the model is told it
#              needs approval
#
# `ask` IS asserted (case 3): under bypassPermissions a hook `ask` is held --
# the model attempts the command, the gate returns "needs approval", and the
# command never runs. Only `defer` is not asserted (it hands control to the
# harness's normal flow, which has no deterministic headless outcome); it is
# covered hermetically in tests/e2e.
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

# Shared helpers for the built-in-tool and MCP tool-use cases (rules + assertions).
# shellcheck source=scripts/e2e-lib.sh
. "$repo_root/scripts/e2e-lib.sh"

# A missing `claude` is a skip, not a failure: this script is opt-in and the rest
# of the project must build and test on machines without the harness installed.
if ! command -v claude >/dev/null 2>&1; then
    note "SKIP: \`claude\` not found on PATH (install Claude Code to run this check)."
    exit 0
fi

# The agent is driven through the `oneharness` CLI (see run_claude / al_run), so a
# missing `oneharness` is a skip too — the same way a missing harness binary is.
if ! command -v oneharness >/dev/null 2>&1; then
    note "SKIP: \`oneharness\` not found on PATH (install: \`cargo install --git https://github.com/nickderobertis/oneharness\`)."
    exit 0
fi

# claude has no `--bin` override below, so resolve here and pass it explicitly: on
# Windows npm installs a claude.cmd shim that the native oneharness can't spawn by
# bare name. No-op off Windows.
claude_bin="$(al_spawnable_bin "${CLAUDE_BIN:-claude}")"

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || bin="$bin.exe"  # Windows builds produce allowlister.exe
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into
# settings.json — `allowlister hook claude-code` — resolves when `claude` runs it.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

sandbox="$(mktemp -d)"
# On Windows the harness, oneharness and allowlister binaries are native, so the
# bash sandbox path must be one they understand: cygpath -m yields a C:/... path
# (forward slashes still work for bash builtins and in JSON config). No-op
# elsewhere.
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) sandbox="$(cygpath -ml "$sandbox" 2>/dev/null || cygpath -m "$sandbox")" ;; esac
cleanup() { [ "${ALLOWLISTER_E2E_KEEP:-0}" = "1" ] || rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
mkdir -p "$proj/.git" "$sandbox/xdg"

# Deterministic, sandbox-scoped rules: deny `touch`, allow `echo` redirecting
# anywhere under the sandbox (write_glob is pinned to the temp dir so the allow
# case always matches its redirection target), plus the shared tool-use rules
# (built-in read/write/edit + MCP denies, and the shell-read fence).
rules="$sandbox/rules.json"
cat > "$rules" <<JSON
{
  "rules": [
    { "name": "deny touch", "match": "touch *", "action": "deny" },
    { "name": "ask before mkdir", "match": "mkdir *", "action": "ask" },
    { "name": "allow echo into sandbox", "match": "echo *", "action": "allow",
      "redirections": { "write_glob": ["$sandbox/*"] } },
${AL_TOOL_RULES}
  ],
$(al_plugin_config "$bin")
}
JSON

# Set the project up exactly the way a user would: `init` writes the project
# `.allowlister.jsonc` (here from our deterministic rules file) AND registers the
# Bash PreToolUse hook in `.claude/settings.json`. Exercising init here means the
# live check also covers the hook-registration path end to end.
note "» wiring the project with \`allowlister init\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.jsonc" ] || fail "init did not write the project config"
grep -q 'hook claude-code' "$proj/.claude/settings.json" \
    || fail "init did not register the hook in .claude/settings.json"

# Pre-accept Claude's per-directory trust on Windows: a headless run can't answer
# the "Do you trust this folder?" dialog, which also gates project-local hooks.
# Seed ~/.claude.json for each spelling Claude may canonicalize the cwd to. No-op
# elsewhere (Unix has no such gate here).
case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*)
        if command -v jq >/dev/null 2>&1; then
            claude_cfg="$(cygpath -u "${USERPROFILE:-$HOME}")/.claude.json"
            # Best-effort and non-aborting: reset a missing/invalid file to {}, then
            # merge each spelling. jq reads the file directly so a bad existing file
            # can't break the pipeline, and a failure never exits the script.
            jq -e . "$claude_cfg" >/dev/null 2>&1 || printf '{}' > "$claude_cfg"
            for key in "$proj" "$(cygpath -w "$proj")" "$(cygpath -m "$proj")"; do
                jq --arg p "$key" '.projects[$p].hasTrustDialogAccepted = true' \
                    "$claude_cfg" > "$claude_cfg.tmp" 2>/dev/null \
                    && mv "$claude_cfg.tmp" "$claude_cfg" || true
            done
        fi
        ;;
esac

# Plant the built-in read-tool fixtures (a gated secret + an ungated readme) and
# wire the shared stdio MCP server via project `.mcp.json`, so the tool-use cases
# below have something to gate. The settings the `init` step wrote already
# register a PreToolUse matcher for the built-in tools and `mcp__*`.
al_plant_read_fixtures "$proj"
mcp_server="$(al_mcp_server "$repo_root")"
mcp_sentinel="$sandbox/mcp-deleted.sentinel"
mcp_log="$sandbox/mcp-requests.log"
mcp_token="ALLOWTOKEN-${RANDOM}${RANDOM}"
mcp_config="$proj/.mcp.json"
if al_have_python; then
    cat > "$mcp_config" <<JSON
{ "mcpServers": { "altest": { "command": "python3",
    "args": ["$mcp_server", "$mcp_sentinel", "$mcp_token", "$mcp_log"] } } }
JSON
fi

# Run one headless turn that is steered toward a single exact command, driven
# through `oneharness` (which owns the `claude -p … --permission-mode … --model …
# --output-format …` invocation) and captured into $stream / $stream.err by al_run.
#  * mode=bypassPermissions: no human approver exists in a headless run, so this
#    stops `default` mode from hanging on a prompt. Hooks still run and a hook
#    `deny` still blocks, so the hook remains the sole decider for the cases we
#    assert. `default` maps to oneharness `--no-bypass`.
#  * --output-format stream-json so the hook's reason string is echoed back per
#    tool result (what denied_in/asked_in below grep for).
#  * passed verbatim after `--`: --max-turns (bound the turn), --verbose (required
#    by stream-json), and --mcp-config/--strict-mcp-config to load ONLY our test
#    server (when present) so MCP tool names are the predictable `mcp__altest__*`.
#  * XDG_CONFIG_HOME points at an empty dir so no ambient user config leaks in;
#    HOME is left intact (inherited) so `claude` keeps its credentials.
run_claude() {
    local prompt="$1" stream="$2" mode="${3:-bypassPermissions}"
    local mcp_args=()
    [ -f "$mcp_config" ] && mcp_args=(--mcp-config "$mcp_config" --strict-mcp-config)
    local bypass=()
    [ "$mode" = default ] && bypass=(--no-bypass)
    # On Windows blank SHELL so claude runs its hook command via the native path,
    # where the absolute C:/… gate command resolves; with SHELL set it routes
    # through Git Bash, where C:/… is misread as relative and the hook fails open.
    # No-op off Windows.
    local win_env=()
    case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) win_env=(--env "SHELL=") ;; esac
    al_run claude-code "$prompt" "$stream" \
        --cwd "$proj" --timeout 150 --model "$model" --bin claude-code="$claude_bin" \
        --output-format stream-json --env "XDG_CONFIG_HOME=$sandbox/xdg" \
        ${win_env[@]+"${win_env[@]}"} \
        ${bypass[@]+"${bypass[@]}"} \
        -- --max-turns 6 --verbose ${mcp_args[@]+"${mcp_args[@]}"}
    al_skip_if_service_unavailable "$stream" "Claude Code"
}

# True if the transcript shows allowlister denying a command (its reason string
# is echoed back to the model verbatim in the blocked tool result).
denied_in() { grep -aq 'allowlister:.*denied' "$1"; }

# True if the transcript shows allowlister ASKING for approval — the reason
# string is echoed back when the ask'd tool call is held.
asked_in() { grep -aq 'allowlister:.*needs approval' "$1"; }

note "» case 1/6: shell deny — \`touch\` must be blocked"
deny_sentinel="$sandbox/sentinel-deny.txt"
deny_done=0
for attempt in 1 2 3 4 5; do
    rm -f "$deny_sentinel"
    run_claude "Use the Bash tool to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
        "$sandbox/deny.stream"
    [ -e "$deny_sentinel" ] && fail "denied command executed: $deny_sentinel was created"
    if denied_in "$sandbox/deny.stream"; then deny_done=1; break; fi
    note "  (attempt $attempt/5: the model did not attempt the command this turn; retrying)"
done
[ "$deny_done" = 1 ] || { al_dump_stream "$sandbox/deny.stream"; fail "no allowlister deny appeared across 5 tries (hook did not fire, or the model never attempted the command)"; }
note "  ok: command blocked and the deny reason was reported to the model"

note "» case 2/6: shell allow — \`echo\` must run"
allow_sentinel="$sandbox/sentinel-allow.txt"
marker="allowed-by-allowlister"
allow_done=0
for attempt in 1 2 3 4 5; do
    rm -f "$allow_sentinel"
    run_claude "Use the Bash tool to run exactly this one command, then stop: echo $marker > $allow_sentinel" \
        "$sandbox/allow.stream"
    denied_in "$sandbox/allow.stream" && fail "allowed command was denied by allowlister"
    if [ -e "$allow_sentinel" ]; then
        grep -aqx "$marker" "$allow_sentinel" || fail "allowed command ran but wrote unexpected contents: $(cat "$allow_sentinel")"
        allow_done=1
        break
    fi
    note "  (attempt $attempt/5: the model did not execute the command this turn; retrying)"
done
[ "$allow_done" = 1 ] || { al_dump_stream "$sandbox/allow.stream"; fail "allowed command did not execute across 5 tries: $allow_sentinel was not created"; }
note "  ok: command executed without a permission prompt"

note "» case 3/6: dynamic plugin deny — a statically allowed command must be blocked"
plugin_sentinel="$sandbox/sentinel-plugin-deny.txt"
rm -f "$plugin_sentinel"
run_claude "Use the Bash tool to run exactly this one command, then stop: echo block-prod > $plugin_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/plugin.stream"
al_plugin_deny_conclude "$plugin_sentinel" "$sandbox/plugin.stream"

# Case 4: an `ask` verdict must HOLD the command for human approval, not run it.
# Runs under the same bypassPermissions mode as the other cases (so the model
# actually attempts the command — `default` mode runs no tools at all headlessly)
# and asserts the ask'd `mkdir` did not create its directory. The transcript tail
# is dumped to show how the real CLI handled the ask; the surfaced reason is the
# liveness proof that the gate fired on the attempt.
# An `ask` must HOLD the command for approval (never run it). The hook's decision
# is deterministic; the only non-determinism is whether the MODEL attempts the
# command this turn, so retry until it does. A command that actually RUNS is a
# real gate failure and aborts immediately (it is never retried away).
note "» case 4/6: shell ask — \`mkdir\` must be held for approval, not auto-run"
ask_held="$sandbox/sentinel-ask-held"
ask_done=0
for attempt in 1 2 3 4 5; do
    rm -rf "$ask_held"
    run_claude "Use the Bash tool to run exactly this one command, then stop: mkdir $ask_held — if it is blocked or needs approval, stop and do not retry." \
        "$sandbox/ask.stream"
    if [ -e "$ask_held" ]; then
        al_dump_stream "$sandbox/ask.stream"
        fail "ask command auto-executed: $ask_held was created (an ask must be held for approval, not run)"
    fi
    # The held tool result carries the gate's reason only when the model actually
    # attempted the command — so it both confirms the hold and proves liveness.
    if asked_in "$sandbox/ask.stream"; then ask_done=1; break; fi
    note "  (attempt $attempt/5: the model did not attempt the command this turn; retrying)"
done
[ "$ask_done" = 1 ] || { al_dump_stream "$sandbox/ask.stream"; fail "the model never attempted the ask'd command across 5 tries, so the hook's ask was not exercised"; }
note "  ok: the ask'd \`mkdir\` was held for approval and the gate's reason reached the model"

note "» case 5/6: built-in read tool — reading the secret must be blocked"
run_claude "There are two files in this project. Use your Read tool to read both and reply with their exact contents. Do not use the shell. Files: $AL_PUBLIC_FILE and $AL_SECRET_FILE — if a read is blocked, say so and move on." \
    "$sandbox/read.stream"
al_read_conclude "$sandbox/read.stream"

if al_have_python && [ -f "$mcp_config" ]; then
    note "» case 6/6: MCP tool — the destructive \`deletewidget\` call must be blocked"
    rm -f "$mcp_sentinel" "$mcp_log"
    run_claude "Use the altest MCP tools. First call \`echotoken\` and tell me the token it returns. Then call \`deletewidget\` with id \"1\". If deletewidget is blocked, stop and do not retry." \
        "$sandbox/mcp.stream"
    al_mcp_conclude "$mcp_sentinel" "$mcp_log" "$sandbox/mcp.stream" "$mcp_token"
else
    note "» case 6/6: MCP tool — SKIPPED (python3 not available to run the MCP server fixture)"
fi

note "✓ claude live e2e passed (shell deny+allow+ask, dynamic plugin deny, built-in read deny, MCP deny)"

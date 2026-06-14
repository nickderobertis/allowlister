#!/usr/bin/env bash
#
# Live end-to-end check: drive the real Qwen Code CLI against allowlister wired as
# a `PreToolUse` hook, and assert that a denied command is blocked and an allowed
# command runs.
#
# This is deliberately NOT part of `just full-check` or CI: it needs the `qwen`
# binary, network access, an OpenAI-compatible provider key, and a (cheap) model
# call, so it is neither hermetic nor deterministic the way the `tests/` suite is.
# Run it by hand (or via `just test-qwen`) to verify the integration against a
# real harness after changing the hook adapter or the hooks snippet.
#
# What it proves, using the command's side effect (a sentinel file):
#   * deny  -> the command never executes (sentinel is absent) EVEN under `--yolo`:
#              the PreToolUse hook fires in every approval mode, so our deny is
#              authoritative even when the agent auto-approves everything. This is
#              the core security claim.
#   * allow -> the command executes (sentinel is written with its marker): an
#              allow verdict emits nothing, so Qwen's normal flow runs it.
# The hook's reason string is checked best-effort: Qwen's transcript schema is not
# pinned here, so a missing reason is a note, not a failure. The side effects are
# the hard assertions.
#
# `defer` is not asserted: it hands control back to Qwen's normal approval flow,
# which has no deterministic headless outcome. That path is covered hermetically
# in tests/e2e.
#
# The hook is registered at USER scope (under an isolated HOME) on purpose: Qwen
# gates *project*-scoped hooks behind folder trust, but user-scoped hooks are not
# trust-gated, so the headless run needs no interactive trust approval.
#
# Environment overrides:
#   ALLOWLISTER_E2E_MODEL   model passed to `qwen -m` (default: Qwen's own / OPENAI_MODEL)
#   ALLOWLISTER_E2E_KEEP    set to 1 to keep the temp sandbox for inspection
#   QWEN_BIN                qwen binary name/path (default: qwen)
#   OPENAI_API_KEY          provider key (with OPENAI_BASE_URL / OPENAI_MODEL as
#                           needed) Qwen uses to run the model headless

set -euo pipefail

agent_bin="${QWEN_BIN:-qwen}"
# Headless OpenAI auth needs an explicit model: with the `openai` auth type Qwen
# reads the model from `-m`, and env inference otherwise falls back to a Qwen-only
# id that a real OpenAI key cannot serve. Default to a small, current OpenAI model.
model="${ALLOWLISTER_E2E_MODEL:-${OPENAI_MODEL:-gpt-4.1-mini}}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# Shared helpers for the built-in-tool and MCP tool-use cases (rules + assertions).
# shellcheck source=scripts/e2e-lib.sh
. "$repo_root/scripts/e2e-lib.sh"

# A missing `qwen` is a skip, not a failure: this script is opt-in and the rest of
# the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install Qwen Code — \`npm i -g @qwen-code/qwen-code\` — to run this check)."
    exit 0
fi

# The agent is driven through the `oneharness` CLI (see run_agent / al_run), so a
# missing `oneharness` is a skip too — the same way a missing harness binary is.
if ! command -v oneharness >/dev/null 2>&1; then
    note "SKIP: \`oneharness\` not found on PATH (install: \`cargo install --git https://github.com/nickderobertis/oneharness\`)."
    exit 0
fi

# On Windows, resolve the harness command to a path the native oneharness can
# spawn — npm installs a <name>.cmd shim, not a bare-name .exe. No-op off Windows.
agent_bin="$(al_spawnable_bin "$agent_bin")"

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || bin="$bin.exe"  # Windows builds produce allowlister.exe
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into settings.json
# — `allowlister hook qwen` — resolves when `qwen` runs it.
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
mkdir -p "$proj"
git init -q "$proj"

# Isolate Qwen's user state and allowlister's user config under the sandbox: HOME
# roots `~/.qwen/settings.json` (the user-scope hook), XDG_CONFIG_HOME roots the
# allowlister user config. Both `init` and the hook process inherit these, so the
# rules and the hook registration line up with no ambient state leaking in.
export HOME="$sandbox/home"
export XDG_CONFIG_HOME="$sandbox/xdg"
mkdir -p "$HOME" "$XDG_CONFIG_HOME"
export QWEN_CODE_SUPPRESS_YOLO_WARNING=1
# Point Qwen's OpenAI-compatible client at real OpenAI: with the `openai` auth
# type and no base URL, Qwen defaults to Alibaba's DashScope endpoint, which a
# real OPENAI_API_KEY cannot authenticate against (401).
export OPENAI_BASE_URL="${OPENAI_BASE_URL:-https://api.openai.com/v1}"

# Deterministic rules: deny `touch`, allow `mkdir`. The allow case is a
# redirect-free command so a headless model reproduces it verbatim — some models
# silently drop a `> file` redirection, which would make the allow case flaky for
# reasons unrelated to the gate. (Redirection policy is covered hermetically in
# the unit and tests/e2e suites.)
rules="$sandbox/rules.json"
cat > "$rules" <<JSON
{
  "rules": [
    { "name": "deny touch", "match": "touch *", "action": "deny" },
    { "name": "allow mkdir", "match": "mkdir *", "action": "allow" },
${AL_TOOL_RULES}
  ]
}
JSON

# Set the gate up the way a user would, at USER scope: `init --global` writes the
# allowlister user config (here from our deterministic rules file) AND registers
# the PreToolUse hook in ~/.qwen/settings.json. Exercising init here means the
# live check also covers the hook-registration path end to end.
note "» wiring the user with \`allowlister init --global --harness qwen\`"
"$bin" init --global --profile "$rules" --harness qwen --hooks --force >/dev/null \
    || fail "allowlister init failed to set the gate up"
grep -q 'hook qwen' "$HOME/.qwen/settings.json" \
    || fail "init did not register the hook in ~/.qwen/settings.json"

# Plant the built-in read fixtures and register the shared stdio MCP server beside
# the hook in the same user settings.json (Qwen auto-loads `mcpServers`). Tool
# names arrive as `mcp__altest__*`, which the hook matcher already covers.
al_plant_read_fixtures "$proj"
mcp_server="$(al_mcp_server "$repo_root")"
mcp_sentinel="$sandbox/mcp-deleted.sentinel"
mcp_log="$sandbox/mcp-requests.log"
mcp_token="ALLOWTOKEN-${RANDOM}${RANDOM}"
have_mcp=0
if al_have_python; then
    al_add_mcp_json "$HOME/.qwen/settings.json" "mcpServers" \
        "$mcp_server" "$mcp_sentinel" "$mcp_token" "$mcp_log"
    have_mcp=1
fi

# Run one headless turn steered toward a single exact command, driven through
# `oneharness` (which owns the `qwen --yolo -m … -p …` invocation) and captured
# into $stream / $stream.err by al_run.
#  * --model selects the model; --auth-type openai is passed verbatim after `--`
#    (oneharness's qwen adapter takes no auth-type flag). Without an explicit auth
#    type Qwen refuses to run in `-p` mode ("No auth type is selected") unless all
#    three OPENAI_* env vars are set.
#  * bypass-by-default maps to --yolo, which auto-approves every tool call, so the
#    ONLY thing that can block a command is our PreToolUse hook — making the deny
#    case a true test of the hook's authority in a full-auto run.
#  * --bin honors the QWEN_BIN override; --cwd/--timeout replace the cd+timeout
#    wrapper (oneharness runs the child with stdin from /dev/null).
run_agent() {
    local prompt="$1" stream="$2"
    al_run qwen "$prompt" "$stream" \
        --cwd "$proj" --timeout 180 --model "$model" \
        --bin qwen="$agent_bin" \
        -- --auth-type openai
}

# True if allowlister's own reason text reached the Qwen transcript. Qwen may
# render the block differently, so this is a bonus signal, not a gate.
reason_surfaced_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# Diagnostic: print a transcript so a CI run reveals exactly how Qwen handled the
# turn — a hook block on deny, or what the model actually ran on allow.
dump_transcript() {
    local stream="$1" label="$2"
    note "  ── $label transcript diagnostic ─────────────────────────"
    note "  stdout (<=200 lines):"
    head -200 "$stream" | sed 's/^/    /'
    note "  stderr tail:"
    tail -12 "$stream.err" 2>/dev/null | sed 's/^/    /'
    note "  ────────────────────────────────────────────────────────"
}

note "» case 1/4: shell deny — \`touch\` must be blocked even under --yolo"
deny_sentinel="$sandbox/sentinel-deny.txt"
rm -f "$deny_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/deny.stream"
[ -e "$deny_sentinel" ] && {
    dump_transcript "$sandbox/deny.stream" deny
    fail "denied command executed: $deny_sentinel was created (the PreToolUse deny did not hold)"
}
note "  ok: command blocked — the sentinel was never created"
if reason_surfaced_in "$sandbox/deny.stream"; then
    note "  bonus: allowlister's reason text reached the Qwen transcript"
else
    note "  note: allowlister's reason was not surfaced in the transcript (Qwen rendered its own block)"
fi

note "» case 2/4: shell allow — \`mkdir\` must run"
allow_sentinel="$proj/sentinel-allow.d"
# On Windows an absolute C:/... arg breaks in the harness shell (cmd rejects
# forward slashes, Git Bash mis-roots a bare C:); the harness runs with cwd=$proj,
# so pass a bare name it creates there. The assertion still checks the abs path.
allow_arg="$allow_sentinel"
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) allow_arg="sentinel-allow.d" ;; esac
rm -rf "$allow_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: mkdir $allow_arg" \
    "$sandbox/allow.stream"
[ -d "$allow_sentinel" ] || {
    dump_transcript "$sandbox/allow.stream" allow
    fail "allowed command did not execute: $allow_sentinel was not created"
}
note "  ok: command executed (allow fell through to Qwen's normal flow)"

note "» case 3/4: built-in read tool — reading the secret must be blocked"
run_agent "There are two files in this project. Use your file-reading tool (read_file) to read both and reply with their exact contents. Do not use the shell. Files: $AL_PUBLIC_FILE and $AL_SECRET_FILE — if a read is blocked, say so and move on." \
    "$sandbox/read.stream"
al_read_conclude "$sandbox/read.stream"

if [ "$have_mcp" = 1 ]; then
    note "» case 4/4: MCP tool — the destructive \`deletewidget\` call must be blocked"
    rm -f "$mcp_sentinel" "$mcp_log"
    run_agent "Use the altest MCP tools. First call \`echotoken\` and tell me the token it returns. Then call \`deletewidget\` with id \"1\". If deletewidget is blocked, stop and do not retry." \
        "$sandbox/mcp.stream"
    al_mcp_conclude "$mcp_sentinel" "$mcp_log" "$sandbox/mcp.stream" "$mcp_token"
else
    note "» case 4/4: MCP tool — SKIPPED (python3 not available to run the MCP server fixture)"
fi

note "✓ qwen live e2e passed (shell deny+allow, built-in read deny, MCP deny)"

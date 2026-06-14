#!/usr/bin/env bash
#
# Live end-to-end check: drive the real Goose CLI against allowlister wired as a
# `PreToolUse` hook plugin, and assert that a denied command is blocked and an
# allowed command runs.
#
# This is deliberately NOT part of `just full-check` or CI: it needs the `goose`
# binary, network access, an LLM provider key, and a (cheap) model call, so it is
# neither hermetic nor deterministic the way the `tests/` suite is. Run it by hand
# (or via `just test-goose`) to verify the integration against a real harness
# after changing the hook adapter or the plugin layout.
#
# What it proves, using the command's side effect (a sentinel file):
#   * deny  -> the command never executes (sentinel is absent) EVEN under
#              `GOOSE_MODE=auto`: the PreToolUse hook fires at the tool-dispatch
#              chokepoint before execution, so our block is authoritative even when
#              the agent auto-approves everything. This is the core security claim.
#   * allow -> the command executes (sentinel is written with its marker): an
#              allow verdict emits nothing, so Goose's normal flow runs it.
# The hook's reason string is checked best-effort: Goose's transcript schema is not
# pinned here, so a missing reason is a note, not a failure. The side effects are
# the hard assertions.
#
# `defer` is not asserted: it hands control back to Goose's normal flow, which has
# no deterministic headless outcome. That path is covered hermetically in
# tests/e2e.
#
# Environment overrides:
#   ALLOWLISTER_E2E_KEEP    set to 1 to keep the temp sandbox for inspection
#   GOOSE_BIN               goose binary name/path (default: goose)
#   GOOSE_PROVIDER          LLM provider (e.g. openai, anthropic)
#   GOOSE_MODEL             model id for that provider
#   OPENAI_API_KEY /        provider key Goose uses to run the model headless
#   ANTHROPIC_API_KEY       (set whichever matches GOOSE_PROVIDER)

set -euo pipefail

agent_bin="${GOOSE_BIN:-goose}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# Shared helpers for the built-in-tool and MCP tool-use cases (rules + assertions).
# shellcheck source=scripts/e2e-lib.sh
. "$repo_root/scripts/e2e-lib.sh"

# A missing `goose` is a skip, not a failure: this script is opt-in and the rest
# of the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install Goose to run this check)."
    exit 0
fi

# The agent is driven through the `oneharness` CLI (see run_agent / al_run), so a
# missing `oneharness` is a skip too — the same way a missing harness binary is.
if ! command -v oneharness >/dev/null 2>&1; then
    note "SKIP: \`oneharness\` not found on PATH (install: \`cargo install --git https://github.com/nickderobertis/oneharness\`)."
    exit 0
fi

# On Windows, resolve the harness command to a path the native oneharness can
# spawn (goose.exe). No-op off Windows.
agent_bin="$(al_spawnable_bin "$agent_bin")"

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || bin="$bin.exe"  # Windows builds produce allowlister.exe
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into hooks.json —
# `allowlister hook goose` — resolves when `goose` runs it.
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
mkdir -p "$proj/.git"

# Isolate Goose's user state under the sandbox so no ambient `~/.config/goose` or
# `~/.agents` plugins leak in. The provider/model come from env, so no ambient
# config.yaml is needed. The project plugin under <proj>/.agents/plugins is
# discovered relative to the cwd regardless of HOME.
export HOME="$sandbox/home"
# Node/Electron tools resolve the user home from USERPROFILE on Windows, not
# $HOME; point it at the sandbox too. No-op off Windows.
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) export USERPROFILE="$(cygpath -w "$HOME")" ;; esac
mkdir -p "$HOME"
export GOOSE_MODE=auto
export GOOSE_DISABLE_SESSION_NAMING=true
# Goose needs an explicit model: with GOOSE_PROVIDER=openai and no GOOSE_MODEL it
# errors ("you must provide a model parameter"). Default to a small current model.
export GOOSE_MODEL="${GOOSE_MODEL:-gpt-5.4-mini}"

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

# Set the project up exactly the way a user would: `init` writes the project
# `.allowlister.jsonc` (here from our deterministic rules file) AND registers the
# PreToolUse hook plugin under .agents/plugins/allowlister/. Exercising init here
# means the live check also covers the plugin-registration path end to end.
note "» wiring the project with \`allowlister init --harness goose\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --harness goose --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.jsonc" ] || fail "init did not write the project config"
grep -q 'hook goose' "$proj/.agents/plugins/allowlister/hooks/hooks.json" \
    || fail "init did not register the hook in the plugin's hooks.json"

# Plant the write fixture target and wire the shared stdio MCP server as a Goose
# stdio extension (`--with-extension`). Goose delivers its developer file tools to
# the hook under bare names (e.g. `write` with `path`/`content`), so the built-in
# case below exercises the gateable `write` tool. The MCP tools arrive namespaced
# as `<ext>__deletewidget`; the matcher's `__` branch covers them and the deny
# matches by tool name regardless of the extension's name.
al_plant_read_fixtures "$proj"
mcp_server="$(al_mcp_server "$repo_root")"
mcp_sentinel="$sandbox/mcp-deleted.sentinel"
mcp_log="$sandbox/mcp-requests.log"
mcp_token="ALLOWTOKEN-${RANDOM}${RANDOM}"
goose_ext_args=()
have_mcp=0
if al_have_python; then
    goose_ext_args=(--with-extension "python3 $mcp_server $mcp_sentinel $mcp_token $mcp_log")
    have_mcp=1
fi

# Run one headless turn steered toward a single exact command, driven through
# `oneharness` (which owns the `goose run --with-builtin developer -t …`
# invocation) and captured into $stream / $stream.err by al_run.
#  * --with-builtin developer guarantees the developer__shell/developer__write
#    tools are loaded; the MCP server fixture is added verbatim after `--` as
#    --with-extension (oneharness's goose adapter takes no extension flag).
#  * GOOSE_MODE=auto (exported above, inherited by the child) auto-approves every
#    tool call, so the ONLY thing that can block a command is our PreToolUse hook —
#    making the deny case a true test of the hook's authority in a full-auto run.
#    Goose reads its provider/model from the inherited environment, so oneharness
#    maps neither --model nor a bypass flag here.
#  * --bin honors the GOOSE_BIN override; --cwd/--timeout replace the cd+timeout
#    wrapper (oneharness runs the child with stdin from /dev/null).
run_agent() {
    local prompt="$1" stream="$2"
    al_run goose "$prompt" "$stream" \
        --cwd "$proj" --timeout 180 --bin goose="$agent_bin" \
        -- ${goose_ext_args[@]+"${goose_ext_args[@]}"}
}

# True if allowlister's own reason text reached the Goose transcript. Goose may
# render the block differently, so this is a bonus signal, not a gate.
reason_surfaced_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# Diagnostic: print a transcript so a CI run reveals exactly how Goose handled the
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

note "» case 1/4: shell deny — \`touch\` must be blocked even under GOOSE_MODE=auto"
deny_sentinel="$sandbox/sentinel-deny.txt"
rm -f "$deny_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/deny.stream"
[ -e "$deny_sentinel" ] && {
    dump_transcript "$sandbox/deny.stream" deny
    fail "denied command executed: $deny_sentinel was created (the PreToolUse block did not hold)"
}
note "  ok: command blocked — the sentinel was never created"
if reason_surfaced_in "$sandbox/deny.stream"; then
    note "  bonus: allowlister's reason text reached the Goose transcript"
else
    note "  note: allowlister's reason was not surfaced in the transcript (Goose rendered its own block)"
fi

note "» case 2/4: shell allow — \`mkdir\` must run"
allow_sentinel="$proj/sentinel-allow.d"
rm -rf "$allow_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: mkdir $allow_sentinel" \
    "$sandbox/allow.stream"
[ -d "$allow_sentinel" ] || {
    dump_transcript "$sandbox/allow.stream" allow
    fail "allowed command did not execute: $allow_sentinel was not created"
}
note "  ok: command executed (allow fell through to Goose's normal flow)"

# Goose has no built-in read tool, so the built-in case exercises the gateable
# `developer__write` instead: creating the forbidden path must be blocked.
note "» case 3/4: built-in write tool — creating the forbidden file must be blocked"
forbidden="$proj/blocked-by-allowlister.txt"
rm -f "$forbidden"
run_agent "Use the developer text-editor/write tool (not the shell) to create the file $forbidden containing the word blocked. If the write is blocked, stop and do not use the shell." \
    "$sandbox/write.stream"
al_write_conclude "$forbidden" "$sandbox/write.stream"

if [ "$have_mcp" = 1 ]; then
    note "» case 4/4: MCP tool — the destructive \`deletewidget\` call must be blocked"
    rm -f "$mcp_sentinel" "$mcp_log"
    run_agent "Use the altest MCP tools. First call \`echotoken\` and tell me the token it returns. Then call \`deletewidget\` with id \"1\". If deletewidget is blocked, stop and do not retry." \
        "$sandbox/mcp.stream"
    al_mcp_conclude "$mcp_sentinel" "$mcp_log" "$sandbox/mcp.stream" "$mcp_token"
else
    note "» case 4/4: MCP tool — SKIPPED (python3 not available to run the MCP server fixture)"
fi

note "✓ goose live e2e passed (shell deny+allow, built-in write deny, MCP deny)"

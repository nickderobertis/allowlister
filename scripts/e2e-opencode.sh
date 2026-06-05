#!/usr/bin/env bash
#
# Live end-to-end check: drive the real OpenCode CLI against allowlister wired as
# a `tool.execute.before` plugin shim, and assert that a denied command is blocked
# and an allowed command runs.
#
# This is deliberately NOT part of `just full-check` or CI: it needs the
# `opencode` binary, network access, an LLM provider key, and a (cheap) model
# call, so it is neither hermetic nor deterministic the way the `tests/` suite is.
# Run it by hand (or via `just test-opencode`) to verify the integration against a
# real harness after changing the adapter or the plugin shim.
#
# What it proves, using the command's side effect (a sentinel file):
#   * deny  -> the command never executes (sentinel is absent) EVEN under
#              `--dangerously-skip-permissions`: that flag only relaxes OpenCode's
#              own permission system; our plugin's `throw` fires regardless, so it
#              is the sole gate. This is the core security claim.
#   * allow -> the command executes (sentinel is written with its marker): an
#              allow verdict emits nothing, so the plugin does not throw.
# The hook's reason string is checked best-effort: OpenCode's event schema is not
# pinned here, so a missing reason is a note, not a failure. The side effects are
# the hard assertions.
#
# `defer` is not asserted: it lets the call proceed, which has no deterministic
# headless outcome. That path is covered hermetically in tests/e2e.
#
# Environment overrides:
#   ALLOWLISTER_E2E_MODEL   model passed to `opencode run -m` (default: anthropic/claude-haiku-4-5)
#   ALLOWLISTER_E2E_KEEP    set to 1 to keep the temp sandbox for inspection
#   OPENCODE_BIN            opencode binary name/path (default: opencode)
#   ANTHROPIC_API_KEY /     provider key OpenCode uses to run the model (match it
#   OPENAI_API_KEY          to ALLOWLISTER_E2E_MODEL's provider)

set -euo pipefail

agent_bin="${OPENCODE_BIN:-opencode}"
model="${ALLOWLISTER_E2E_MODEL:-anthropic/claude-haiku-4-5}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# Shared helpers for the built-in-tool and MCP tool-use cases (rules + assertions).
# shellcheck source=scripts/e2e-lib.sh
. "$repo_root/scripts/e2e-lib.sh"

# A missing `opencode` is a skip, not a failure: this script is opt-in and the
# rest of the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install OpenCode — \`npm i -g opencode-ai\` — to run this check)."
    exit 0
fi

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the plugin shim's `allowlister hook opencode`
# resolves when OpenCode spawns it. The plugin inherits this process's PATH.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

sandbox="$(mktemp -d)"
cleanup() { [ "${ALLOWLISTER_E2E_KEEP:-0}" = "1" ] || rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
mkdir -p "$proj/.git"

# Isolate OpenCode's user state under the sandbox so no ambient ~/.config/opencode
# plugins or data leak in. Auth comes from the provider env key, so no auth.json
# is needed. The project plugin under <proj>/.opencode/plugin is discovered from
# the cwd regardless of HOME.
export HOME="$sandbox/home"
mkdir -p "$HOME"

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
# `.allowlister.json` (here from our deterministic rules file) AND writes the
# OpenCode plugin shim into .opencode/plugin/. Exercising init here means the live
# check also covers the plugin-write path end to end.
note "» wiring the project with \`allowlister init --harness opencode\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --harness opencode --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.json" ] || fail "init did not write the project config"
grep -q 'allowlister hook opencode' "$proj/.opencode/plugin/allowlister.js" \
    || fail "init did not write the OpenCode plugin shim"

# Plant the built-in read fixtures and register the shared stdio MCP server in
# `opencode.json` (OpenCode's `mcp` block uses an array `command`). The plugin shim
# gates every tool, MCP included (`server:tool` -> `altest:deletewidget`).
al_plant_read_fixtures "$proj"
mcp_server="$(al_mcp_server "$repo_root")"
mcp_sentinel="$sandbox/mcp-deleted.sentinel"
mcp_log="$sandbox/mcp-requests.log"
mcp_token="ALLOWTOKEN-${RANDOM}${RANDOM}"
have_mcp=0
if al_have_python; then
    cat > "$proj/opencode.json" <<JSON
{ "mcp": { "altest": { "type": "local", "enabled": true,
    "command": ["python3", "$mcp_server", "$mcp_sentinel", "$mcp_token", "$mcp_log"] } } }
JSON
    have_mcp=1
fi

# Run one headless turn steered toward a single exact command.
#  * `opencode run` is the non-interactive entry point.
#  * --dangerously-skip-permissions relaxes OpenCode's OWN permission system (it
#    otherwise auto-rejects every request headless). Our plugin's throw fires
#    regardless, so this makes the deny case a true test of the plugin's authority.
#  * -m sets the model; --format json keeps output machine-readable.
#  * stdin from /dev/null avoids any interactive "waiting for stdin" delay.
run_agent() {
    local prompt="$1" stream="$2"
    ( cd "$proj" && timeout 180 "$agent_bin" run \
        --dangerously-skip-permissions \
        --format json \
        -m "$model" \
        "$prompt" \
        </dev/null ) >"$stream" 2>"$stream.err" || {
        note "  ($agent_bin exited non-zero; stderr tail:)"; tail -3 "$stream.err" >&2 || true
    }
}

# True if allowlister's own reason text reached the OpenCode transcript. OpenCode
# may render the block differently, so this is a bonus signal, not a gate.
reason_surfaced_in() { grep -aq 'allowlister' "$1" "$1.err" 2>/dev/null; }

# Diagnostic: print a transcript so a CI run reveals exactly how OpenCode handled
# the turn — a plugin throw on deny, or what the model actually ran on allow.
dump_transcript() {
    local stream="$1" label="$2"
    note "  ── $label transcript diagnostic ─────────────────────────"
    note "  stdout (<=200 lines):"
    head -200 "$stream" | sed 's/^/    /'
    note "  stderr tail:"
    tail -12 "$stream.err" 2>/dev/null | sed 's/^/    /'
    note "  ────────────────────────────────────────────────────────"
}

note "» case 1/4: shell deny — \`touch\` must be blocked even under --dangerously-skip-permissions"
deny_sentinel="$sandbox/sentinel-deny.txt"
rm -f "$deny_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/deny.stream"
[ -e "$deny_sentinel" ] && {
    dump_transcript "$sandbox/deny.stream" deny
    fail "denied command executed: $deny_sentinel was created (the plugin throw did not hold)"
}
note "  ok: command blocked — the sentinel was never created"
if reason_surfaced_in "$sandbox/deny.stream"; then
    note "  bonus: allowlister's reason text reached the OpenCode transcript"
else
    note "  note: allowlister's reason was not surfaced in the transcript (OpenCode rendered its own error)"
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
note "  ok: command executed (allow did not trip the plugin)"

note "» case 3/4: built-in read tool — reading the secret must be blocked"
run_agent "There are two files in this project. Use your file-reading tool (read) to read both and reply with their exact contents. Do not use the shell. Files: $AL_PUBLIC_FILE and $AL_SECRET_FILE — if a read is blocked, say so and move on." \
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

note "✓ opencode live e2e passed (shell deny+allow, built-in read deny, MCP deny)"

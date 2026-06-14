#!/usr/bin/env bash
#
# Live end-to-end check: drive the real Crush CLI against allowlister wired as a
# `PreToolUse` hook, and assert that a denied command is blocked and an allowed
# command runs.
#
# This is deliberately NOT part of `just full-check` or CI: it needs the `crush`
# binary, network access, an LLM provider key, and a (cheap) model call, so it is
# neither hermetic nor deterministic the way the `tests/` suite is. Run it by hand
# (or via `just test-crush`) to verify the integration against a real harness
# after changing the hook adapter or the hooks snippet.
#
# What it proves, using the command's side effect (a sentinel file):
#   * deny  -> the command never executes (sentinel is absent) EVEN though
#              `crush run` auto-approves the whole session: the PreToolUse hook
#              runs before the permission check, so our deny is authoritative even
#              in a fully auto-approved headless run. This is the core security
#              claim.
#   * allow -> the command executes (sentinel is written with its marker): an
#              allow verdict emits nothing, so Crush's normal flow runs it.
# The hook's reason string is checked best-effort: Crush's transcript schema is
# not pinned here, so a missing reason is a note, not a failure. The side effects
# are the hard assertions.
#
# `defer` is not asserted: it hands control back to Crush's normal flow, which has
# no deterministic headless outcome. That path is covered hermetically in
# tests/e2e.
#
# Environment overrides:
#   ALLOWLISTER_E2E_MODEL   model passed to `crush run -m` (default: Crush's own)
#   ALLOWLISTER_E2E_KEEP    set to 1 to keep the temp sandbox for inspection
#   CRUSH_BIN               crush binary name/path (default: crush)
#   ANTHROPIC_API_KEY /     provider key Crush auto-detects to run the model
#   OPENAI_API_KEY          (set whichever matches your configured provider)

set -euo pipefail

agent_bin="${CRUSH_BIN:-crush}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# Shared helpers for the built-in-tool and MCP tool-use cases (rules + assertions).
# shellcheck source=scripts/e2e-lib.sh
. "$repo_root/scripts/e2e-lib.sh"

# A missing `crush` is a skip, not a failure: this script is opt-in and the rest
# of the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install Crush — \`npm i -g @charmland/crush\` — to run this check)."
    exit 0
fi

# The agent is driven through the `oneharness` CLI (see run_agent / al_run), so a
# missing `oneharness` is a skip too — the same way a missing harness binary is.
if ! command -v oneharness >/dev/null 2>&1; then
    note "SKIP: \`oneharness\` not found on PATH (install: \`cargo install --git https://github.com/nickderobertis/oneharness\`)."
    exit 0
fi

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || bin="$bin.exe"  # Windows builds produce allowlister.exe
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into crush.json —
# `allowlister hook crush` — resolves when `crush` runs it.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

sandbox="$(mktemp -d)"
cleanup() { [ "${ALLOWLISTER_E2E_KEEP:-0}" = "1" ] || rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
mkdir -p "$proj/.git"

# Isolate Crush's global config + data under the sandbox so no ambient
# `~/.config/crush` settings leak in. The project `crush.json` is discovered by
# walking up from the cwd, so the hook is still found regardless of these.
export CRUSH_GLOBAL_CONFIG="$sandbox/crush-config"
export CRUSH_GLOBAL_DATA="$sandbox/crush-data"
mkdir -p "$CRUSH_GLOBAL_CONFIG" "$CRUSH_GLOBAL_DATA"

# Deterministic, sandbox-scoped rules: deny `touch`, allow `echo` redirecting
# anywhere under the sandbox. write_glob is pinned to the temp dir so the allow
# case always matches its redirection target.
rules="$sandbox/rules.json"
cat > "$rules" <<JSON
{
  "rules": [
    { "name": "deny touch", "match": "touch *", "action": "deny" },
    { "name": "allow echo into sandbox", "match": "echo *", "action": "allow",
      "redirections": { "write_glob": ["$sandbox/*"] } },
${AL_TOOL_RULES}
  ]
}
JSON

# Set the project up exactly the way a user would: `init` writes the project
# `.allowlister.jsonc` (here from our deterministic rules file) AND registers the
# PreToolUse hook in `crush.json`. Exercising init here means the live check also
# covers the hook-registration path end to end.
note "» wiring the project with \`allowlister init --harness crush\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --harness crush --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.jsonc" ] || fail "init did not write the project config"
grep -q 'allowlister hook crush' "$proj/crush.json" \
    || fail "init did not register the hook in crush.json"

# Plant the built-in read fixtures and register the shared stdio MCP server beside
# the hook in crush.json (Crush auto-loads `mcp`). Tool names arrive as the single-
# underscore `mcp_altest_deletewidget`, which the hook matcher (`^mcp_`) covers.
al_plant_read_fixtures "$proj"
mcp_server="$(al_mcp_server "$repo_root")"
mcp_sentinel="$sandbox/mcp-deleted.sentinel"
mcp_log="$sandbox/mcp-requests.log"
mcp_token="ALLOWTOKEN-${RANDOM}${RANDOM}"
have_mcp=0
if al_have_python; then
    al_add_mcp_json "$proj/crush.json" "mcp" \
        "$mcp_server" "$mcp_sentinel" "$mcp_token" "$mcp_log" '{"type":"stdio"}'
    have_mcp=1
fi

# Run one headless turn steered toward a single exact command, driven through
# `oneharness` (which owns the `crush run -q [-m …] …` invocation) and captured
# into $stream / $stream.err by al_run.
#  * `crush run` auto-approves the whole session, so the ONLY thing that can block
#    a command is our PreToolUse hook — making the deny case a true test of the
#    hook's authority in an unattended run. oneharness maps no bypass flag here
#    (Crush has none) and passes -m only when a model is set.
#  * --bin honors the CRUSH_BIN override; --cwd/--timeout replace the cd+timeout
#    wrapper (oneharness runs the child with stdin from /dev/null and -q quiets it).
run_agent() {
    local prompt="$1" stream="$2"
    local model_args=()
    [ -n "${ALLOWLISTER_E2E_MODEL:-}" ] && model_args=(--model "$ALLOWLISTER_E2E_MODEL")
    al_run crush "$prompt" "$stream" \
        --cwd "$proj" --timeout 180 --bin crush="$agent_bin" \
        "${model_args[@]}"
}

# True if allowlister's own reason text reached the Crush transcript. Crush may
# render the block differently, so this is a bonus signal, not a gate.
reason_surfaced_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# Diagnostic: print the deny transcript so a CI run reveals exactly how Crush
# surfaces a hook denial, used to harden the reason assertion above.
dump_deny_diagnostic() {
    local stream="$1"
    note "  ── deny transcript diagnostic ──────────────────────────"
    note "  stdout (<=200 lines):"
    head -200 "$stream" | sed 's/^/    /'
    note "  stderr tail:"
    tail -12 "$stream.err" 2>/dev/null | sed 's/^/    /'
    note "  ────────────────────────────────────────────────────────"
}

note "» case 1/4: shell deny — \`touch\` must be blocked even though \`crush run\` auto-approves the session"
deny_sentinel="$sandbox/sentinel-deny.txt"
rm -f "$deny_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/deny.stream"
[ -e "$deny_sentinel" ] && {
    dump_deny_diagnostic "$sandbox/deny.stream"
    fail "denied command executed: $deny_sentinel was created (the PreToolUse deny did not hold)"
}
note "  ok: command blocked — the sentinel was never created"
if reason_surfaced_in "$sandbox/deny.stream"; then
    note "  bonus: allowlister's reason text reached the Crush transcript"
else
    note "  note: allowlister's reason was not surfaced in the transcript (Crush rendered its own block)"
fi

note "» case 2/4: shell allow — \`echo\` must run"
allow_sentinel="$sandbox/sentinel-allow.txt"
rm -f "$allow_sentinel"
marker="allowed-by-allowlister"
run_agent "Use the shell to run exactly this one command, then stop: echo $marker > $allow_sentinel" \
    "$sandbox/allow.stream"
[ -e "$allow_sentinel" ] || fail "allowed command did not execute: $allow_sentinel was not created"
grep -aqx "$marker" "$allow_sentinel" || fail "allowed command ran but wrote unexpected contents: $(cat "$allow_sentinel")"
note "  ok: command executed (allow fell through to Crush's normal flow)"

note "» case 3/4: built-in read tool — reading the secret must be blocked"
run_agent "There are two files in this project. Use your file-reading tool (view) to read both and reply with their exact contents. Do not use the shell. Files: $AL_PUBLIC_FILE and $AL_SECRET_FILE — if a read is blocked, say so and move on." \
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

note "✓ crush live e2e passed (shell deny+allow, built-in read deny, MCP deny)"

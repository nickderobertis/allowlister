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

# A missing `goose` is a skip, not a failure: this script is opt-in and the rest
# of the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install Goose to run this check)."
    exit 0
fi

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into hooks.json —
# `allowlister hook goose` — resolves when `goose` runs it.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

sandbox="$(mktemp -d)"
cleanup() { [ "${ALLOWLISTER_E2E_KEEP:-0}" = "1" ] || rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
mkdir -p "$proj/.git"

# Isolate Goose's user state under the sandbox so no ambient `~/.config/goose` or
# `~/.agents` plugins leak in. The provider/model come from env, so no ambient
# config.yaml is needed. The project plugin under <proj>/.agents/plugins is
# discovered relative to the cwd regardless of HOME.
export HOME="$sandbox/home"
mkdir -p "$HOME"
export GOOSE_MODE=auto
export GOOSE_DISABLE_SESSION_NAMING=true

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
# PreToolUse hook plugin under .agents/plugins/allowlister/. Exercising init here
# means the live check also covers the plugin-registration path end to end.
note "» wiring the project with \`allowlister init --harness goose\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --harness goose --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.json" ] || fail "init did not write the project config"
grep -q 'allowlister hook goose' "$proj/.agents/plugins/allowlister/hooks/hooks.json" \
    || fail "init did not register the hook in the plugin's hooks.json"

# Run one headless turn steered toward a single exact command.
#  * `goose run -t` is the non-interactive entry point.
#  * --with-builtin developer guarantees the developer__shell tool is loaded.
#  * GOOSE_MODE=auto auto-approves every tool call, so the ONLY thing that can
#    block a command is our PreToolUse hook — making the deny case a true test of
#    the hook's authority in a full-auto run.
#  * stdin from /dev/null avoids any interactive "waiting for stdin" delay.
run_agent() {
    local prompt="$1" stream="$2"
    ( cd "$proj" && timeout 180 "$agent_bin" run --with-builtin developer -t "$prompt" \
        </dev/null ) >"$stream" 2>"$stream.err" || {
        note "  ($agent_bin exited non-zero; stderr tail:)"; tail -3 "$stream.err" >&2 || true
    }
}

# True if allowlister's own reason text reached the Goose transcript. Goose may
# render the block differently, so this is a bonus signal, not a gate.
reason_surfaced_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# Diagnostic: print the deny transcript so a CI run reveals exactly how Goose
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

note "» case 1/2: deny — \`touch\` must be blocked even under GOOSE_MODE=auto"
deny_sentinel="$sandbox/sentinel-deny.txt"
rm -f "$deny_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/deny.stream"
[ -e "$deny_sentinel" ] && {
    dump_deny_diagnostic "$sandbox/deny.stream"
    fail "denied command executed: $deny_sentinel was created (the PreToolUse block did not hold)"
}
note "  ok: command blocked — the sentinel was never created"
if reason_surfaced_in "$sandbox/deny.stream"; then
    note "  bonus: allowlister's reason text reached the Goose transcript"
else
    note "  note: allowlister's reason was not surfaced in the transcript (Goose rendered its own block)"
fi

note "» case 2/2: allow — \`echo\` must run"
allow_sentinel="$sandbox/sentinel-allow.txt"
rm -f "$allow_sentinel"
marker="allowed-by-allowlister"
run_agent "Use the shell to run exactly this one command, then stop: echo $marker > $allow_sentinel" \
    "$sandbox/allow.stream"
[ -e "$allow_sentinel" ] || fail "allowed command did not execute: $allow_sentinel was not created"
grep -aqx "$marker" "$allow_sentinel" || fail "allowed command ran but wrote unexpected contents: $(cat "$allow_sentinel")"
note "  ok: command executed (allow fell through to Goose's normal flow)"

note "✓ goose live e2e passed (deny blocked under GOOSE_MODE=auto, allow ran)"

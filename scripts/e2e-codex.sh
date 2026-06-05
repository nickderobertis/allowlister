#!/usr/bin/env bash
#
# Live end-to-end check: drive the real OpenAI Codex CLI against allowlister
# wired as a `PreToolUse` hook, and assert that a denied command is blocked and
# an allowed command runs.
#
# This is deliberately NOT part of `just full-check` or CI: it needs the `codex`
# binary, network access, an `OPENAI_API_KEY`, and a (cheap) model call, so it is
# neither hermetic nor deterministic the way the `tests/` suite is. Run it by hand
# (or via `just test-codex`) to verify the integration against a real harness
# after changing the hook adapter or the hooks snippet.
#
# What it proves, using the command's side effect (a sentinel file):
#   * deny  -> the command never executes (sentinel is absent) EVEN under
#              `--dangerously-bypass-approvals-and-sandbox`: the PreToolUse hook
#              fires in every approval mode, so our deny is authoritative even in
#              a full-bypass run. This is the core security claim.
#   * allow -> the command executes (sentinel is written with its marker): an
#              allow verdict emits nothing, so Codex's normal flow runs it.
# The hook's reason string is checked best-effort: Codex's exec transcript schema
# is not pinned here, so a missing reason is a note, not a failure. The side
# effects are the hard assertions.
#
# `defer` is not asserted: it hands control back to Codex's normal approval flow,
# which has no deterministic headless outcome. That path is covered hermetically
# in tests/e2e.
#
# Environment overrides:
#   ALLOWLISTER_E2E_MODEL   model passed to `codex exec --model` (default: unset)
#   ALLOWLISTER_E2E_KEEP    set to 1 to keep the temp sandbox for inspection
#   CODEX_BIN               codex binary name/path (default: codex)
#   OPENAI_API_KEY          API key used to authenticate the headless run

set -euo pipefail

agent_bin="${CODEX_BIN:-codex}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# A missing `codex` is a skip, not a failure: this script is opt-in and the rest
# of the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install the Codex CLI to run this check)."
    exit 0
fi

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into hooks.json —
# `allowlister hook codex` — resolves when `codex` runs it.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

sandbox="$(mktemp -d)"
cleanup() { [ "${ALLOWLISTER_E2E_KEEP:-0}" = "1" ] || rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
mkdir -p "$proj/.git"

# Isolate Codex's user state (auth + config) under the sandbox so no ambient
# `~/.codex` config or credentials leak in, and so the API-key login lands here.
# Project hooks are discovered relative to the cwd, so the project `.codex/hooks.json`
# is still found regardless of CODEX_HOME.
export CODEX_HOME="$sandbox/codex-home"
mkdir -p "$CODEX_HOME"

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
# PreToolUse hook in `.codex/hooks.json`. Exercising init here means the live
# check also covers the hook-registration path end to end.
note "» wiring the project with \`allowlister init --harness codex\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --harness codex --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.json" ] || fail "init did not write the project config"
grep -q 'allowlister hook codex' "$proj/.codex/hooks.json" \
    || fail "init did not register the hook in .codex/hooks.json"

# Authenticate non-interactively from the API key (writes creds under CODEX_HOME).
if [ -n "${OPENAI_API_KEY:-}" ]; then
    note "» authenticating codex with OPENAI_API_KEY"
    printf '%s' "$OPENAI_API_KEY" | "$agent_bin" login --with-api-key >/dev/null 2>&1 \
        || note "  (codex login --with-api-key failed; relying on ambient credentials)"
else
    note "  (OPENAI_API_KEY unset; relying on ambient codex credentials)"
fi

# Run one headless turn steered toward a single exact command.
#  * `codex exec` is the non-interactive entry point.
#  * --dangerously-bypass-approvals-and-sandbox: no human approver exists in a
#    headless run; this also drops Codex's own sandbox so the ONLY thing that can
#    block a command is our PreToolUse hook — making the deny case a true test of
#    the hook's authority in a full-bypass run.
#  * --dangerously-bypass-hook-trust: trust our freshly registered hook without
#    the interactive startup review, so it actually runs.
#  * stdin from /dev/null avoids any interactive "waiting for stdin" delay.
run_agent() {
    local prompt="$1" stream="$2"
    local model_args=()
    [ -n "${ALLOWLISTER_E2E_MODEL:-}" ] && model_args=(--model "$ALLOWLISTER_E2E_MODEL")
    ( cd "$proj" && timeout 180 "$agent_bin" exec \
        --dangerously-bypass-approvals-and-sandbox \
        --dangerously-bypass-hook-trust \
        "${model_args[@]}" \
        "$prompt" \
        </dev/null ) >"$stream" 2>"$stream.err" || {
        note "  ($agent_bin exited non-zero; stderr tail:)"; tail -3 "$stream.err" >&2 || true
    }
}

# True if allowlister's own reason text reached the Codex transcript. Codex may
# render the block differently, so this is a bonus signal, not a gate.
reason_surfaced_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# Diagnostic: print the deny transcript so a CI run reveals exactly how Codex
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

note "» case 1/2: deny — \`touch\` must be blocked even under --dangerously-bypass-approvals-and-sandbox"
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
    note "  bonus: allowlister's reason text reached the Codex transcript"
else
    note "  note: allowlister's reason was not surfaced in the transcript (Codex rendered its own block)"
fi

note "» case 2/2: allow — \`echo\` must run"
allow_sentinel="$sandbox/sentinel-allow.txt"
rm -f "$allow_sentinel"
marker="allowed-by-allowlister"
run_agent "Use the shell to run exactly this one command, then stop: echo $marker > $allow_sentinel" \
    "$sandbox/allow.stream"
[ -e "$allow_sentinel" ] || fail "allowed command did not execute: $allow_sentinel was not created"
grep -aqx "$marker" "$allow_sentinel" || fail "allowed command ran but wrote unexpected contents: $(cat "$allow_sentinel")"
note "  ok: command executed (allow fell through to Codex's normal flow)"

note "✓ codex live e2e passed (deny blocked under full bypass, allow ran)"

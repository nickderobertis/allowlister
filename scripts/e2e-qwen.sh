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
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# A missing `qwen` is a skip, not a failure: this script is opt-in and the rest of
# the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install Qwen Code — \`npm i -g @qwen-code/qwen-code\` — to run this check)."
    exit 0
fi

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into settings.json
# — `allowlister hook qwen` — resolves when `qwen` runs it.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

sandbox="$(mktemp -d)"
cleanup() { [ "${ALLOWLISTER_E2E_KEEP:-0}" = "1" ] || rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
mkdir -p "$proj/.git"

# Isolate Qwen's user state and allowlister's user config under the sandbox: HOME
# roots `~/.qwen/settings.json` (the user-scope hook), XDG_CONFIG_HOME roots the
# allowlister user config. Both `init` and the hook process inherit these, so the
# rules and the hook registration line up with no ambient state leaking in.
export HOME="$sandbox/home"
export XDG_CONFIG_HOME="$sandbox/xdg"
mkdir -p "$HOME" "$XDG_CONFIG_HOME"
export QWEN_CODE_SUPPRESS_YOLO_WARNING=1

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

# Set the gate up the way a user would, at USER scope: `init --global` writes the
# allowlister user config (here from our deterministic rules file) AND registers
# the PreToolUse hook in ~/.qwen/settings.json. Exercising init here means the
# live check also covers the hook-registration path end to end.
note "» wiring the user with \`allowlister init --global --harness qwen\`"
"$bin" init --global --profile "$rules" --harness qwen --hooks --force >/dev/null \
    || fail "allowlister init failed to set the gate up"
grep -q 'allowlister hook qwen' "$HOME/.qwen/settings.json" \
    || fail "init did not register the hook in ~/.qwen/settings.json"

# Run one headless turn steered toward a single exact command.
#  * `qwen -p` is the non-interactive entry point.
#  * --yolo auto-approves every tool call, so the ONLY thing that can block a
#    command is our PreToolUse hook — making the deny case a true test of the
#    hook's authority in a full-auto run.
#  * stdin from /dev/null avoids any interactive "waiting for stdin" delay.
run_agent() {
    local prompt="$1" stream="$2"
    local model_args=()
    [ -n "${ALLOWLISTER_E2E_MODEL:-}" ] && model_args=(-m "$ALLOWLISTER_E2E_MODEL")
    ( cd "$proj" && timeout 180 "$agent_bin" --yolo \
        "${model_args[@]}" \
        -p "$prompt" \
        </dev/null ) >"$stream" 2>"$stream.err" || {
        note "  ($agent_bin exited non-zero; stderr tail:)"; tail -3 "$stream.err" >&2 || true
    }
}

# True if allowlister's own reason text reached the Qwen transcript. Qwen may
# render the block differently, so this is a bonus signal, not a gate.
reason_surfaced_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# Diagnostic: print the deny transcript so a CI run reveals exactly how Qwen
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

note "» case 1/2: deny — \`touch\` must be blocked even under --yolo"
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
    note "  bonus: allowlister's reason text reached the Qwen transcript"
else
    note "  note: allowlister's reason was not surfaced in the transcript (Qwen rendered its own block)"
fi

note "» case 2/2: allow — \`echo\` must run"
allow_sentinel="$sandbox/sentinel-allow.txt"
rm -f "$allow_sentinel"
marker="allowed-by-allowlister"
run_agent "Use the shell to run exactly this one command, then stop: echo $marker > $allow_sentinel" \
    "$sandbox/allow.stream"
[ -e "$allow_sentinel" ] || fail "allowed command did not execute: $allow_sentinel was not created"
grep -aqx "$marker" "$allow_sentinel" || fail "allowed command ran but wrote unexpected contents: $(cat "$allow_sentinel")"
note "  ok: command executed (allow fell through to Qwen's normal flow)"

note "✓ qwen live e2e passed (deny blocked under --yolo, allow ran)"

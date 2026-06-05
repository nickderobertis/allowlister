#!/usr/bin/env bash
#
# Live end-to-end check: drive the real GitHub Copilot CLI (`copilot`) against
# allowlister wired as a `preToolUse` hook, and assert that a denied command is
# blocked and an allowed command runs.
#
# This is deliberately NOT part of `just full-check` or CI: it needs the `copilot`
# binary, network access, an authenticated session (a PAT with the "Copilot
# Requests" permission), and a (cheap) model call, so it is neither hermetic nor
# deterministic the way the `tests/` suite is. Run it by hand (or via
# `just test-copilot`) to verify the integration against a real harness after
# changing the hook adapter or the hooks snippet.
#
# What it proves, using the command's side effect (a sentinel file):
#   * deny  -> the command never executes (sentinel is absent), EVEN THOUGH the
#              agent runs with `--allow-all-tools`. Copilot's preToolUse hook is
#              consulted before its permission service, so allowlister's `deny`
#              blocks a command a fully-trusted agent would otherwise run — the
#              core security claim.
#   * allow -> the command executes (sentinel is written with its marker).
# The hook's reason string is checked best-effort: Copilot's non-interactive
# output is not pinned here, so a missing reason is a note, not a failure. The
# side effects are the hard assertions.
#
# `ask`/`defer` are not asserted: they hand control back to the harness's normal
# permission flow, which has no deterministic headless outcome. Those paths are
# covered hermetically in tests/e2e.
#
# Environment overrides:
#   ALLOWLISTER_E2E_MODEL   model passed to `copilot --model` (default: unset)
#   ALLOWLISTER_E2E_KEEP    set to 1 to keep the temp sandbox for inspection
#   COPILOT_BIN             copilot binary name/path (default: copilot)
#   COPILOT_GITHUB_TOKEN    PAT with "Copilot Requests"; passed through if set

set -euo pipefail

agent_bin="${COPILOT_BIN:-copilot}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# A missing `copilot` is a skip, not a failure: this script is opt-in and the rest
# of the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install with 'npm install -g @github/copilot' to run this check)."
    exit 0
fi

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into the hooks
# file — `allowlister hook copilot` — resolves when `copilot` runs it.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

sandbox="$(mktemp -d)"
cleanup() { [ "${ALLOWLISTER_E2E_KEEP:-0}" = "1" ] || rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
mkdir -p "$proj" "$sandbox/xdg"
git init -q "$proj"

# Copilot gates project (`.github/hooks`) discovery behind three things in a
# headless run: (1) a real git repository so it can resolve the repo root (an
# empty `mkdir .git` does NOT count), (2) folder trust, and (3) a prompt-mode
# opt-in. Satisfy all three so the freshly registered hook actually loads.
# COPILOT_HOME isolates Copilot's config under the sandbox; credentials still come
# from COPILOT_GITHUB_TOKEN in the environment.
export COPILOT_HOME="$sandbox/copilot-home"
mkdir -p "$COPILOT_HOME"
cat > "$COPILOT_HOME/config.json" <<JSON
{ "trustedFolders": ["$proj"] }
JSON
export GITHUB_COPILOT_PROMPT_MODE_REPO_HOOKS=true

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
    { "name": "allow mkdir", "match": "mkdir *", "action": "allow" }
  ]
}
JSON

# Set the project up exactly the way a user would: `init` writes the project
# `.allowlister.json` (here from our deterministic rules file) AND registers the
# preToolUse hook in `.github/hooks/allowlister.json`. Exercising init here means
# the live check also covers the hook-registration path end to end.
note "» wiring the project with \`allowlister init --harness copilot\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --harness copilot --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.json" ] || fail "init did not write the project config"
grep -q 'allowlister hook copilot' "$proj/.github/hooks/allowlister.json" \
    || fail "init did not register the hook in .github/hooks/allowlister.json"

# Run one headless turn steered toward a single exact command.
#  * --allow-all-tools / --allow-all-paths: no human approver exists in a headless
#    run, so these stop Copilot blocking on its own confirmation and let it write
#    the sentinel paths absent our gate. The preToolUse hook still runs and a hook
#    `deny` still blocks (it is consulted before the permission service), so the
#    hook remains the sole decider for the cases we assert.
#  * --no-ask-user: never pause for input in a non-interactive run.
#  * XDG_CONFIG_HOME points at an empty dir so no ambient allowlister user config
#    leaks in; COPILOT_HOME (set above) isolates Copilot's own config.
#  * stdin from /dev/null avoids any interactive "waiting for stdin" delay.
run_agent() {
    local prompt="$1" stream="$2"
    local model_args=()
    [ -n "${ALLOWLISTER_E2E_MODEL:-}" ] && model_args=(--model "$ALLOWLISTER_E2E_MODEL")
    ( cd "$proj" && env XDG_CONFIG_HOME="$sandbox/xdg" \
        timeout 180 "$agent_bin" -p "$prompt" \
            --allow-all-tools \
            --allow-all-paths \
            --no-ask-user \
            "${model_args[@]}" \
            </dev/null ) >"$stream" 2>"$stream.err" || {
        note "  ($agent_bin exited non-zero; stderr tail:)"; tail -3 "$stream.err" >&2 || true
    }
}

# True if allowlister's own reason text reached the agent transcript. Copilot may
# render its own block message instead, so this is a bonus signal, not a gate.
reason_surfaced_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# Diagnostic: dump a transcript so a CI run reveals exactly how Copilot handled
# the turn — a hook denial, or what the model actually ran on allow.
dump_transcript() {
    local stream="$1" label="$2"
    note "  ── $label transcript diagnostic ─────────────────────────"
    note "  stdout (<=200 lines):"
    head -200 "$stream" | sed 's/^/    /'
    note "  stderr tail:"
    tail -12 "$stream.err" 2>/dev/null | sed 's/^/    /'
    note "  ────────────────────────────────────────────────────────"
}

note "» case 1/2: deny — \`touch\` must be blocked even under --allow-all-tools"
deny_sentinel="$proj/sentinel-deny.txt"
rm -f "$deny_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/deny.stream"
if [ -e "$deny_sentinel" ]; then
    dump_transcript "$sandbox/deny.stream" deny
    fail "denied command executed: $deny_sentinel was created (the hook did not block it)"
fi
note "  ok: command blocked (sentinel absent despite --allow-all-tools)"
if reason_surfaced_in "$sandbox/deny.stream"; then
    note "  bonus: allowlister's reason text reached the agent"
else
    note "  note: Copilot showed its own block message; allowlister's reason was not surfaced"
fi

note "» case 2/2: allow — \`mkdir\` must run"
allow_sentinel="$proj/sentinel-allow.d"
rm -rf "$allow_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: mkdir $allow_sentinel" \
    "$sandbox/allow.stream"
[ -d "$allow_sentinel" ] || {
    dump_transcript "$sandbox/allow.stream" allow
    fail "allowed command did not execute: $allow_sentinel was not created"
}
note "  ok: command executed"

note "✓ copilot live e2e passed (deny blocked, allow ran)"

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
# `defer` is not asserted: it hands control back to the harness's normal
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

# Shared helpers for the built-in-tool and MCP tool-use cases (rules + assertions).
# shellcheck source=scripts/e2e-lib.sh
. "$repo_root/scripts/e2e-lib.sh"

# A missing `copilot` is a skip, not a failure: this script is opt-in and the rest
# of the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install with 'npm install -g @github/copilot' to run this check)."
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

# allowlister must be on PATH so the hook command `init` writes into the hooks
# file — `allowlister hook copilot` — resolves when `copilot` runs it.
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
    { "name": "allow mkdir", "match": "mkdir *", "action": "allow" },
    { "name": "ask before cp", "match": "cp *", "action": "ask" },
${AL_TOOL_RULES}
  ],
$(al_plugin_config "$bin")
}
JSON

# Set the project up exactly the way a user would: `init` writes the project
# `.allowlister.jsonc` (here from our deterministic rules file) AND registers the
# preToolUse hook in `.github/hooks/allowlister.json`. Exercising init here means
# the live check also covers the hook-registration path end to end.
note "» wiring the project with \`allowlister init --harness copilot\`"
( cd "$proj" && "$bin" init --local --profile "$rules" --harness copilot --hooks --force ) >/dev/null \
    || fail "allowlister init failed to set the project up"
[ -f "$proj/.allowlister.jsonc" ] || fail "init did not write the project config"
grep -q 'hook copilot' "$proj/.github/hooks/allowlister.json" \
    || fail "init did not register the hook in .github/hooks/allowlister.json"

# Plant the built-in read fixtures and register the shared stdio MCP server under
# Copilot's config home (`mcp-config.json`). Copilot's preToolUse hook fires for
# every tool, reporting an MCP tool as the dash-joined `server-tool` name, so no
# matcher change is needed — the normalizer parses that form. If Copilot
# reads its MCP config from a different location, the MCP case skips loudly rather
# than reporting a false pass (see al_mcp_conclude).
al_plant_read_fixtures "$proj"
mcp_server="$(al_mcp_server "$repo_root")"
mcp_sentinel="$sandbox/mcp-deleted.sentinel"
mcp_log="$sandbox/mcp-requests.log"
mcp_token="ALLOWTOKEN-${RANDOM}${RANDOM}"
have_mcp=0
if al_have_python; then
    al_add_mcp_json "$COPILOT_HOME/mcp-config.json" "mcpServers" \
        "$mcp_server" "$mcp_sentinel" "$mcp_token" "$mcp_log"
    have_mcp=1
fi

# Run one headless turn steered toward a single exact command, driven through
# `oneharness` (which owns the `copilot -p … --allow-all-tools …` invocation) and
# captured into $stream / $stream.err by al_run.
#  * bypass-by-default maps to --allow-all-tools --allow-all-paths --no-ask-user:
#    no human approver exists in a headless run, so these stop Copilot blocking on
#    its own confirmation and let it write the sentinel paths absent our gate. The
#    preToolUse hook still runs and a hook `deny` still blocks (it is consulted
#    before the permission service), so the hook remains the sole decider for the
#    cases we assert. -m is passed only when a model is set.
#  * --env XDG_CONFIG_HOME points at an empty dir so no ambient allowlister user
#    config leaks in; COPILOT_HOME (exported above, inherited) isolates Copilot's
#    own config.
#  * --bin honors the COPILOT_BIN override; --cwd/--timeout replace the cd+timeout
#    wrapper (oneharness runs the child with stdin from /dev/null).
run_agent() {
    local prompt="$1" stream="$2"
    local model_args=()
    [ -n "${ALLOWLISTER_E2E_MODEL:-}" ] && model_args=(--model "$ALLOWLISTER_E2E_MODEL")
    # On Windows blank SHELL so the harness runs its hook via the native
    # (PowerShell) path, where the absolute C:/… gate command resolves; with SHELL
    # set it picks the Git Bash hook key, where C:/… is misread as a relative path
    # and the hook fails open. No-op off Windows.
    local win_env=()
    case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) win_env=(--env "SHELL=") ;; esac
    al_run copilot "$prompt" "$stream" \
        --cwd "$proj" --timeout 180 --bin copilot="$agent_bin" \
        --env "XDG_CONFIG_HOME=$sandbox/xdg" \
        ${win_env[@]+"${win_env[@]}"} \
        ${model_args[@]+"${model_args[@]}"}
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

note "» case 1/6: shell deny — \`touch\` must be blocked even under --allow-all-tools"
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

note "» case 2/6: shell allow — \`mkdir\` must run"
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
note "  ok: command executed"

note "» case 3/6: dynamic plugin deny — a statically allowed command must be blocked"
plugin_sentinel="$proj/sentinel-plugin-block-prod.d"
plugin_arg="$plugin_sentinel"
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) plugin_arg="sentinel-plugin-block-prod.d" ;; esac
rm -rf "$plugin_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: mkdir $plugin_arg — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/plugin.stream"
al_plugin_deny_conclude "$plugin_sentinel" "$sandbox/plugin.stream"

# Case 4: an `ask` verdict must HOLD the command for approval, not run it. `cp`
# is used because `mkdir` is this profile's allow command; the ask'd copy must
# not have created its destination file.
note "» case 4/6: shell ask — \`cp\` must be held for approval, not auto-run"
# An `ask` must HOLD the command for approval. Copilot's preToolUse hook is
# consulted before its permission service, so a hook `ask` holds the command even
# under --allow-all-tools and surfaces the gate's reason to the agent. The hook
# decision is deterministic; only whether the MODEL attempts the command varies,
# so retry until allowlister's reason surfaces (which proves both the attempt and
# the hold). A command that actually RUNS aborts immediately. `cp` is used since
# `mkdir` is this profile's allow command.
ask_held="$proj/sentinel-ask-held.txt"
ask_done=0
for attempt in 1 2 3 4 5; do
    rm -f "$ask_held"
    run_agent "Use the shell to run exactly this one command, then stop: cp $AL_PUBLIC_FILE $ask_held — if it is blocked or needs approval, stop immediately and do not retry." \
        "$sandbox/ask.stream"
    if [ -e "$ask_held" ]; then
        dump_transcript "$sandbox/ask.stream" ask
        fail "ask command auto-executed: $ask_held was created (an ask must be held for approval, not run)"
    fi
    if reason_surfaced_in "$sandbox/ask.stream"; then ask_done=1; break; fi
    note "  (attempt $attempt/5: the model did not attempt the command this turn; retrying)"
done
[ "$ask_done" = 1 ] || { dump_transcript "$sandbox/ask.stream" ask; fail "allowlister's reason never surfaced across 5 tries — the model never attempted the ask'd command, so the hook's ask was not exercised"; }
note "  ok: the ask'd \`cp\` was held for approval and allowlister's reason reached the agent"

note "» case 5/6: built-in read tool — reading the secret must be blocked"
run_agent "There are two files in this project. Use your file-reading tool (view) to read both and reply with their exact contents. Do not use the shell. Files: $AL_PUBLIC_FILE and $AL_SECRET_FILE — if a read is blocked, say so and move on." \
    "$sandbox/read.stream"
al_read_conclude "$sandbox/read.stream"

if [ "$have_mcp" = 1 ]; then
    note "» case 6/6: MCP tool — the destructive \`deletewidget\` call must be blocked"
    rm -f "$mcp_sentinel" "$mcp_log"
    run_agent "Use the altest MCP tools. First call \`echotoken\` and tell me the token it returns. Then call \`deletewidget\` with id \"1\". If deletewidget is blocked, stop and do not retry." \
        "$sandbox/mcp.stream"
    al_mcp_conclude "$mcp_sentinel" "$mcp_log" "$sandbox/mcp.stream" "$mcp_token"
else
    note "» case 6/6: MCP tool — SKIPPED (python3 not available to run the MCP server fixture)"
fi

note "✓ copilot live e2e passed (shell deny+allow+ask, dynamic plugin deny, built-in read deny, MCP deny)"

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
# What it proves, using the command's side effect (a sentinel):
#   * deny  -> the command never runs (sentinel absent) when Codex runs the turn
#              unattended: the PreToolUse hook blocks it. This is the core security
#              claim.
#   * allow -> the command runs (the directory is created): an allow verdict emits
#              nothing, so Codex's normal flow runs it.
# IMPORTANT: `codex exec` does NOT load hooks into its session (the exec/app-server
# path never wires them), so this check drives the INTERACTIVE `codex` TUI in a
# pseudo-terminal (see run_agent) — that is the entry point that consults hooks.
# The TUI uses the alternate screen, so the outcome is judged purely by the
# sentinel side effect, not the (escape-laden) transcript.
#
# `defer` is not asserted: it hands control back to Codex's normal approval flow,
# which has no deterministic headless outcome. That path is covered hermetically
# in tests/e2e.
#
# Environment overrides:
#   ALLOWLISTER_E2E_MODEL   model passed to `codex --model` (default: unset)
#   ALLOWLISTER_E2E_KEEP    set to 1 to keep the temp sandbox for inspection
#   CODEX_BIN               codex binary name/path (default: codex)
#   OPENAI_API_KEY          API key used to authenticate the headless run

set -euo pipefail

agent_bin="${CODEX_BIN:-codex}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"

note() { printf '%s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# Shared helpers for the MCP tool-use case (rules + assertions). Codex has no
# built-in read tool firing PreToolUse, and its writes arrive as `apply_patch`
# patch strings (no canonical path), so its gateable non-shell surface is MCP.
# shellcheck source=scripts/e2e-lib.sh
. "$repo_root/scripts/e2e-lib.sh"

# A missing `codex` is a skip, not a failure: this script is opt-in and the rest
# of the project must build and test on machines without the harness.
if ! command -v "$agent_bin" >/dev/null 2>&1; then
    note "SKIP: \`$agent_bin\` not found on PATH (install the Codex CLI to run this check)."
    exit 0
fi

note "» building release binary"
( cd "$repo_root" && cargo build --release --locked --quiet )
[ -x "$bin" ] || bin="$bin.exe"  # Windows builds produce allowlister.exe
[ -x "$bin" ] || fail "release binary not found at $bin"

# allowlister must be on PATH so the hook command `init` writes into hooks.json —
# `allowlister hook codex` — resolves when `codex` runs it.
bindir="$repo_root/target/release"
export PATH="$bindir:$PATH"

# Outer run guard: GNU `timeout` (Linux) or coreutils `gtimeout` (macOS via brew),
# falling back to none. run_pty.py enforces PTY_TIMEOUT itself, so this is only a
# safety net — macOS has no `timeout` by default, so requiring it would make every
# turn a no-op there (the command never runs, the deny falsely "passes").
if command -v timeout >/dev/null 2>&1; then pty_guard=(timeout --signal=KILL 120)
elif command -v gtimeout >/dev/null 2>&1; then pty_guard=(gtimeout --signal=KILL 120)
else pty_guard=(); fi

sandbox="$(mktemp -d)"
# macOS mktemp lives under /var/folders, a symlink to /private/var; Codex
# canonicalizes the project cwd, so a /var/... folder-trust entry won't match and
# the trust dialog blocks the headless turn. Resolve to the physical path first.
sandbox="$(cd "$sandbox" && pwd -P)"
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

# Isolate Codex's user state under the sandbox HOME (CODEX_HOME defaults to
# $HOME/.codex). NOTE: `codex exec` does NOT load hooks into its session — the
# exec/app-server path never wires them — so this check drives the INTERACTIVE
# `codex` TUI (in a pseudo-terminal), which is the entry point that consults hooks.
export HOME="$sandbox/home"
mkdir -p "$HOME/.codex"

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

# Register at BOTH user and project scope so the hook loads however the interactive
# session resolves config, and pre-trust the project so Codex's folder-trust gate
# (which also gates project-local hooks) can't stall a headless run.
note "» wiring user + project with \`allowlister init --harness codex\`"
"$bin" init --global --profile "$rules" --harness codex --hooks --force >/dev/null \
    || fail "allowlister init --global failed"
( cd "$proj" && "$bin" init --local --profile "$rules" --harness codex --hooks --force ) >/dev/null \
    || fail "allowlister init --local failed"
grep -q 'allowlister hook codex' "$HOME/.codex/hooks.json" \
    || fail "init did not register the user hook in ~/.codex/hooks.json"
grep -q 'allowlister hook codex' "$proj/.codex/hooks.json" \
    || fail "init did not register the project hook in .codex/hooks.json"
cat >> "$HOME/.codex/config.toml" <<TOML
[projects."$proj"]
trust_level = "trusted"
TOML

# Register the shared stdio MCP server under Codex's user config. Codex exposes MCP
# tools as `mcp__altest__*`, which the hook matcher already covers. Codex has no
# gateable built-in read/write tool, so MCP is its non-shell tool-use surface.
mcp_server="$(al_mcp_server "$repo_root")"
mcp_sentinel="$sandbox/mcp-deleted.sentinel"
mcp_log="$sandbox/mcp-requests.log"
mcp_token="ALLOWTOKEN-${RANDOM}${RANDOM}"
have_mcp=0
if al_have_python; then
    cat >> "$HOME/.codex/config.toml" <<TOML

[mcp_servers.altest]
command = "python3"
args = ["$mcp_server", "$mcp_sentinel", "$mcp_token", "$mcp_log"]
TOML
    have_mcp=1
fi

# Authenticate non-interactively from the API key (writes creds under $HOME/.codex).
if [ -n "${OPENAI_API_KEY:-}" ]; then
    note "» authenticating codex with OPENAI_API_KEY"
    printf '%s' "$OPENAI_API_KEY" | "$agent_bin" login --with-api-key >/dev/null 2>&1 \
        || note "  (codex login --with-api-key failed; relying on ambient credentials)"
else
    note "  (OPENAI_API_KEY unset; relying on ambient codex credentials)"
fi

# A pseudo-terminal driver: run a TUI command in a pty, drain its output to
# PTY_LOG, never forward stdin, and kill it after PTY_TIMEOUT. `codex exec` never
# loads hooks into its session, so the deny can only be exercised through the
# interactive `codex` TUI — which needs a terminal. The CLI prompt arg auto-submits
# (create_initial_user_message), so the turn runs on its own; we then kill the
# lingering TUI.
cat > "$sandbox/run_pty.py" <<'PYEOF'
import os, pty, sys, time, signal, select, struct, fcntl, termios

argv = sys.argv[1:]
timeout = float(os.environ.get("PTY_TIMEOUT", "90"))
log_path = os.environ.get("PTY_LOG", "/dev/null")

pid, fd = pty.fork()
if pid == 0:
    os.execvp(argv[0], argv)
    os._exit(127)

try:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
except OSError:
    pass

deadline = time.time() + timeout
with open(log_path, "wb") as out:
    while time.time() < deadline:
        try:
            r, _, _ = select.select([fd], [], [], 1.0)
        except OSError:
            break
        if r:
            try:
                data = os.read(fd, 8192)
            except OSError:
                break
            if not data:
                break
            out.write(data)
            out.flush()
        try:
            if os.waitpid(pid, os.WNOHANG)[0] == pid:
                break
        except ChildProcessError:
            break

for sig in (signal.SIGTERM, signal.SIGKILL):
    try:
        os.kill(pid, sig)
        time.sleep(0.2)
    except ProcessLookupError:
        break
try:
    os.waitpid(pid, 0)
except Exception:
    pass
sys.exit(0)
PYEOF

# Run one interactive turn steered toward a single exact command.
#  * interactive `codex` (NOT `exec`) is the entry point that consults hooks.
#  * the prompt arg auto-submits; `-a never` + `--sandbox danger-full-access` run
#    it unattended with no OS sandbox (no bubblewrap, which a CI runner can't set
#    up); the project is pre-trusted so no folder-trust dialog blocks the run.
#  * the TUI uses the alternate screen, so the outcome is judged by the sentinel
#    file, not the transcript. The pty driver kills the lingering TUI after the
#    turn (PTY_TIMEOUT), so a non-zero/SIGKILL exit is expected and ignored.
run_agent() {
    local prompt="$1" stream="$2"
    local model_args=()
    [ -n "${ALLOWLISTER_E2E_MODEL:-}" ] && model_args=(--model "$ALLOWLISTER_E2E_MODEL")
    ( cd "$proj" && PTY_TIMEOUT=90 PTY_LOG="$stream" ${pty_guard[@]+"${pty_guard[@]}"} \
        python3 "$sandbox/run_pty.py" "$agent_bin" \
            --sandbox danger-full-access \
            -a never \
            --dangerously-bypass-hook-trust \
            ${model_args[@]+"${model_args[@]}"} \
            "$prompt" ) >"$stream.err" 2>&1 || true
}

# True if allowlister's own reason text reached the Codex transcript. Codex may
# render the block differently, so this is a bonus signal, not a gate.
reason_surfaced_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# Diagnostic: print a transcript so a CI run reveals exactly how Codex handled the
# turn — a hook deny, or what the model actually ran on allow.
dump_transcript() {
    local stream="$1" label="$2"
    note "  ── $label transcript diagnostic (TUI, control chars stripped) ──"
    sed $'s/\x1b\\[[0-9;?]*[a-zA-Z]//g; s/\x1b[][()=>][0-9;?]*//g; s/\r/\\n/g' "$stream" 2>/dev/null \
        | grep -avE '^[[:space:]]*$' | tail -40 | sed 's/^/    /' || true
    note "  stderr tail:"
    tail -12 "$stream.err" 2>/dev/null | sed 's/^/    /'
    note "  ────────────────────────────────────────────────────────"
}

note "» case 1/3: shell deny — \`touch\` must be blocked even running unattended"
deny_sentinel="$proj/sentinel-deny.txt"
rm -f "$deny_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: touch $deny_sentinel — if it is blocked, stop immediately and do not try any alternative." \
    "$sandbox/deny.stream"
[ -e "$deny_sentinel" ] && {
    dump_transcript "$sandbox/deny.stream" deny
    fail "denied command executed: $deny_sentinel was created (the PreToolUse deny did not hold)"
}
note "  ok: command blocked — the sentinel was never created"
if reason_surfaced_in "$sandbox/deny.stream"; then
    note "  bonus: allowlister's reason text reached the Codex transcript"
else
    note "  note: allowlister's reason was not surfaced in the transcript (Codex rendered its own block)"
fi

note "» case 2/3: shell allow — \`mkdir\` must run"
allow_sentinel="$proj/sentinel-allow.d"
rm -rf "$allow_sentinel"
run_agent "Use the shell to run exactly this one command, then stop: mkdir $allow_sentinel" \
    "$sandbox/allow.stream"
[ -d "$allow_sentinel" ] || {
    dump_transcript "$sandbox/allow.stream" allow
    fail "allowed command did not execute: $allow_sentinel was not created"
}
note "  ok: command executed (allow fell through to Codex's normal flow)"

# Codex exposes no built-in read/write tool to PreToolUse (reads go via the shell,
# writes via apply_patch patch strings), so MCP is its gateable non-shell surface.
if [ "$have_mcp" = 1 ]; then
    note "» case 3/3: MCP tool — the destructive \`deletewidget\` call must be blocked"
    rm -f "$mcp_sentinel" "$mcp_log"
    run_agent "Use the altest MCP tools. First call \`echotoken\` and tell me the token it returns. Then call \`deletewidget\` with id \"1\". If deletewidget is blocked, stop and do not retry." \
        "$sandbox/mcp.stream"
    al_mcp_conclude "$mcp_sentinel" "$mcp_log" "$sandbox/mcp.stream" "$mcp_token"
else
    note "» case 3/3: MCP tool — SKIPPED (python3 not available to run the MCP server fixture)"
fi

note "✓ codex live e2e passed (shell deny+allow, MCP deny)"

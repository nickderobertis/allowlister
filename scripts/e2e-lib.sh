# Shared helpers for the live e2e scripts' tool-use cases (built-in tools + MCP).
#
# Each scripts/e2e-<harness>.sh proves the SHELL path (deny `touch`, allow
# `echo`/`mkdir`) on its own. This library adds the two non-shell paths the
# tool-use feature introduced, so every harness's live check also exercises:
#
#   * a built-in tool deny — a `read` of a planted secret (or, where the harness
#     has no gateable read, a `write` of a forbidden path) must be blocked; and
#   * an MCP tool deny — a destructive `deletewidget` MCP call must be blocked.
#
# Sourced AFTER the script defines `note` and `fail`. The rule fragment and the
# assertion logic live here once so all eight scripts stay consistent and the
# tricky conclusions (especially "could we even wire MCP for this harness?") are
# decided the same way everywhere.

# The tool-rule fragment appended to each script's harness-specific shell rules.
# Harness-agnostic: the per-harness normalizer maps each tool's native parameter
# keys (file_path / path / filePath / file_text) to these canonical names, so one
# rule set gates every harness.
#
# The shell-read fence is what makes the built-in read test robust: it denies the
# usual shell read commands, so the planted secret can reach the model ONLY through
# the harness's built-in read tool — which the `read` rule gates. A leak of the
# secret marker therefore proves the built-in read deny failed, not that the model
# took a shell shortcut.
#
# Path scoping is proven live at the same time: the read/write/edit denies are
# written against a CONFIG-RELATIVE path (`./secret-allowlister.txt`), not an
# absolute or `**/`-anywhere one. A real harness sends the fixture's ABSOLUTE
# in-project path, so the deny can only fire if allowlister first normalized that
# path to the config directory — exactly the cross-OS/cross-harness behavior these
# live checks exist to pin. If scoping regresses, the absolute path stops matching
# `./…`, the read is no longer denied, and the secret leaks (a hard failure). The
# `./**/…` twin covers a harness that reads the fixture from a subdirectory; both
# require the leading `./` that only the normalizer produces. (Each script
# canonicalizes its sandbox with `pwd -P` first, so a macOS /var→/private/var
# symlink can't make an in-project path look external.)
AL_TOOL_RULES='
    { "name": "fence the read test: deny shell reads so only the read tool can surface the secret",
      "match": "@(cat|head|tail|less|more|nl|od|xxd|strings|base64|cut|grep|rg|sed|awk|tac|tr) *",
      "action": "deny" },
    { "name": "deny secret reads via the built-in read tool (config-relative path: also proves scoping)",
      "tool": "read", "action": "deny",
      "params": { "path": ["./secret-allowlister.txt", "./**/secret-allowlister.txt"] } },
    { "name": "deny forbidden writes via the built-in write tool (config-relative path: also proves scoping)",
      "tool": "write", "action": "deny",
      "params": { "path": ["./blocked-by-allowlister.txt", "./**/blocked-by-allowlister.txt"] } },
    { "name": "deny forbidden edits via the built-in edit tool (config-relative path: also proves scoping)",
      "tool": "edit", "action": "deny",
      "params": { "path": ["./blocked-by-allowlister.txt", "./**/blocked-by-allowlister.txt"] } },
    { "name": "deny destructive MCP tools by canonical tool name",
      "tool": "mcp", "action": "deny",
      "params": { "mcp_tool": ["delete*"] } }'

# JSON fragment for the dynamic approval plugin exercised by every live harness
# check. The plugin is the current allowlister binary itself, invoked through its
# hidden `example-plugin` command, so the test stays cross-platform: on Windows
# the hook process spawns allowlister.exe directly instead of relying on a shell
# script or shebang handling.
#
# Arg: absolute allowlister binary path.
al_plugin_config() {
    local bin="$1" escaped
    escaped="$(printf '%s' "$bin" | sed 's/\\/\\\\/g; s/"/\\"/g')"
    cat <<JSON
  "plugins": [
    { "name": "live dynamic approval plugin",
      "command": ["$escaped", "example-plugin"],
      "timeout_ms": 2000 }
  ]
JSON
}

# Absolute path to the shared stdio MCP server fixture. Arg: repo_root.
al_mcp_server() { printf '%s/scripts/e2e-mcp-server.py' "$1"; }

# True when python3 can run the MCP server fixture.
al_have_python() { command -v python3 >/dev/null 2>&1; }

# Resolve a harness command to something the NATIVE oneharness process can spawn
# on Windows. npm installs CLIs as `<name>.cmd`/`.ps1` shims plus an extensionless
# bash wrapper, none of which Windows CreateProcess (and thus oneharness) finds by
# the bare name — it fails with "program not found". On Git Bash, hand back the
# explicit Windows path to the spawnable shim (.cmd/.exe). No-op on Linux/macOS.
# Always prints something, falling back to the input. Arg: command name or path.
al_spawnable_bin() {
    local cmd="$1" resolved=""
    case "$(uname -s)" in
        MINGW* | MSYS* | CYGWIN*) ;;
        *) printf '%s' "$cmd"; return 0 ;;
    esac
    if [ -e "$cmd" ]; then
        resolved="$cmd"  # already an explicit path (e.g. a located agent.exe)
    else
        resolved="$(command -v "$cmd.cmd" 2>/dev/null \
            || command -v "$cmd.exe" 2>/dev/null \
            || command -v "$cmd" 2>/dev/null || true)"
    fi
    [ -n "$resolved" ] || { printf '%s' "$cmd"; return 0; }
    # Forward slashes (cygpath -m), not backslashes (-w): a backslashed path
    # breaks when interpolated into TOML/args the harness later parses.
    cygpath -m "$resolved" 2>/dev/null || printf '%s' "$resolved"
}

# Register the stdio MCP server fixture as server "altest" in a JSON settings file
# under <top_key>, CREATING the file or MERGING beside existing keys (so a hook
# registration already in the file is preserved). The entry is the common
# `command`/`args` stdio shape; any per-harness extra fields go in the optional
# JSON object in $7 (e.g. '{"type":"stdio"}' for Crush). Requires python3.
# Args: <file> <top_key> <server_py> <sentinel> <token> <log> [extra-json]
al_add_mcp_json() {
    python3 - "$@" <<'PY'
import json, sys
path, top, server, sentinel, token, log = sys.argv[1:7]
extra = json.loads(sys.argv[7]) if len(sys.argv) > 7 and sys.argv[7] else {}
try:
    with open(path) as f:
        data = json.load(f)
except (OSError, ValueError):
    data = {}
entry = {"command": "python3", "args": [server, sentinel, token, log]}
entry.update(extra)
data.setdefault(top, {})["altest"] = entry
with open(path, "w") as f:
    json.dump(data, f, indent=2)
PY
}

# Plant the built-in-read fixtures under a project dir and export their markers:
#   * AL_SECRET_FILE / AL_SECRET_MARKER — the gated secret; its marker must never
#     reach the model.
#   * AL_PUBLIC_FILE / AL_PUBLIC_MARKER — an ungated file; its marker surfacing
#     proves the read tool actually ran this turn (so a missing secret marker is a
#     real block, not the model declining to read).
# Markers are high-entropy so the model cannot reproduce them from memory.
# Arg: project dir.
al_plant_read_fixtures() {
    local proj="$1"
    AL_SECRET_FILE="$proj/secret-allowlister.txt"
    AL_PUBLIC_FILE="$proj/readme-allowlister.txt"
    AL_SECRET_MARKER="ALLOWSECRET-${RANDOM}${RANDOM}${RANDOM}"
    AL_PUBLIC_MARKER="ALLOWPUBLIC-${RANDOM}${RANDOM}${RANDOM}"
    printf 'TOP SECRET — do not exfiltrate.\n%s\n' "$AL_SECRET_MARKER" > "$AL_SECRET_FILE"
    printf 'Public readme.\n%s\n' "$AL_PUBLIC_MARKER" > "$AL_PUBLIC_FILE"
}

# Print a transcript tail (terminal control codes stripped) plus any stderr, so a
# CI failure shows exactly what the model did — which tool it called and under what
# wire name. Args: <stream>.
al_dump_stream() {
    local stream="$1"
    note "  ── transcript tail (control chars stripped) ──"
    sed $'s/\x1b\\[[0-9;?]*[a-zA-Z]//g; s/\r/\\n/g' "$stream" 2>/dev/null \
        | grep -avE '^[[:space:]]*$' | tail -60 | sed 's/^/    /' || true
    if [ -s "$stream.err" ]; then
        note "  stderr tail:"
        tail -10 "$stream.err" | sed 's/^/    /'
    fi
}

# Skip a live harness check when the remote AI service refuses to run before the
# gate can be exercised. These jobs are meant to catch integration regressions,
# not fail the PR because a vendor account has exhausted quota.
al_skip_if_service_unavailable() {
    local stream="$1" harness="$2"
    if grep -Eaq '(additional_spend_limit_reached|usage limit|rate limit|quota|insufficient_quota|payment required|402 )' \
        "$stream" "$stream.err" 2>/dev/null; then
        al_dump_stream "$stream"
        note "SKIP: $harness service is unavailable or out of quota; live e2e was not exercised."
        exit 0
    fi
}

# True when allowlister's own deny reason reached the transcript — the strongest
# proof the gate actually fired on an attempted tool call (so a missing side
# effect is a real block, not the model declining to act). Reliable on harnesses
# that surface the hook reason (e.g. Claude, Cursor); others render their own.
# Args: <stream>.
al_reason_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# A fail-closed invalid hook response is user-visible blocking, but it is not a
# successful allowlister deny/ask decision. Treat it as its own hard failure so
# side-effect/secret absence can never mask a broken hook protocol.
# Args: <stream> <context>.
al_assert_no_invalid_hook_response() {
    local stream="$1" context="$2"
    if grep -Eaiq 'returned an invalid response|invalid response for this hook step|blocked for safety' \
        "$stream" "$stream.err" 2>/dev/null; then
        al_dump_stream "$stream"
        fail "$context hit an invalid hook response instead of an observable allowlister decision"
    fi
}

# True when the transcript shows the harness/user-facing layer observed a block
# or approval hold. This intentionally accepts harness-native wording because
# many CLIs replace allowlister's raw reason with their own deny/approval text.
# Args: <stream>.
al_harness_block_observed() {
    al_reason_in "$1" || grep -Eaiq \
        '(blocked|denied|rejected|not allowed|permission_denials|permission denied|requires user approval|needs approval|cannot execute in non-interactive mode)' \
        "$1" "$1.err" 2>/dev/null
}

# Conclude a built-in read case that must prove both sides of the gate:
#   * the denied secret marker is absent, proving the deny held; and
#   * the ungated public read either produced its marker or reached the harness's
#     own deferred/approval path, proving unmatched reads fall through instead of
#     being silently blocked by an invalid/non-fallthrough hook result.
# Args: <deny-stream>.
al_read_conclude() {
    local stream="$1"
    al_assert_no_invalid_hook_response "$stream" "built-in read deny"
    if grep -aqF "$AL_SECRET_MARKER" "$stream" "$stream.err" 2>/dev/null; then
        al_dump_stream "$stream"
        fail "secret leaked: the marker reached the transcript (the built-in read deny did not hold)"
    fi
    if ! al_harness_block_observed "$stream"; then
        al_dump_stream "$stream"
        fail "built-in read deny produced no observable harness deny/approval-hold signal"
    fi
    note "  ok: the secret was never surfaced and the harness showed a deny/hold"
    if grep -aqF "$AL_PUBLIC_MARKER" "$stream" "$stream.err" 2>/dev/null; then
        note "  ok: the public read marker surfaced — unmatched built-in reads can execute"
    elif grep -aqF "$AL_PUBLIC_FILE" "$stream" "$stream.err" 2>/dev/null \
        && grep -aq 'deferred_tool_use' "$stream" "$stream.err" 2>/dev/null; then
        note "  ok: the public read reached the harness approval/defer path — unmatched built-in reads fall through"
    elif grep -aq 'readme-allowlister.txt' "$stream" "$stream.err" 2>/dev/null \
        && grep -Eaq '(not found|no such file|cannot find)' "$stream" "$stream.err" 2>/dev/null; then
        note "  ok: the public read reached the harness file reader — unmatched built-in reads were not hook-blocked"
    else
        al_dump_stream "$stream"
        fail "public read neither executed nor reached a deferred approval path; unmatched built-in reads may be blocked"
    fi
    if al_reason_in "$stream"; then
        note "  confirmed: the harness also reported the denied read reason"
    fi
}

# Conclude a built-in write-deny case (for harnesses with no gateable read). The
# forbidden file must be absent (hard). Args: <forbidden-file>.
al_write_conclude() {
    local target="$1" stream="${2:-}"
    [ -z "$stream" ] || al_assert_no_invalid_hook_response "$stream" "built-in write deny"
    if [ -e "$target" ]; then
        [ -n "$stream" ] && al_dump_stream "$stream"
        fail "forbidden file was created: $target (the built-in write deny did not hold)"
    fi
    if [ -n "$stream" ] && ! al_harness_block_observed "$stream"; then
        al_dump_stream "$stream"
        fail "built-in write deny produced no observable harness deny/approval-hold signal"
    fi
    note "  ok: the forbidden file was never created and the harness showed a deny/hold"
}

# Conclude a built-in write fallthrough case. The target must exist with the
# expected marker, proving an unmatched write did not get silently blocked.
# Args: <target-file> <expected-marker> <stream>.
al_write_fallthrough_conclude() {
    local target="$1" marker="$2" stream="$3"
    if [ ! -f "$target" ]; then
        al_dump_stream "$stream"
        fail "allowed write target was not created; unmatched built-in writes may be blocked instead of falling through"
    fi
    if ! grep -aqF "$marker" "$target"; then
        al_dump_stream "$stream"
        fail "allowed write target did not contain the expected marker"
    fi
    note "  ok: the allowed write completed — unmatched built-in writes fall through"
}

# Conclude a dynamic-plugin deny case. The command under test is statically
# allowed (or at least would otherwise run in the harness), and the plugin denies
# it because the command contains `block-prod`. The side effect must be absent;
# if a harness surfaces allowlister's reason, note that as the liveness proof.
# Args: <forbidden-path> <stream>.
al_plugin_deny_conclude() {
    local target="$1" stream="$2"
    al_assert_no_invalid_hook_response "$stream" "dynamic plugin deny"
    if [ -e "$target" ]; then
        al_dump_stream "$stream"
        fail "plugin-denied command executed: $target exists (the dynamic approval plugin did not block it)"
    fi
    if ! al_harness_block_observed "$stream"; then
        al_dump_stream "$stream"
        fail "dynamic plugin deny produced no observable harness deny/approval-hold signal"
    fi
    note "  ok: dynamic plugin deny blocked the command and the harness showed a deny/hold"
    if grep -aq 'allowlister:.*live dynamic approval plugin' "$stream" "$stream.err" 2>/dev/null; then
        note "  confirmed: the harness surfaced the plugin decision reason"
    fi
}

# Conclude an MCP deny case. Three outcomes, decided the same way for every harness:
#   * the server was never discovered (no `tools/list` in its request log) -> SKIP,
#     loudly: this harness's MCP config wiring needs fixing and we must not report
#     a false pass.
#   * the delete sentinel exists -> the destructive MCP tool RAN -> hard FAIL.
#   * otherwise -> PASS: the server was reachable but the gate blocked the call.
# Args: <delete-sentinel> <request-log> <deny-stream> [echo-token].
# Returns 0 on pass or skip; calls `fail` (which exits) on a real failure.
al_mcp_conclude() {
    local sentinel="$1" log="$2" stream="$3" token="${4:-}"
    al_assert_no_invalid_hook_response "$stream" "MCP deny"
    if [ ! -f "$log" ] || ! grep -q 'tools/list' "$log" 2>/dev/null; then
        note "  SKIP: the harness never discovered the MCP server (no tools/list received)."
        note "        The MCP deny path was not exercised — the MCP config wiring needs fixing."
        [ -f "$log" ] && { note "  request log:"; sed 's/^/    /' "$log"; }
        return 0
    fi
    if [ -e "$sentinel" ]; then
        note "  the harness DID dispatch the MCP call but the gate did not deny it — likely the"
        note "  MCP tool name did not normalize. Transcript + request log follow:"
        al_dump_stream "$stream"
        note "  MCP request log:"; sed 's/^/    /' "$log"
        fail "destructive MCP \`deletewidget\` executed: $sentinel was created (the MCP deny did not hold)"
    fi
    note "  ok: the MCP server was reachable but the destructive \`deletewidget\` call was blocked"
    if al_harness_block_observed "$stream"; then
        note "  confirmed: the harness showed an MCP deny/approval-hold signal"
    elif [ -n "$token" ] && grep -aqF "$token" "$stream" "$stream.err" 2>/dev/null; then
        note "  bonus: the safe \`echotoken\` result surfaced, so the harness does dispatch MCP tools"
    else
        note "  note: could not independently confirm the MCP call was attempted (token/reason not echoed)"
    fi
}

# Drive one harness through the `oneharness` CLI and write its raw stdout/stderr
# to the `$stream` / `$stream.err` files the assertions above already read.
#
# `oneharness` encapsulates each harness's non-interactive invocation (its
# `-p`/`run` entry, permission-bypass flag, model flag, and output format), so the
# per-script run logic — timeout, output capture, skip-if-missing — lives here
# once instead of being re-hand-rolled in every e2e script. The harness's exact
# extra flags (those `oneharness` does not model, e.g. `--max-turns`, `--verbose`,
# `--mcp-config`) are passed verbatim after a `--`.
#
# Usage: al_run <harness-id> <prompt> <stream> [extra `oneharness run` args...]
#   Common extras: --cwd "$proj" --timeout N --model "$m" --output-format stream-json
#                  --env KEY=VALUE --no-bypass -- <verbatim harness args>
al_run() {
    local id="$1" prompt="$2" stream="$3"
    shift 3
    local od
    od="$(mktemp -d)"
    # The JSON report on stdout is discarded (the assertions read the stream
    # files); --output-dir gives us the raw transcript without needing a JSON
    # parser. A non-zero harness exit is not fatal here — the outcome is judged by
    # the stream and the command's side effects, exactly as before.
    oneharness run --harness "$id" --prompt "$prompt" \
        --output-dir "$od" --compact "$@" >"$od/report.json" 2>"$od/oneharness.err" || true
    cp -f "$od/$id.stdout" "$stream" 2>/dev/null || : >"$stream"
    cp -f "$od/$id.stderr" "$stream.err" 2>/dev/null || : >"$stream.err"
    # Append oneharness's own diagnostics (a spawn failure or timeout note) so a
    # CI failure points straight at the cause.
    [ -s "$od/oneharness.err" ] && cat "$od/oneharness.err" >>"$stream.err"
    # oneharness reports a failed harness run as "see results[].status and
    # results[].error", but that detail lives in the JSON report on stdout (above,
    # otherwise discarded). Surface those fields so a CI failure shows *why* the
    # harness run did not succeed (e.g. a non-zero command exit on Windows).
    if [ -s "$od/report.json" ]; then
        grep -oE '"(status|error|exit_code|exitCode|code|signal)"[[:space:]]*:[^,}]*' \
            "$od/report.json" 2>/dev/null | sed 's/^/oneharness-report: /' >>"$stream.err" || true
    fi
    rm -rf "$od"
}

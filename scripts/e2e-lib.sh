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
AL_TOOL_RULES='
    { "name": "fence the read test: deny shell reads so only the read tool can surface the secret",
      "match": "@(cat|head|tail|less|more|nl|od|xxd|strings|base64|cut|grep|rg|sed|awk|tac|tr) *",
      "action": "deny" },
    { "name": "deny secret reads via the built-in read tool",
      "tool": "read", "action": "deny",
      "params": { "path": ["**/secret-allowlister.txt"] } },
    { "name": "deny forbidden writes via the built-in write tool",
      "tool": "write", "action": "deny",
      "params": { "path": ["**/blocked-by-allowlister.txt"] } },
    { "name": "deny forbidden edits via the built-in edit tool",
      "tool": "edit", "action": "deny",
      "params": { "path": ["**/blocked-by-allowlister.txt"] } },
    { "name": "deny destructive MCP tools by canonical tool name",
      "tool": "mcp", "action": "deny",
      "params": { "mcp_tool": ["delete*"] } }'

# Absolute path to the shared stdio MCP server fixture. Arg: repo_root.
al_mcp_server() { printf '%s/scripts/e2e-mcp-server.py' "$1"; }

# True when python3 can run the MCP server fixture.
al_have_python() { command -v python3 >/dev/null 2>&1; }

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
        | grep -avE '^[[:space:]]*$' | tail -60 | sed 's/^/    /'
    if [ -s "$stream.err" ]; then
        note "  stderr tail:"
        tail -10 "$stream.err" | sed 's/^/    /'
    fi
}

# True when allowlister's own deny reason reached the transcript — the strongest
# proof the gate actually fired on an attempted tool call (so a missing side
# effect is a real block, not the model declining to act). Reliable on harnesses
# that surface the hook reason (e.g. Claude, Cursor); others render their own.
# Args: <stream>.
al_reason_in() { grep -aq 'allowlister:' "$1" "$1.err" 2>/dev/null; }

# Conclude a built-in read-deny case. The secret marker must be absent from the
# transcript (hard). Liveness — that the read was actually attempted and blocked,
# not merely skipped — is shown by the deny reason surfacing, or failing that by
# the ungated public file's marker appearing. Args: <deny-stream>.
al_read_conclude() {
    local stream="$1"
    if grep -aqF "$AL_SECRET_MARKER" "$stream" "$stream.err" 2>/dev/null; then
        al_dump_stream "$stream"
        fail "secret leaked: the marker reached the transcript (the built-in read deny did not hold)"
    fi
    note "  ok: the secret was never surfaced — the built-in read deny held"
    if al_reason_in "$stream"; then
        note "  confirmed: the harness reported the read was denied (the gate fired on the attempt)"
    elif grep -aqF "$AL_PUBLIC_MARKER" "$stream" "$stream.err" 2>/dev/null; then
        note "  bonus: the ungated public file's marker surfaced, so the read tool did run this turn"
    else
        note "  note: could not independently confirm the read was attempted (marker/reason not echoed)"
    fi
}

# Conclude a built-in write-deny case (for harnesses with no gateable read). The
# forbidden file must be absent (hard). Args: <forbidden-file>.
al_write_conclude() {
    local target="$1" stream="${2:-}"
    if [ -e "$target" ]; then
        [ -n "$stream" ] && al_dump_stream "$stream"
        fail "forbidden file was created: $target (the built-in write deny did not hold)"
    fi
    note "  ok: the forbidden file was never created — the built-in write deny held"
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
    if al_reason_in "$stream"; then
        note "  confirmed: the harness reported the MCP call was denied (the gate fired on the attempt)"
    elif [ -n "$token" ] && grep -aqF "$token" "$stream" "$stream.err" 2>/dev/null; then
        note "  bonus: the safe \`echotoken\` result surfaced, so the harness does dispatch MCP tools"
    else
        note "  note: could not independently confirm the MCP call was attempted (token/reason not echoed)"
    fi
}

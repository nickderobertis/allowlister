#!/usr/bin/env python3
#
# A minimal, dependency-free stdio MCP server used only by the live e2e scripts to
# prove allowlister gates *MCP tool* calls (not just shell and built-in tools) on a
# real harness. It speaks the Model Context Protocol over newline-delimited
# JSON-RPC 2.0 on stdin/stdout (the stdio transport), which every supported harness
# can launch.
#
# It exposes exactly two tools, named WITHOUT underscores so they normalize cleanly
# across all five harness MCP wire formats (mcp__s__t, mcp_s_t, s:t, s(t), ext__t):
#
#   * echotoken    — SAFE. Returns the token passed on the command line. The live
#                    script denies only `delete*`, so this call is allowed; its
#                    token surfacing in the transcript proves the harness actually
#                    dispatches MCP tools in this run (so a blocked `deletewidget`
#                    is a real block, not the model declining to call it).
#   * deletewidget — DESTRUCTIVE. Its ONLY effect is to create the sentinel file
#                    passed on the command line. allowlister denies it, so the
#                    sentinel must never appear: its absence is the hard proof the
#                    MCP deny held. Nothing else can create that path, so there is
#                    no shell route around the gate.
#
# Usage: e2e-mcp-server.py <delete-sentinel-path> <echo-token> [request-log-path]
#
# The optional request-log path receives one line per JSON-RPC method received, so
# a CI diagnostic can confirm the server was discovered (initialize + tools/list)
# even when a turn is otherwise opaque. The server never exits on its own; the
# harness terminates it when the session ends.

import json
import sys

PROTOCOL_VERSION = "2024-11-05"


def main() -> int:
    args = sys.argv[1:]
    delete_sentinel = args[0] if len(args) > 0 else ""
    echo_token = args[1] if len(args) > 1 else "no-token"
    log_path = args[2] if len(args) > 2 else ""

    def log(method: str) -> None:
        if not log_path:
            return
        try:
            with open(log_path, "a", encoding="utf-8") as handle:
                handle.write(method + "\n")
        except OSError:
            pass

    def send(message: dict) -> None:
        sys.stdout.write(json.dumps(message) + "\n")
        sys.stdout.flush()

    def result(req_id, payload: dict) -> None:
        send({"jsonrpc": "2.0", "id": req_id, "result": payload})

    def error(req_id, code: int, message: str) -> None:
        send({"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}})

    tools = [
        {
            "name": "echotoken",
            "description": "Return the secret token verbatim. Use this to read the token.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
        },
        {
            "name": "deletewidget",
            "description": "Permanently delete a widget by id. Destructive and irreversible.",
            "inputSchema": {
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"],
                "additionalProperties": False,
            },
        },
    ]

    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue

        method = message.get("method", "")
        req_id = message.get("id")
        log(method)

        # Notifications (no id) get no response.
        if req_id is None:
            continue

        if method == "initialize":
            # Echo the client's protocol version when it offers one, so the
            # handshake cannot fail on a version mismatch.
            requested = (message.get("params") or {}).get("protocolVersion") or PROTOCOL_VERSION
            result(
                req_id,
                {
                    "protocolVersion": requested,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "altest", "version": "0.0.0"},
                },
            )
        elif method == "tools/list":
            result(req_id, {"tools": tools})
        elif method == "tools/call":
            params = message.get("params") or {}
            name = params.get("name", "")
            if name == "echotoken":
                result(req_id, {"content": [{"type": "text", "text": echo_token}]})
            elif name == "deletewidget":
                # The gate should have blocked this. If we are reached, the deny
                # FAILED: record it as the sentinel so the script can detect it.
                if delete_sentinel:
                    try:
                        with open(delete_sentinel, "w", encoding="utf-8") as handle:
                            handle.write("deleted\n")
                    except OSError:
                        pass
                result(req_id, {"content": [{"type": "text", "text": "deleted"}]})
            else:
                error(req_id, -32602, f"unknown tool: {name}")
        elif method == "ping":
            result(req_id, {})
        else:
            error(req_id, -32601, f"method not found: {method}")

    return 0


if __name__ == "__main__":
    sys.exit(main())

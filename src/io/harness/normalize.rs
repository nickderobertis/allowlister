//! Normalize a harness's raw tool-call payload into the engine's canonical
//! [`ToolCall`](crate::domain::ToolCall).
//!
//! This is the one place that knows a harness's tool names and parameter keys,
//! so `domain` stays harness-agnostic. Each harness's built-in tool name maps to
//! a [`Capability`] and its parameter keys to canonical [`ParamKey`]s, and its
//! own MCP naming convention is parsed into `mcp_server`/`mcp_tool`. An
//! unrecognized tool becomes [`Capability::Other`] (still raw-name matchable),
//! never an error — the engine then simply defers it.

use serde_json::Value;

use crate::domain::{Capability, NormalizedParams, ParamKey, ToolCall};

/// Normalize a Claude Code `PreToolUse` tool call.
///
/// Confirmed against live `claude` payloads: `Read`/`Write`/`Edit` carry
/// `file_path` (and `Write` a `content`), `WebFetch` a `url`, `WebSearch` a
/// `query`, and MCP tools use the `mcp__<server>__<tool>` name with structured
/// arguments under `tool_input`.
pub(crate) fn claude(tool_name: &str, tool_input: &Value) -> ToolCall {
    if let Some((server, tool)) = parse_mcp_dunder(tool_name) {
        let mut params = NormalizedParams::new();
        params.insert(ParamKey::McpServer, server);
        params.insert(ParamKey::McpTool, tool);
        return ToolCall::new(
            Capability::Mcp,
            tool_name.to_string(),
            params,
            tool_input.clone(),
        );
    }

    let capability = match tool_name {
        "Read" => Capability::Read,
        "Write" => Capability::Write,
        "Edit" => Capability::Edit,
        "Glob" => Capability::Glob,
        "Grep" => Capability::Grep,
        "WebFetch" => Capability::WebFetch,
        "WebSearch" => Capability::WebSearch,
        _ => Capability::Other,
    };

    let mut params = NormalizedParams::new();
    let mut set = |key: ParamKey, raw_key: &str| {
        if let Some(value) = tool_input.get(raw_key).and_then(Value::as_str) {
            params.insert(key, value.to_string());
        }
    };
    match capability {
        Capability::Read | Capability::Edit => set(ParamKey::Path, "file_path"),
        Capability::Write => {
            set(ParamKey::Path, "file_path");
            set(ParamKey::Content, "content");
        }
        Capability::Glob | Capability::Grep => {
            set(ParamKey::Pattern, "pattern");
            set(ParamKey::Path, "path");
        }
        Capability::WebFetch => set(ParamKey::Url, "url"),
        Capability::WebSearch => set(ParamKey::Query, "query"),
        // MCP is handled above; Other carries no canonical params.
        Capability::Mcp | Capability::Other => {}
    }

    ToolCall::new(
        capability,
        tool_name.to_string(),
        params,
        tool_input.clone(),
    )
}

/// Parse the `mcp__<server>__<tool>` convention (Claude Code, Codex, Qwen) into
/// `(server, tool)`. Returns `None` for any other name.
pub(crate) fn parse_mcp_dunder(tool_name: &str) -> Option<(String, String)> {
    let rest = tool_name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_maps_file_path_to_canonical_path() {
        let call = claude("Read", &json!({ "file_path": "/repo/a.ts" }));
        assert_eq!(call.capability, Capability::Read);
        assert_eq!(call.params.get(ParamKey::Path), Some("/repo/a.ts"));
    }

    #[test]
    fn write_maps_path_and_content() {
        let call = claude(
            "Write",
            &json!({ "file_path": "/repo/o.txt", "content": "x" }),
        );
        assert_eq!(call.capability, Capability::Write);
        assert_eq!(call.params.get(ParamKey::Path), Some("/repo/o.txt"));
        assert_eq!(call.params.get(ParamKey::Content), Some("x"));
    }

    #[test]
    fn web_fetch_and_search_map_url_and_query() {
        let fetch = claude(
            "WebFetch",
            &json!({ "url": "https://github.com/x", "prompt": "p" }),
        );
        assert_eq!(fetch.capability, Capability::WebFetch);
        assert_eq!(
            fetch.params.get(ParamKey::Url),
            Some("https://github.com/x")
        );

        let search = claude("WebSearch", &json!({ "query": "rust" }));
        assert_eq!(search.capability, Capability::WebSearch);
        assert_eq!(search.params.get(ParamKey::Query), Some("rust"));
    }

    #[test]
    fn mcp_name_parses_into_server_and_tool() {
        let call = claude("mcp__linear__list_issues", &json!({ "teamId": "T1" }));
        assert_eq!(call.capability, Capability::Mcp);
        assert_eq!(call.params.get(ParamKey::McpServer), Some("linear"));
        assert_eq!(call.params.get(ParamKey::McpTool), Some("list_issues"));
        // The raw object is retained for JSON-path matching of server params.
        assert_eq!(call.raw["teamId"], json!("T1"));
    }

    #[test]
    fn unknown_tool_is_other_with_no_canonical_params() {
        let call = claude("ToolSearch", &json!({ "query": "x" }));
        assert_eq!(call.capability, Capability::Other);
        assert!(call.params.is_empty());
        assert_eq!(call.tool_name, "ToolSearch");
    }

    #[test]
    fn malformed_mcp_names_are_not_mcp() {
        assert!(parse_mcp_dunder("mcp__only").is_none());
        assert!(parse_mcp_dunder("mcp____tool").is_none());
        assert!(parse_mcp_dunder("Read").is_none());
    }

    #[test]
    fn glob_grep_and_edit_map_their_keys() {
        let glob = claude("Glob", &json!({ "pattern": "**/*.rs", "path": "/repo" }));
        assert_eq!(glob.capability, Capability::Glob);
        assert_eq!(glob.params.get(ParamKey::Pattern), Some("**/*.rs"));
        assert_eq!(glob.params.get(ParamKey::Path), Some("/repo"));

        let grep = claude("Grep", &json!({ "pattern": "TODO", "path": "/repo/src" }));
        assert_eq!(grep.capability, Capability::Grep);
        assert_eq!(grep.params.get(ParamKey::Pattern), Some("TODO"));

        let edit = claude(
            "Edit",
            &json!({ "file_path": "/repo/x", "old_string": "a" }),
        );
        assert_eq!(edit.capability, Capability::Edit);
        assert_eq!(edit.params.get(ParamKey::Path), Some("/repo/x"));
    }
}

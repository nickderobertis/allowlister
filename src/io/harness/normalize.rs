//! Normalize a harness's raw tool-call payload into the engine's canonical
//! [`ToolCall`](crate::domain::ToolCall).
//!
//! This is the one place that knows a harness's tool names and parameter keys,
//! so `domain` stays harness-agnostic. Each harness's built-in tool name maps to
//! a [`Capability`] and its parameter keys to canonical [`ParamKey`]s via a small
//! table, and its own MCP naming convention is parsed into `mcp_server`/
//! `mcp_tool`. An unrecognized tool becomes [`Capability::Other`] (still raw-name
//! matchable), never an error — the engine then simply defers it.
//!
//! Param key names, tool names, and MCP wire formats were confirmed from each
//! harness's source/docs; the divergence is real (e.g. `file_path` vs `path` vs
//! `filePath`, and `mcp__s__t` vs `mcp_s_t` vs `s:t` vs `s(t)` vs `ext__t`), and
//! is exactly what this layer normalizes away so one allowlist is portable.

use serde_json::Value;

use crate::domain::{Capability, NormalizedParams, ParamKey, ToolCall};

/// One built-in tool's mapping: its raw name, the capability it maps to, and the
/// raw→canonical parameter-key pairs to lift out of `tool_input`.
struct ToolSpec {
    name: &'static str,
    capability: Capability,
    params: &'static [(&'static str, ParamKey)],
}

/// A per-harness MCP-name parser: raw tool name → `(server, tool)` or `None`.
type McpParser = fn(&str) -> Option<(String, String)>;

/// Build a [`ToolCall`] from a tool table and an MCP parser. The table is tried
/// first: some harnesses' MCP wire format is a bare `<server>_<tool>` that can
/// collide with a built-in whose own name contains `_` (e.g. OpenCode's
/// `apply_patch`), so a known built-in name always wins. Then the MCP parser;
/// otherwise the tool is `Other`, carrying only its raw name (still raw-name
/// matchable).
fn normalize(tool_name: &str, tool_input: &Value, table: &[ToolSpec], mcp: McpParser) -> ToolCall {
    if let Some(spec) = table.iter().find(|spec| spec.name == tool_name) {
        let mut params = NormalizedParams::new();
        for (raw, canonical) in spec.params {
            if let Some(value) = tool_input.get(*raw).and_then(Value::as_str) {
                params.insert(*canonical, value.to_string());
            }
        }
        return ToolCall::new(
            spec.capability,
            tool_name.to_string(),
            params,
            tool_input.clone(),
        );
    }
    if let Some((server, tool)) = mcp(tool_name) {
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
    ToolCall::new(
        Capability::Other,
        tool_name.to_string(),
        NormalizedParams::new(),
        tool_input.clone(),
    )
}

// ---------- MCP name parsers (one per wire format) ----------

/// `mcp__<server>__<tool>` — Claude Code, Codex, Qwen.
pub(crate) fn parse_mcp_dunder(tool_name: &str) -> Option<(String, String)> {
    let rest = tool_name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    nonempty(server, tool)
}

/// `mcp_<server>_<tool>` — Crush. Single underscores make this ambiguous when a
/// server or tool name itself contains `_`; we split on the first underscore as a
/// best effort (a rule can fall back to raw-name matching when that is wrong).
fn parse_mcp_underscore(tool_name: &str) -> Option<(String, String)> {
    if tool_name.starts_with("mcp__") {
        return None; // that is the dunder form, not Crush's.
    }
    let rest = tool_name.strip_prefix("mcp_")?;
    let (server, tool) = rest.split_once('_')?;
    nonempty(server, tool)
}

/// `<server>(<tool>)` — Copilot's sanitized server-qualified name.
fn parse_mcp_paren(tool_name: &str) -> Option<(String, String)> {
    let inner = tool_name.strip_suffix(')')?;
    let open = inner.find('(')?;
    nonempty(&inner[..open], &inner[open + 1..])
}

/// `<ext>__<tool>` — Goose's namespace, shared by built-in extensions and MCP
/// servers. The built-in `developer` extension is not an MCP server, so its
/// tools (`developer__text_editor`, `developer__shell`) are handled by `goose`
/// directly rather than treated as MCP.
fn parse_mcp_namespaced(tool_name: &str) -> Option<(String, String)> {
    let (server, tool) = tool_name.split_once("__")?;
    if server == "developer" {
        return None;
    }
    nonempty(server, tool)
}

/// `<server>_<tool>` — OpenCode's sanitized server-qualified name (e.g.
/// `altest_deletewidget`). Like Crush's form the single underscore is ambiguous
/// when a server or tool name contains `_`; split on the first underscore as a
/// best effort. Built-ins are matched by the table first (see [`normalize`]), so
/// a name like `apply_patch` is never mistaken for an MCP server `apply`.
fn parse_mcp_opencode(tool_name: &str) -> Option<(String, String)> {
    let (server, tool) = tool_name.split_once('_')?;
    nonempty(server, tool)
}

fn nonempty(server: &str, tool: &str) -> Option<(String, String)> {
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_string(), tool.to_string()))
}

// ---------- Per-harness tool tables + entry points ----------

const CLAUDE: &[ToolSpec] = &[
    ToolSpec {
        name: "Read",
        capability: Capability::Read,
        params: &[("file_path", ParamKey::Path)],
    },
    ToolSpec {
        name: "Edit",
        capability: Capability::Edit,
        params: &[("file_path", ParamKey::Path)],
    },
    ToolSpec {
        name: "Write",
        capability: Capability::Write,
        params: &[
            ("file_path", ParamKey::Path),
            ("content", ParamKey::Content),
        ],
    },
    ToolSpec {
        name: "Glob",
        capability: Capability::Glob,
        params: &[("pattern", ParamKey::Pattern), ("path", ParamKey::Path)],
    },
    ToolSpec {
        name: "Grep",
        capability: Capability::Grep,
        params: &[("pattern", ParamKey::Pattern), ("path", ParamKey::Path)],
    },
    ToolSpec {
        name: "WebFetch",
        capability: Capability::WebFetch,
        params: &[("url", ParamKey::Url)],
    },
    ToolSpec {
        name: "WebSearch",
        capability: Capability::WebSearch,
        params: &[("query", ParamKey::Query)],
    },
];

/// Normalize a Claude Code `PreToolUse` tool call (built-ins use `file_path`/
/// `content`; MCP is `mcp__server__tool`). Confirmed against live `claude`.
pub(crate) fn claude(tool_name: &str, tool_input: &Value) -> ToolCall {
    normalize(tool_name, tool_input, CLAUDE, parse_mcp_dunder)
}

const QWEN: &[ToolSpec] = &[
    ToolSpec {
        name: "read_file",
        capability: Capability::Read,
        params: &[("file_path", ParamKey::Path)],
    },
    ToolSpec {
        name: "write_file",
        capability: Capability::Write,
        params: &[
            ("file_path", ParamKey::Path),
            ("content", ParamKey::Content),
        ],
    },
    ToolSpec {
        name: "edit",
        capability: Capability::Edit,
        params: &[("file_path", ParamKey::Path)],
    },
    ToolSpec {
        name: "glob",
        capability: Capability::Glob,
        params: &[("pattern", ParamKey::Pattern), ("path", ParamKey::Path)],
    },
    ToolSpec {
        name: "grep_search",
        capability: Capability::Grep,
        params: &[("pattern", ParamKey::Pattern), ("path", ParamKey::Path)],
    },
    ToolSpec {
        name: "web_fetch",
        capability: Capability::WebFetch,
        params: &[("url", ParamKey::Url)],
    },
];

/// Normalize a Qwen Code `PreToolUse` tool call (Gemini-style names; canonical
/// keys are `file_path`/`content`; MCP is `mcp__server__tool`).
pub(crate) fn qwen(tool_name: &str, tool_input: &Value) -> ToolCall {
    normalize(tool_name, tool_input, QWEN, parse_mcp_dunder)
}

const CRUSH: &[ToolSpec] = &[
    ToolSpec {
        name: "view",
        capability: Capability::Read,
        params: &[("file_path", ParamKey::Path)],
    },
    ToolSpec {
        name: "write",
        capability: Capability::Write,
        params: &[
            ("file_path", ParamKey::Path),
            ("content", ParamKey::Content),
        ],
    },
    ToolSpec {
        name: "edit",
        capability: Capability::Edit,
        params: &[("file_path", ParamKey::Path)],
    },
    ToolSpec {
        name: "multiedit",
        capability: Capability::Edit,
        params: &[("file_path", ParamKey::Path)],
    },
    ToolSpec {
        name: "fetch",
        capability: Capability::WebFetch,
        params: &[("url", ParamKey::Url)],
    },
    ToolSpec {
        name: "web_fetch",
        capability: Capability::WebFetch,
        params: &[("url", ParamKey::Url)],
    },
    ToolSpec {
        name: "web_search",
        capability: Capability::WebSearch,
        params: &[("query", ParamKey::Query)],
    },
    ToolSpec {
        name: "glob",
        capability: Capability::Glob,
        params: &[("pattern", ParamKey::Pattern), ("path", ParamKey::Path)],
    },
    ToolSpec {
        name: "grep",
        capability: Capability::Grep,
        params: &[("pattern", ParamKey::Pattern), ("path", ParamKey::Path)],
    },
];

/// Normalize a Crush `PreToolUse` tool call (`view` is read; keys are `file_path`/
/// `content`; MCP is the single-underscore `mcp_server_tool`).
pub(crate) fn crush(tool_name: &str, tool_input: &Value) -> ToolCall {
    normalize(tool_name, tool_input, CRUSH, parse_mcp_underscore)
}

const CODEX: &[ToolSpec] = &[
    // Codex has no native read tool, and `apply_patch` carries the path inside a
    // patch string under `command` (no discrete path), so it maps to `edit` with
    // no canonical params — a capability-only `edit` rule can still gate it.
    ToolSpec {
        name: "apply_patch",
        capability: Capability::Edit,
        params: &[],
    },
];

/// Normalize an OpenAI Codex CLI `PreToolUse` tool call (`apply_patch` for writes;
/// MCP is `mcp__server__tool`).
pub(crate) fn codex(tool_name: &str, tool_input: &Value) -> ToolCall {
    normalize(tool_name, tool_input, CODEX, parse_mcp_dunder)
}

// Goose's developer extension delivers its file tools to the hook under BARE
// names (verified from a live payload: `write` carries `path`/`content`), not the
// `developer__`-namespaced names the docs use. Map those bare tools here; the
// multi-purpose `text_editor` (older Goose) is handled by `goose` below. Any
// `<server>__<tool>` that isn't a developer builtin is MCP.
const GOOSE: &[ToolSpec] = &[
    ToolSpec {
        name: "write",
        capability: Capability::Write,
        params: &[("path", ParamKey::Path), ("content", ParamKey::Content)],
    },
    ToolSpec {
        name: "read",
        capability: Capability::Read,
        params: &[("path", ParamKey::Path)],
    },
    ToolSpec {
        name: "edit",
        capability: Capability::Edit,
        params: &[("path", ParamKey::Path)],
    },
];

/// Normalize a Goose `PreToolUse` tool call. Developer file tools arrive under bare
/// names (`write`/`read`/`edit`) or, on older Goose, the multi-purpose
/// `text_editor` whose `command` selects the capability; any non-developer
/// `<server>__<tool>` is MCP.
pub(crate) fn goose(tool_name: &str, tool_input: &Value) -> ToolCall {
    if tool_name == "text_editor" || tool_name == "developer__text_editor" {
        return goose_text_editor(tool_name, tool_input);
    }
    normalize(tool_name, tool_input, GOOSE, parse_mcp_namespaced)
}

/// Map Goose's `text_editor` to a capability by its `command`: `view` is a read,
/// `write` a write, and `str_replace`/`insert`/anything else an edit. The file is
/// at `path`; new content (for `write`) is at `file_text`.
fn goose_text_editor(tool_name: &str, tool_input: &Value) -> ToolCall {
    let capability = match tool_input.get("command").and_then(Value::as_str) {
        Some("view") => Capability::Read,
        Some("write") => Capability::Write,
        _ => Capability::Edit,
    };
    let mut params = NormalizedParams::new();
    if let Some(path) = tool_input.get("path").and_then(Value::as_str) {
        params.insert(ParamKey::Path, path.to_string());
    }
    if let Some(content) = tool_input.get("file_text").and_then(Value::as_str) {
        params.insert(ParamKey::Content, content.to_string());
    }
    ToolCall::new(
        capability,
        tool_name.to_string(),
        params,
        tool_input.clone(),
    )
}

const COPILOT: &[ToolSpec] = &[
    ToolSpec {
        name: "view",
        capability: Capability::Read,
        params: &[("path", ParamKey::Path)],
    },
    ToolSpec {
        name: "create",
        capability: Capability::Write,
        params: &[("path", ParamKey::Path), ("file_text", ParamKey::Content)],
    },
    ToolSpec {
        name: "edit",
        capability: Capability::Edit,
        params: &[("path", ParamKey::Path)],
    },
    ToolSpec {
        name: "web_fetch",
        capability: Capability::WebFetch,
        params: &[("url", ParamKey::Url)],
    },
];

/// Normalize a GitHub Copilot CLI `preToolUse` tool call. Copilot encodes
/// `toolArgs` as a JSON *string*, so parse it first; `view` is read with `path`,
/// `create` writes with `path`/`file_text`; MCP is the `server(tool)` form.
pub(crate) fn copilot(tool_name: &str, tool_args: &Value) -> ToolCall {
    match tool_args {
        Value::String(text) => {
            let parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
            normalize(tool_name, &parsed, COPILOT, parse_mcp_paren)
        }
        other => normalize(tool_name, other, COPILOT, parse_mcp_paren),
    }
}

const OPENCODE: &[ToolSpec] = &[
    ToolSpec {
        name: "read",
        capability: Capability::Read,
        params: &[("filePath", ParamKey::Path)],
    },
    ToolSpec {
        name: "write",
        capability: Capability::Write,
        params: &[("filePath", ParamKey::Path), ("content", ParamKey::Content)],
    },
    ToolSpec {
        name: "edit",
        capability: Capability::Edit,
        params: &[("filePath", ParamKey::Path)],
    },
    ToolSpec {
        name: "apply_patch",
        capability: Capability::Edit,
        params: &[],
    },
    ToolSpec {
        name: "webfetch",
        capability: Capability::WebFetch,
        params: &[("url", ParamKey::Url)],
    },
    ToolSpec {
        name: "websearch",
        capability: Capability::WebSearch,
        params: &[("query", ParamKey::Query)],
    },
    ToolSpec {
        name: "grep",
        capability: Capability::Grep,
        params: &[("pattern", ParamKey::Pattern), ("path", ParamKey::Path)],
    },
    ToolSpec {
        name: "glob",
        capability: Capability::Glob,
        params: &[("pattern", ParamKey::Pattern), ("path", ParamKey::Path)],
    },
];

/// Normalize an OpenCode `tool.execute.before` call (the shim forwards `input.tool`
/// as the name and `output.args` as the input). Built-ins use camelCase
/// `filePath`; MCP is the `server_tool` form (e.g. `altest_deletewidget`).
pub(crate) fn opencode(tool_name: &str, tool_input: &Value) -> ToolCall {
    normalize(tool_name, tool_input, OPENCODE, parse_mcp_opencode)
}

/// Normalize a Cursor `beforeReadFile` event into a read tool call. The event has
/// no `tool_name`; the file is at `file_path`.
pub(crate) fn cursor_read(file_path: &str) -> ToolCall {
    let mut params = NormalizedParams::new();
    params.insert(ParamKey::Path, file_path.to_string());
    ToolCall::new(
        Capability::Read,
        "beforeReadFile".to_string(),
        params,
        serde_json::json!({ "file_path": file_path }),
    )
}

/// Normalize a Cursor `beforeMCPExecution` event. Its `tool_input` is an object
/// per Cursor's types but a JSON string per its docs, so parse-if-string; the
/// name is the standard `mcp__server__tool`.
pub(crate) fn cursor_mcp(tool_name: &str, tool_input: &Value) -> ToolCall {
    match tool_input {
        Value::String(text) => {
            let parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
            normalize(tool_name, &parsed, &[], parse_mcp_dunder)
        }
        other => normalize(tool_name, other, &[], parse_mcp_dunder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_maps_read_write_and_mcp() {
        let read = claude("Read", &json!({ "file_path": "/repo/a.ts" }));
        assert_eq!(read.capability, Capability::Read);
        assert_eq!(read.params.get(ParamKey::Path), Some("/repo/a.ts"));

        let write = claude("Write", &json!({ "file_path": "/repo/o", "content": "x" }));
        assert_eq!(write.params.get(ParamKey::Content), Some("x"));

        let mcp = claude("mcp__linear__list_issues", &json!({}));
        assert_eq!(mcp.capability, Capability::Mcp);
        assert_eq!(mcp.params.get(ParamKey::McpServer), Some("linear"));
        assert_eq!(mcp.params.get(ParamKey::McpTool), Some("list_issues"));
    }

    #[test]
    fn claude_glob_grep_edit_and_unknown() {
        let glob = claude("Glob", &json!({ "pattern": "**/*.rs", "path": "/repo" }));
        assert_eq!(glob.capability, Capability::Glob);
        assert_eq!(glob.params.get(ParamKey::Pattern), Some("**/*.rs"));
        let grep = claude("Grep", &json!({ "pattern": "TODO" }));
        assert_eq!(grep.capability, Capability::Grep);
        let edit = claude("Edit", &json!({ "file_path": "/x" }));
        assert_eq!(edit.capability, Capability::Edit);
        let other = claude("ToolSearch", &json!({ "q": 1 }));
        assert_eq!(other.capability, Capability::Other);
        assert!(other.params.is_empty());
    }

    #[test]
    fn qwen_uses_gemini_names() {
        let read = qwen("read_file", &json!({ "file_path": "/r/a", "offset": 0 }));
        assert_eq!(read.capability, Capability::Read);
        assert_eq!(read.params.get(ParamKey::Path), Some("/r/a"));
        let grep = qwen("grep_search", &json!({ "pattern": "x", "path": "/r" }));
        assert_eq!(grep.capability, Capability::Grep);
        let mcp = qwen("mcp__memory__create", &json!({}));
        assert_eq!(mcp.capability, Capability::Mcp);
    }

    #[test]
    fn crush_view_is_read_and_single_underscore_mcp() {
        let read = crush("view", &json!({ "file_path": "/r/a" }));
        assert_eq!(read.capability, Capability::Read);
        assert_eq!(read.params.get(ParamKey::Path), Some("/r/a"));
        let mcp = crush("mcp_linear_list_issues", &json!({}));
        assert_eq!(mcp.capability, Capability::Mcp);
        assert_eq!(mcp.params.get(ParamKey::McpServer), Some("linear"));
        assert_eq!(mcp.params.get(ParamKey::McpTool), Some("list_issues"));
        // The dunder form is not Crush's; it stays Other (not MCP) here.
        assert_eq!(crush("mcp__x__y", &json!({})).capability, Capability::Other);
    }

    #[test]
    fn codex_apply_patch_is_edit_without_path() {
        let edit = codex("apply_patch", &json!({ "command": "*** Begin Patch" }));
        assert_eq!(edit.capability, Capability::Edit);
        assert!(edit.params.is_empty());
        assert_eq!(
            codex("mcp__memory__create", &json!({})).capability,
            Capability::Mcp
        );
    }

    #[test]
    fn goose_text_editor_capability_follows_command() {
        // `developer__text_editor` is one tool whose `command` selects the
        // capability: view -> read, write -> write, str_replace/insert -> edit.
        let view = goose(
            "developer__text_editor",
            &json!({ "command": "view", "path": "/r/a" }),
        );
        assert_eq!(view.capability, Capability::Read);
        assert_eq!(view.params.get(ParamKey::Path), Some("/r/a"));

        let write = goose(
            "developer__text_editor",
            &json!({ "command": "write", "path": "/r/a", "file_text": "x" }),
        );
        assert_eq!(write.capability, Capability::Write);
        assert_eq!(write.params.get(ParamKey::Path), Some("/r/a"));
        assert_eq!(write.params.get(ParamKey::Content), Some("x"));

        let edit = goose(
            "developer__text_editor",
            &json!({ "command": "str_replace", "path": "/r/a" }),
        );
        assert_eq!(edit.capability, Capability::Edit);
    }

    #[test]
    fn goose_bare_developer_tools_map_to_capabilities() {
        // Goose delivers its developer file tools to the hook under bare names.
        let write = goose("write", &json!({ "path": "/r/a", "content": "x" }));
        assert_eq!(write.capability, Capability::Write);
        assert_eq!(write.params.get(ParamKey::Path), Some("/r/a"));
        assert_eq!(write.params.get(ParamKey::Content), Some("x"));

        let read = goose("read", &json!({ "path": "/r/a" }));
        assert_eq!(read.capability, Capability::Read);
        assert_eq!(read.params.get(ParamKey::Path), Some("/r/a"));
    }

    #[test]
    fn goose_namespace_distinguishes_builtin_from_mcp() {
        // The developer shell is not a file tool, and a non-developer namespace is
        // an MCP server.
        assert_eq!(
            goose("developer__shell", &json!({ "command": "ls" })).capability,
            Capability::Other
        );
        let mcp = goose("linear__list_issues", &json!({}));
        assert_eq!(mcp.capability, Capability::Mcp);
        assert_eq!(mcp.params.get(ParamKey::McpServer), Some("linear"));
    }

    #[test]
    fn copilot_parses_stringified_args_and_paren_mcp() {
        // toolArgs arrives as a JSON string.
        let read = copilot("view", &json!(r#"{"path":"/repo/a"}"#));
        assert_eq!(read.capability, Capability::Read);
        assert_eq!(read.params.get(ParamKey::Path), Some("/repo/a"));
        let create = copilot("create", &json!(r#"{"path":"/r/o","file_text":"x"}"#));
        assert_eq!(create.capability, Capability::Write);
        assert_eq!(create.params.get(ParamKey::Content), Some("x"));
        // An object also works (defensive), and MCP is server(tool).
        let mcp = copilot("linear(list_issues)", &json!({}));
        assert_eq!(mcp.capability, Capability::Mcp);
        assert_eq!(mcp.params.get(ParamKey::McpTool), Some("list_issues"));
    }

    #[test]
    fn opencode_camel_case_and_underscore_mcp() {
        let read = opencode("read", &json!({ "filePath": "/repo/a" }));
        assert_eq!(read.capability, Capability::Read);
        assert_eq!(read.params.get(ParamKey::Path), Some("/repo/a"));
        let write = opencode("write", &json!({ "filePath": "/r/o", "content": "x" }));
        assert_eq!(write.params.get(ParamKey::Content), Some("x"));
        // OpenCode names MCP tools `<server>_<tool>` (e.g. `altest_deletewidget`).
        let mcp = opencode("altest_deletewidget", &json!({ "id": "1" }));
        assert_eq!(mcp.capability, Capability::Mcp);
        assert_eq!(mcp.params.get(ParamKey::McpServer), Some("altest"));
        assert_eq!(mcp.params.get(ParamKey::McpTool), Some("deletewidget"));
        // A built-in whose name contains `_` is NOT mistaken for MCP: the table
        // wins (see `normalize`), so `apply_patch` stays an edit.
        assert_eq!(
            opencode("apply_patch", &json!({})).capability,
            Capability::Edit
        );
    }

    #[test]
    fn cursor_read_event_and_mcp() {
        let read = cursor_read("/repo/secret.txt");
        assert_eq!(read.capability, Capability::Read);
        assert_eq!(read.params.get(ParamKey::Path), Some("/repo/secret.txt"));

        // Object args.
        let mcp = cursor_mcp("mcp__filesystem__read_file", &json!({ "path": "/x" }));
        assert_eq!(mcp.capability, Capability::Mcp);
        assert_eq!(mcp.params.get(ParamKey::McpServer), Some("filesystem"));
        // Stringified args (Cursor's docs shape) also work.
        let mcp_str = cursor_mcp("mcp__linear__list", &json!(r#"{"teamId":"T"}"#));
        assert_eq!(mcp_str.capability, Capability::Mcp);
        assert_eq!(mcp_str.raw["teamId"], json!("T"));
    }

    #[test]
    fn malformed_mcp_names_do_not_parse() {
        assert!(parse_mcp_dunder("mcp__only").is_none());
        assert!(parse_mcp_dunder("Read").is_none());
        assert!(parse_mcp_underscore("mcp_only").is_none());
        assert!(parse_mcp_paren("plain").is_none());
        assert!(parse_mcp_namespaced("developer__shell").is_none());
        assert!(parse_mcp_namespaced("noseparator").is_none());
        assert!(parse_mcp_opencode("plain").is_none());
    }
}

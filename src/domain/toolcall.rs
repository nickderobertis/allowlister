//! The canonical, harness-agnostic model of a non-shell tool call.
//!
//! A shell command is decomposed by the [`analyzer`](super::analyzer) into
//! role-tagged fragments; a *tool* call has no shell structure — it is a
//! capability plus a bag of named parameters. Each adapter normalizes its
//! harness's raw hook payload into this shape (see `io/harness/normalize`), so
//! the engine never sees a harness-specific tool name or parameter key.
//!
//! This stays pure data. `raw` carries the original tool-input JSON so a rule
//! can match any server-defined parameter by JSON path, but the domain only ever
//! *reads* it — no I/O.

use std::collections::BTreeMap;

use serde_json::Value;

/// The portable capability vocabulary every harness's built-in tools map onto.
///
/// Shell is intentionally absent: it keeps its richer structural path through
/// the [`analyzer`](super::analyzer). These are the *non-shell* capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Read,
    Write,
    Edit,
    Glob,
    Grep,
    WebFetch,
    WebSearch,
    /// A Model Context Protocol tool, addressed by `mcp_server` + `mcp_tool`.
    Mcp,
    /// A tool an adapter recognized but has no canonical capability for. It is
    /// still matchable by raw tool name, but no canonical parameter sugar
    /// applies — so an unmatched `Other` simply defers, never crashes.
    Other,
}

impl Capability {
    /// The lowercase vocabulary word, as written in a rule's `tool` field.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Read => "read",
            Capability::Write => "write",
            Capability::Edit => "edit",
            Capability::Glob => "glob",
            Capability::Grep => "grep",
            Capability::WebFetch => "web_fetch",
            Capability::WebSearch => "web_search",
            Capability::Mcp => "mcp",
            Capability::Other => "other",
        }
    }

    /// Parse a rule's `tool` field into a capability selector. `Other` is not a
    /// user-selectable word (it only arises from normalization), so it returns
    /// `None`; an unrecognized word is likewise `None`, which lets the config
    /// layer fall back to raw-tool-name glob matching (e.g. `mcp__github__*`).
    pub fn parse(value: &str) -> Option<Capability> {
        Some(match value {
            "read" => Capability::Read,
            "write" => Capability::Write,
            "edit" => Capability::Edit,
            "glob" => Capability::Glob,
            "grep" => Capability::Grep,
            "web_fetch" => Capability::WebFetch,
            "web_search" => Capability::WebSearch,
            "mcp" => Capability::Mcp,
            _ => return None,
        })
    }
}

/// A canonical scalar parameter name. Built-in tools across harnesses normalize
/// their own keys (`file_path`/`path`/`filePath`/…) onto these, so one rule is
/// portable. MCP addressing fills `McpServer`/`McpTool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, strum::VariantArray)]
pub enum ParamKey {
    Path,
    Url,
    Query,
    Pattern,
    Content,
    McpServer,
    McpTool,
}

impl ParamKey {
    /// The canonical name as written in a rule's `params` map.
    pub fn as_str(self) -> &'static str {
        match self {
            ParamKey::Path => "path",
            ParamKey::Url => "url",
            ParamKey::Query => "query",
            ParamKey::Pattern => "pattern",
            ParamKey::Content => "content",
            ParamKey::McpServer => "mcp_server",
            ParamKey::McpTool => "mcp_tool",
        }
    }

    /// Parse a canonical parameter name from a rule's `params` key.
    pub fn parse(value: &str) -> Option<ParamKey> {
        Some(match value {
            "path" => ParamKey::Path,
            "url" => ParamKey::Url,
            "query" => ParamKey::Query,
            "pattern" => ParamKey::Pattern,
            "content" => ParamKey::Content,
            "mcp_server" => ParamKey::McpServer,
            "mcp_tool" => ParamKey::McpTool,
            _ => return None,
        })
    }

    /// Whether a value for this parameter is a filesystem path, and so should be
    /// rejected when it contains `..` traversal (the same guard redirections use).
    pub fn is_path_like(self) -> bool {
        matches!(self, ParamKey::Path)
    }
}

/// The canonical scalar parameters an adapter extracted, keyed by [`ParamKey`].
#[derive(Clone, Debug, Default)]
pub struct NormalizedParams {
    map: BTreeMap<ParamKey, String>,
}

impl NormalizedParams {
    pub fn new() -> Self {
        NormalizedParams::default()
    }

    /// Record a canonical parameter value the adapter mapped from a raw key.
    pub fn insert(&mut self, key: ParamKey, value: String) {
        self.map.insert(key, value);
    }

    /// The value for a canonical parameter, if the call carried one.
    pub fn get(&self, key: ParamKey) -> Option<&str> {
        self.map.get(&key).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// A single non-shell tool invocation, already normalized into the portable
/// vocabulary by the adapter layer.
#[derive(Clone, Debug)]
pub struct ToolCall {
    /// The portable capability this call maps to.
    pub capability: Capability,
    /// The harness's own tool name, e.g. `"Read"`, `"mcp__github__create_issue"`.
    /// Retained for diagnostics and for raw-name matching of MCP/`Other` tools.
    pub tool_name: String,
    /// Canonical scalar parameters the adapter mapped (path/url/query/…).
    pub params: NormalizedParams,
    /// The original tool-input object, verbatim, for JSON-path matching of any
    /// server-defined parameter the canonical set does not cover.
    pub raw: Value,
}

impl ToolCall {
    pub fn new(
        capability: Capability,
        tool_name: String,
        params: NormalizedParams,
        raw: Value,
    ) -> Self {
        ToolCall {
            capability,
            tool_name,
            params,
            raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_round_trips_through_str() {
        for cap in [
            Capability::Read,
            Capability::Write,
            Capability::Edit,
            Capability::Glob,
            Capability::Grep,
            Capability::WebFetch,
            Capability::WebSearch,
            Capability::Mcp,
        ] {
            assert_eq!(Capability::parse(cap.as_str()), Some(cap));
        }
    }

    #[test]
    fn other_and_unknown_capabilities_are_not_user_selectable() {
        assert_eq!(Capability::parse("other"), None);
        assert_eq!(Capability::parse("mcp__github__create"), None);
        assert_eq!(Capability::parse(""), None);
    }

    #[test]
    fn param_key_round_trips_and_only_path_is_path_like() {
        for key in [
            ParamKey::Path,
            ParamKey::Url,
            ParamKey::Query,
            ParamKey::Pattern,
            ParamKey::Content,
            ParamKey::McpServer,
            ParamKey::McpTool,
        ] {
            assert_eq!(ParamKey::parse(key.as_str()), Some(key));
            assert_eq!(key.is_path_like(), key == ParamKey::Path);
        }
        assert_eq!(ParamKey::parse("nope"), None);
    }

    #[test]
    fn normalized_params_insert_and_get() {
        let mut params = NormalizedParams::new();
        assert!(params.is_empty());
        params.insert(ParamKey::Path, "/repo/x".to_string());
        assert_eq!(params.get(ParamKey::Path), Some("/repo/x"));
        assert_eq!(params.get(ParamKey::Url), None);
        assert!(!params.is_empty());
    }
}

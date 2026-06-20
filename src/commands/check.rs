//! `allowlister check '<cmd>'` — evaluate one command, or one tool call, and
//! print its verdict.

use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::config;
use crate::domain::{self, Capability, NormalizedParams, ParamKey, ToolCall, Verdict};
use crate::errors::{Error, Result};
use crate::io::harness::normalize;
use crate::io::plugins;

use super::resolve_cwd;

#[derive(Serialize)]
struct CheckJson<'a> {
    verdict: &'a str,
    reason: &'a str,
}

/// Inputs to [`run`]. Either `command` (a shell command) or `tool` (a tool call)
/// is set — the CLI's argument group enforces exactly one.
pub struct CheckArgs<'a> {
    pub command: Option<&'a str>,
    pub cwd: Option<&'a Path>,
    pub json: bool,
    pub tool: Option<&'a str>,
    pub params: &'a [String],
    pub raw: Option<&'a str>,
}

/// Evaluate a command or tool call. Returns exit code 2 for deny, 0 otherwise;
/// a malformed `--tool` invocation is a usage error (exit 1).
pub fn run(args: CheckArgs) -> Result<i32> {
    let cwd = resolve_cwd(args.cwd);
    let loaded = config::load(&cwd);

    let result = if let Some(tool) = args.tool {
        let call = build_tool_call(tool, args.params, args.raw)?;
        let result = domain::evaluate_tool_call(&call, &loaded.tool_rules);
        plugins::evaluate_tool(
            &loaded.plugins,
            "check",
            &cwd.to_string_lossy(),
            // A manual `check` is not inside a harness session.
            None,
            &call,
            result,
        )
    } else {
        // The CLI argument group guarantees a command when `--tool` is absent.
        let command = args.command.unwrap_or_default();
        let result = domain::evaluate(command, &loaded.rules);
        plugins::evaluate_shell(
            &loaded.plugins,
            "check",
            &cwd.to_string_lossy(),
            // A manual `check` is not inside a harness session.
            None,
            command,
            result,
        )
    };

    if args.json {
        let payload = CheckJson {
            verdict: result.verdict.as_str(),
            reason: &result.reason,
        };
        // This fixed shape always serializes.
        if let Ok(line) = serde_json::to_string(&payload) {
            println!("{line}");
        }
    } else {
        println!(
            "{}: {}",
            result.verdict.as_str().to_uppercase(),
            result.reason
        );
    }

    Ok(match result.verdict {
        Verdict::Deny => 2,
        _ => 0,
    })
}

/// Build a synthetic [`ToolCall`] from the CLI flags, mirroring what an adapter's
/// normalizer would produce: a capability word maps to a [`Capability`]; a raw
/// `mcp__server__tool` name maps to `Capability::Mcp` with the server/tool filled
/// in; anything else is `Capability::Other`. `--param key=value` sets canonical
/// parameters, and `--raw` (or, absent it, the params) provides the JSON object
/// that `jsonpath` rules see.
fn build_tool_call(tool: &str, params: &[String], raw: Option<&str>) -> Result<ToolCall> {
    let mut canonical = NormalizedParams::new();
    let mut raw_map = Map::new();
    for entry in params {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| usage(format!("--param must be key=value, got '{entry}'")))?;
        let param = ParamKey::parse(key)
            .ok_or_else(|| usage(format!("unknown canonical param '{key}'")))?;
        canonical.insert(param, value.to_string());
        raw_map.insert(key.to_string(), Value::String(value.to_string()));
    }

    let raw_value = match raw {
        Some(text) => serde_json::from_str::<Value>(text)
            .map_err(|err| usage(format!("--raw is not valid JSON: {err}")))?,
        None => Value::Object(raw_map),
    };

    let (capability, mcp) = classify(tool);
    for (key, value) in mcp {
        canonical.insert(key, value);
    }
    Ok(ToolCall::new(
        capability,
        tool.to_string(),
        canonical,
        raw_value,
    ))
}

/// Resolve a `--tool` value to a capability, plus any MCP server/tool params a
/// raw `mcp__…` name implies.
fn classify(tool: &str) -> (Capability, Vec<(ParamKey, String)>) {
    if let Some(capability) = Capability::parse(tool) {
        return (capability, Vec::new());
    }
    if let Some((server, name)) = normalize::parse_mcp_dunder(tool) {
        return (
            Capability::Mcp,
            vec![(ParamKey::McpServer, server), (ParamKey::McpTool, name)],
        );
    }
    (Capability::Other, Vec::new())
}

fn usage(message: String) -> Error {
    Error::InvalidConfig {
        origin: "check".to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_capability_word() {
        let (cap, mcp) = classify("read");
        assert_eq!(cap, Capability::Read);
        assert!(mcp.is_empty());
    }

    #[test]
    fn classify_parses_raw_mcp_name() {
        let (cap, mcp) = classify("mcp__linear__list_issues");
        assert_eq!(cap, Capability::Mcp);
        assert_eq!(
            mcp,
            vec![
                (ParamKey::McpServer, "linear".to_string()),
                (ParamKey::McpTool, "list_issues".to_string()),
            ]
        );
    }

    #[test]
    fn classify_unknown_tool_is_other() {
        let (cap, _) = classify("Frobnicate");
        assert_eq!(cap, Capability::Other);
    }

    #[test]
    fn build_tool_call_collects_params_into_canonical_and_raw() {
        let call = build_tool_call("read", &["path=/repo/x".to_string()], None).unwrap();
        assert_eq!(call.capability, Capability::Read);
        assert_eq!(call.params.get(ParamKey::Path), Some("/repo/x"));
        assert_eq!(call.raw["path"], Value::String("/repo/x".to_string()));
    }

    #[test]
    fn build_tool_call_rejects_bad_param_and_raw() {
        assert!(build_tool_call("read", &["noeq".to_string()], None).is_err());
        assert!(build_tool_call("read", &["bogus=1".to_string()], None).is_err());
        assert!(build_tool_call("mcp", &[], Some("{not json")).is_err());
    }

    #[test]
    fn build_tool_call_uses_explicit_raw_json() {
        let call =
            build_tool_call("mcp__github__create", &[], Some(r#"{"owner":"acme"}"#)).unwrap();
        assert_eq!(call.capability, Capability::Mcp);
        assert_eq!(call.params.get(ParamKey::McpServer), Some("github"));
        assert_eq!(call.raw["owner"], Value::String("acme".to_string()));
    }
}

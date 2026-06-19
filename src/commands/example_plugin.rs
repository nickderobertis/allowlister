//! A tiny built-in example dynamic approval plugin.
//!
//! It is hidden from the normal CLI help but exercised by the e2e suite and
//! mirrors `examples/dynamic-approval-plugin.sh`: read a plugin request JSON from
//! stdin and return one verdict JSON on stdout.

use std::collections::BTreeMap;
use std::io::Read;

use serde::Deserialize;
use serde_json::json;

use crate::errors::Result;

#[derive(Debug, Default, Deserialize)]
struct Request {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    protocol_version: u8,
    #[serde(default)]
    fragments: Vec<RequestFragment>,
    #[serde(default)]
    tool: Option<RequestTool>,
}

/// The protocol-v2 tool payload, deserialized so the plugin can prove it received
/// the structured tool call rather than only `current_reason`.
#[derive(Debug, Default, Clone, Deserialize)]
struct RequestTool {
    #[serde(default)]
    name: String,
    #[serde(default)]
    capability: String,
    #[serde(default)]
    params: BTreeMap<String, String>,
}

/// The protocol-v2 per-fragment payload, deserialized so the plugin can prove it
/// received the structured decomposition rather than only `current_reason`.
#[derive(Debug, Deserialize)]
struct RequestFragment {
    #[serde(default)]
    role: String,
    #[serde(default)]
    verdict: String,
    #[serde(default)]
    rule: Option<String>,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    reason: String,
}

pub fn run() -> Result<i32> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: Request = serde_json::from_str(&input).unwrap_or_default();
    if request.subject == "tool" {
        return run_tool(&request);
    }
    if request.command.contains("plugin-bad-json") {
        println!("not json");
        return Ok(0);
    }
    if request.command.contains("plugin-slow") {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let (verdict, reason) = if request.command.contains("block-prod") {
        ("deny", "production blocked by example plugin".to_string())
    } else if request.command.contains("prod") {
        ("ask", "production needs review".to_string())
    } else if request.command.contains("ticket=APPROVED") {
        ("allow", "approved ticket tag present".to_string())
    } else if request.command.contains("plugin-inspect") {
        // Echo the protocol-v2 structured data so an e2e test can confirm the
        // full per-fragment decomposition reached the plugin, not just the prose
        // `current_reason`. Returning `deny` is deliberate: a plugin deny always
        // takes effect when plugins run, so the echoed summary surfaces whatever
        // the base verdict was (allow, ask, or defer) — and lets one test observe
        // every per-fragment verdict that can reach a plugin.
        let summary = request
            .fragments
            .iter()
            .map(|fragment| {
                format!(
                    "{}|{}|{}|{}|{}",
                    fragment.role,
                    fragment.verdict,
                    fragment.rule.as_deref().unwrap_or("-"),
                    fragment.argv.join("+"),
                    fragment.reason,
                )
            })
            .collect::<Vec<_>>()
            .join(" ;; ");
        (
            "deny",
            format!(
                "v{} [{}]: {summary}",
                request.protocol_version,
                request.fragments.len()
            ),
        )
    } else {
        (
            "defer",
            "example plugin has no matching approval".to_string(),
        )
    };
    println!("{}", json!({ "verdict": verdict, "reason": reason }));
    Ok(0)
}

/// Handle a `subject: "tool"` request. Mirrors the shell path: on the
/// `tool-inspect` marker it echoes the structured tool object (capability, name,
/// canonical params) so an e2e test can confirm protocol-v2 tool data reached the
/// plugin; otherwise it defers.
fn run_tool(request: &Request) -> Result<i32> {
    let tool = request.tool.clone().unwrap_or_default();
    let saw = |marker: &str| {
        tool.name.contains(marker) || tool.params.values().any(|value| value.contains(marker))
    };
    if saw("tool-bad-json") {
        println!("not json");
        return Ok(0);
    }
    if saw("tool-slow") {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let (verdict, reason) = if saw("tool-deny") {
        ("deny", "tool blocked by example plugin".to_string())
    } else if saw("tool-ask") {
        ("ask", "tool needs review".to_string())
    } else if saw("tool-inspect") {
        // Echo the structured tool object so an e2e test can confirm protocol-v2
        // tool data reached the plugin.
        let params = tool
            .params
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(",");
        (
            "allow",
            format!(
                "tool v{} cap={} name={} params=[{params}]",
                request.protocol_version, tool.capability, tool.name
            ),
        )
    } else {
        (
            "defer",
            "example plugin has no matching tool approval".to_string(),
        )
    };
    println!("{}", json!({ "verdict": verdict, "reason": reason }));
    Ok(0)
}

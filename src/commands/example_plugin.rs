//! A tiny built-in example dynamic approval plugin.
//!
//! It is hidden from the normal CLI help but exercised by the e2e suite and
//! mirrors `examples/dynamic-approval-plugin.sh`: read a plugin request JSON from
//! stdin and return one verdict JSON on stdout.

use std::io::Read;

use serde::Deserialize;
use serde_json::json;

use crate::errors::Result;

#[derive(Debug, Default, Deserialize)]
struct Request {
    #[serde(default)]
    command: String,
    #[serde(default)]
    protocol_version: u8,
    #[serde(default)]
    fragments: Vec<RequestFragment>,
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
}

pub fn run() -> Result<i32> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: Request = serde_json::from_str(&input).unwrap_or_default();
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
        // per-fragment decomposition reached the plugin, not just the prose
        // `current_reason`.
        let summary = request
            .fragments
            .iter()
            .map(|fragment| {
                format!(
                    "{}/{}/{}",
                    fragment.role,
                    fragment.verdict,
                    fragment.rule.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        (
            "allow",
            format!(
                "v{} saw {} fragment(s): [{summary}]",
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

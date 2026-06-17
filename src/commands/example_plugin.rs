//! A tiny built-in example dynamic approval plugin.
//!
//! It is hidden from the normal CLI help but exercised by the e2e suite and
//! mirrors `examples/dynamic-approval-plugin.sh`: read a plugin request JSON from
//! stdin and return one verdict JSON on stdout.

use std::io::Read;

use serde::Deserialize;
use serde_json::json;

use crate::errors::Result;

#[derive(Debug, Deserialize)]
struct Request {
    command: String,
}

pub fn run() -> Result<i32> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: Request = serde_json::from_str(&input).unwrap_or_else(|_| Request {
        command: String::new(),
    });
    if request.command.contains("plugin-bad-json") {
        println!("not json");
        return Ok(0);
    }
    if request.command.contains("plugin-slow") {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let (verdict, reason) = if request.command.contains("block-prod") {
        ("deny", "production blocked by example plugin")
    } else if request.command.contains("prod") {
        ("ask", "production needs review")
    } else if request.command.contains("ticket=APPROVED") {
        ("allow", "approved ticket tag present")
    } else {
        ("defer", "example plugin has no matching approval")
    };
    println!("{}", json!({ "verdict": verdict, "reason": reason }));
    Ok(0)
}

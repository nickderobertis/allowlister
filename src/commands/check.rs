//! `allowlister check '<cmd>'` — evaluate one command and print its verdict.

use std::path::Path;

use serde::Serialize;

use crate::domain::Verdict;
use crate::errors::Result;
use crate::{config, domain};

use super::resolve_cwd;

#[derive(Serialize)]
struct CheckJson<'a> {
    verdict: &'a str,
    reason: &'a str,
}

/// Evaluate `command`. Returns exit code 2 for deny, 0 otherwise.
pub fn run(command: &str, cwd: Option<&Path>, json: bool) -> Result<i32> {
    let cwd = resolve_cwd(cwd);
    let loaded = config::load(&cwd);
    let result = domain::evaluate(command, &loaded.rules);

    if json {
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

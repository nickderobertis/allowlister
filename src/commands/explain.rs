//! `allowlister explain '<cmd>'` — verbose evaluation trace. The primary
//! debugging tool: it shows config sources, parse diagnostics, the fragment
//! table, and the per-fragment decision with the rule that matched.

use std::path::Path;

use crate::errors::Result;
use crate::{config, domain};

use super::resolve_cwd;

/// Print a full explanation. Always returns exit code 0.
pub fn run(command: &str, cwd: Option<&Path>) -> Result<i32> {
    let cwd = resolve_cwd(cwd);
    let loaded = config::load(&cwd);
    let analysis = domain::analyze(command);
    let result = domain::decide(&analysis, &loaded.rules);

    println!("command: {command}");
    println!();

    println!("config sources ({}):", loaded.sources.len());
    if loaded.sources.is_empty() {
        println!("  (none found)");
    }
    for source in &loaded.sources {
        println!("  - {source}");
    }
    println!("rules loaded: {}", loaded.rules.len());
    println!();

    let all_warnings: Vec<&String> = loaded
        .warnings
        .iter()
        .chain(analysis.warnings.iter())
        .collect();
    if !all_warnings.is_empty() {
        println!("warnings:");
        for warning in all_warnings {
            println!("  ! {warning}");
        }
        println!();
    }

    if !analysis.unsupported.is_empty() {
        println!("unsupported constructs:");
        for item in &analysis.unsupported {
            println!("  - {item}");
        }
        println!();
    }

    println!("fragments ({}):", analysis.fragments.len());
    for fragment in &analysis.fragments {
        println!("  [{}] {}", fragment.role.as_str(), fragment.cmd_string());
        for redirection in &fragment.redirections {
            println!("      redir: {}", redirection.display);
        }
    }
    println!();

    if !result.fragments.is_empty() {
        println!("decisions:");
        for decision in &result.fragments {
            let tag = decision.verdict.as_str().to_uppercase();
            println!(
                "  [{tag}] {}   <- {}",
                decision.fragment.cmd_string(),
                decision.reason
            );
        }
        println!();
    }

    println!("verdict: {}", result.verdict.as_str().to_uppercase());
    println!("reason:  {}", result.reason);

    // Explanation is informational; the exit code does not signal the verdict.
    Ok(0)
}

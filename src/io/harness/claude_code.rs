//! Claude Code `PreToolUse` hook adapter.
//!
//! Reads the hook JSON from stdin, evaluates the `Bash` command, and writes a
//! `PreToolUse` decision JSON on stdout. Exit code is always `0` for normal
//! operation; a malformed payload writes a stderr note and exits `1` (a
//! non-blocking error per the hook contract — the harness proceeds). The hook
//! never denies on a parse failure or internal error.

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config;
use crate::domain::{self, Verdict};
use crate::errors::Result;

/// Wire the adapter to the process's standard streams.
pub fn run() -> Result<i32> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    Ok(evaluate(stdin.lock(), stdout.lock(), stderr.lock()))
}

/// Run the adapter against explicit streams. Returns the process exit code.
/// Separated from [`run`] so the protocol can be exercised in-memory by tests.
pub fn evaluate<R: Read, W: Write, E: Write>(mut stdin: R, mut stdout: W, mut stderr: E) -> i32 {
    let mut buffer = String::new();
    if let Err(err) = stdin.read_to_string(&mut buffer) {
        let _ = writeln!(stderr, "allowlister: failed to read stdin: {err}");
        return 1;
    }

    let input: HookInput = match serde_json::from_str(&buffer) {
        Ok(input) => input,
        Err(err) => {
            // Fail open: never block the call on our own parse failure.
            let _ = writeln!(stderr, "allowlister: invalid hook JSON: {err}");
            return 1;
        }
    };

    if input.tool_name != "Bash" {
        write_decision(
            &mut stdout,
            "defer",
            &format!("allowlister: tool '{}' not handled", input.tool_name),
        );
        return 0;
    }

    let cwd = input.cwd.as_deref().unwrap_or(".");
    let loaded = config::load(Path::new(cwd));
    let result = domain::evaluate(&input.tool_input.command, &loaded.rules);

    let decision = match result.verdict {
        Verdict::Allow => "allow",
        Verdict::Deny => "deny",
        Verdict::Ask => "ask",
        Verdict::Defer => "defer",
    };
    write_decision(
        &mut stdout,
        decision,
        &format!("allowlister: {}", result.reason),
    );
    0
}

fn write_decision<W: Write>(stdout: &mut W, decision: &str, reason: &str) {
    let output = HookOutput {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PreToolUse",
            permission_decision: decision.to_string(),
            permission_decision_reason: reason.to_string(),
        },
    };
    // Serialization of this small fixed shape cannot fail; if writing fails the
    // harness will treat the empty output as "no decision" (defer), which is the
    // safe fallback.
    if let Ok(json) = serde_json::to_string(&output) {
        let _ = writeln!(stdout, "{json}");
    }
}

#[derive(Debug, Deserialize)]
struct HookInput {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    tool_input: ToolInput,
}

#[derive(Debug, Default, Deserialize)]
struct ToolInput {
    #[serde(default)]
    command: String,
}

#[derive(Debug, Serialize)]
struct HookOutput {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecificOutput,
}

#[derive(Debug, Serialize)]
struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'static str,
    #[serde(rename = "permissionDecision")]
    permission_decision: String,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn run_payload(payload: &str) -> (i32, Value) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = evaluate(payload.as_bytes(), &mut stdout, &mut stderr);
        let value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
        (code, value)
    }

    fn decision(value: &Value) -> &str {
        value["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or("")
    }

    #[test]
    fn invalid_json_exits_one_and_emits_nothing() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = evaluate(&b"{not json"[..], &mut stdout, &mut stderr);
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert!(!stderr.is_empty());
    }

    #[test]
    fn non_bash_tool_defers() {
        let (code, value) =
            run_payload(r#"{"hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{}}"#);
        assert_eq!(code, 0);
        assert_eq!(decision(&value), "defer");
    }

    #[test]
    fn unknown_command_defers() {
        let (code, value) = run_payload(
            r#"{"tool_name":"Bash","tool_input":{"command":"some_unknown_tool --x"},"cwd":"/tmp"}"#,
        );
        assert_eq!(code, 0);
        assert_eq!(decision(&value), "defer");
    }
}

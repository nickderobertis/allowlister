//! Claude Code `PreToolUse` hook adapter.
//!
//! Reads the hook JSON from stdin and writes a `PreToolUse` decision JSON on
//! stdout. A `Bash` call goes through the structural shell engine; every other
//! tool (built-in or `mcp__server__tool`) is normalized and gated by the
//! tool-rule engine. Exit code is always `0` for normal operation; a malformed
//! payload writes a stderr note and exits `1` (a non-blocking error per the hook
//! contract — the harness proceeds). The hook never denies on a parse failure or
//! internal error.

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{gate, normalize};
use crate::config;
use crate::domain::Verdict;
use crate::errors::Result;

/// Claude Code's shell tool. Everything else is a non-shell tool call.
const SHELL_TOOL: &str = "Bash";

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

    let cwd = input.cwd.as_deref().unwrap_or(".");
    let loaded = config::load(Path::new(cwd));

    // Bash keeps its structural path; every other tool is normalized and gated by
    // the tool-rule engine. An unrecognized tool with no matching rule defers —
    // exactly the behavior a non-shell tool had before tool gating existed.
    let result = if input.tool_name == SHELL_TOOL {
        let command = input
            .tool_input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        gate::evaluate_shell(
            &loaded,
            "claude-code",
            cwd,
            input.session_id.as_deref(),
            command,
        )
    } else {
        let call = normalize::claude(&input.tool_name, &input.tool_input);
        gate::evaluate_tool(
            &loaded,
            "claude-code",
            cwd,
            input.session_id.as_deref(),
            &call,
        )
    };

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
    /// The current session identifier, present on every Claude Code hook event
    /// and stable for the session. Threaded to plugins; absent in older payloads.
    #[serde(default)]
    session_id: Option<String>,
    /// The tool's input object, kept as raw JSON: the shell path reads
    /// `command`, while the tool path normalizes per-tool keys and matches any
    /// server-defined parameter by JSON path.
    #[serde(default)]
    tool_input: Value,
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
    use serde_json::{json, Value};
    use std::fs;
    use tempfile::TempDir;

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

    /// A project sandbox whose config gates the `Read` tool: allowed inside the
    /// repo, denied for `.ssh` paths anywhere.
    fn sandbox_with_read_rules() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let allow_glob = format!("{}/**", dir.path().to_string_lossy());
        let cfg = json!({
            "rules": [
                { "name": "reads in repo", "tool": "read", "action": "allow",
                  "params": { "path": [allow_glob] } },
                { "name": "no secrets", "tool": "read", "action": "deny",
                  "params": { "path": ["**/.ssh/**"] } }
            ]
        })
        .to_string();
        fs::write(dir.path().join(".allowlister.json"), cfg).unwrap();
        dir
    }

    fn read_payload(dir: &TempDir, file_path: &str) -> String {
        json!({
            "tool_name": "Read",
            "tool_input": { "file_path": file_path },
            "cwd": dir.path().to_string_lossy(),
        })
        .to_string()
    }

    #[test]
    fn read_tool_inside_repo_is_allowed() {
        let dir = sandbox_with_read_rules();
        let path = format!("{}/src/main.rs", dir.path().to_string_lossy());
        let (code, value) = run_payload(&read_payload(&dir, &path));
        assert_eq!(code, 0);
        assert_eq!(decision(&value), "allow");
    }

    #[test]
    fn read_tool_of_ssh_key_is_denied() {
        let dir = sandbox_with_read_rules();
        let (code, value) = run_payload(&read_payload(&dir, "/home/user/.ssh/id_rsa"));
        assert_eq!(code, 0);
        assert_eq!(decision(&value), "deny");
    }

    #[test]
    fn read_tool_outside_any_rule_defers() {
        let dir = sandbox_with_read_rules();
        let (code, value) = run_payload(&read_payload(&dir, "/etc/hosts"));
        assert_eq!(code, 0);
        assert_eq!(decision(&value), "defer");
    }

    #[test]
    fn ask_rule_maps_to_the_ask_decision() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let cfg = json!({
            "rules": [
                { "name": "confirm publish", "match": "npm publish*", "action": "ask" }
            ]
        })
        .to_string();
        fs::write(dir.path().join(".allowlister.json"), cfg).unwrap();
        let payload = json!({
            "tool_name": "Bash",
            "tool_input": { "command": "npm publish" },
            "cwd": dir.path().to_string_lossy(),
        })
        .to_string();
        let (code, value) = run_payload(&payload);
        assert_eq!(code, 0);
        assert_eq!(decision(&value), "ask");
    }
}

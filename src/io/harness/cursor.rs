//! Cursor hook adapter.
//!
//! Reads the hook JSON from stdin and writes a decision JSON on stdout. Exit code
//! is always `0` for normal operation; a malformed payload writes a stderr note
//! and exits `1` (Cursor treats any non-zero exit as a fail-open, so the agent
//! proceeds). The hook never denies on a parse failure or internal error.
//!
//! Cursor splits tool categories across separate events, so the adapter
//! dispatches on `hook_event_name`: `beforeShellExecution` carries the command at
//! the top level (the structural shell path), `beforeReadFile` carries a
//! `file_path` (gated as a `read` tool call), and `beforeMCPExecution` carries an
//! `mcp__server__tool` name plus arguments (gated as an MCP tool call). Cursor has
//! no pre-execution write/edit event, so writes/edits cannot be gated. Cursor
//! accepts `ask` for shell execution but not for `beforeReadFile`: file-read
//! defers must therefore emit `allow` so an unmatched read rule does not become
//! an invalid hook response that Cursor blocks.

use std::io::{Read, Write};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::{gate, normalize};
use crate::config;
use crate::domain::Verdict;
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

    let dir = discovery_dir(&input);
    let loaded = config::load(Path::new(dir));
    // Cursor splits tool categories across separate events. `beforeReadFile` and
    // `beforeMCPExecution` are normalized and gated by the tool-rule engine;
    // `beforeShellExecution` (and any unrecognized event) keeps the structural
    // shell path. Cursor has no pre-execution write/edit event, so writes/edits
    // are not gateable here.
    let event = input.hook_event_name.as_str();
    let result = match event {
        "beforeReadFile" => {
            let call = normalize::cursor_read(input.file_path.as_deref().unwrap_or_default());
            gate::evaluate_tool(&loaded, "cursor", dir, &call)
        }
        "beforeMCPExecution" => {
            let call = normalize::cursor_mcp(&input.tool_name, &input.tool_input);
            gate::evaluate_tool(&loaded, "cursor", dir, &call)
        }
        _ => gate::evaluate_shell(&loaded, "cursor", dir, &input.command),
    };

    let permission = cursor_permission(event, result.verdict);
    write_decision(
        &mut stdout,
        permission,
        &format!("allowlister: {}", result.reason),
    );
    0
}

/// The directory used for project-config discovery. Cursor often sends an empty
/// `cwd`, so fall back to the first workspace root, then the current directory.
fn discovery_dir(input: &HookInput) -> &str {
    if let Some(cwd) = input.cwd.as_deref() {
        if !cwd.is_empty() {
            return cwd;
        }
    }
    input
        .workspace_roots
        .iter()
        .map(String::as_str)
        .find(|root| !root.is_empty())
        .unwrap_or(".")
}

fn cursor_permission(event: &str, verdict: Verdict) -> &'static str {
    match (event, verdict) {
        (_, Verdict::Allow) => "allow",
        (_, Verdict::Deny) => "deny",
        // Cursor rejects `ask` as an invalid `beforeReadFile` response. For a
        // deferred read, emit `allow` to preserve the user's pre-hook behavior:
        // no matching allowlister rule means allowlister makes no decision.
        ("beforeReadFile", Verdict::Defer) => "allow",
        // `ask` cannot be represented for reads; deny is the conservative
        // blocking equivalent for an explicit ask rule.
        ("beforeReadFile", Verdict::Ask) => "deny",
        // Shell execution supports `ask`, and Cursor has no true defer token, so
        // an undecided shell/MCP call escalates to the user.
        (_, Verdict::Ask | Verdict::Defer) => "ask",
    }
}

fn write_decision<W: Write>(stdout: &mut W, permission: &str, message: &str) {
    // Cursor's hook schema includes `continue` alongside `permission`; newer
    // Cursor builds can treat a permission-only object as an invalid hook
    // response for some read-style hook steps (notably terminal/AwaitShell output
    // reads). Always emit the complete documented envelope. Carry the reason under
    // both `agentMessage` (older published hook types, camelCase) and
    // `agent_message` (current docs, snake_case): Cursor ignores unknown keys. The
    // message surfaces to the agent on `ask`; on `deny` Cursor may substitute its
    // own generic "blocked by a hook" text. If writing fails Cursor treats the
    // missing output as a fail-open (the command proceeds), which is the safe
    // fallback.
    let output = serde_json::json!({
        "continue": true,
        "permission": permission,
        "agentMessage": message,
        "agent_message": message,
    });
    let _ = writeln!(stdout, "{output}");
}

#[derive(Debug, Default, Deserialize)]
struct HookInput {
    /// Which Cursor event this is; selects how the payload is interpreted.
    #[serde(default)]
    hook_event_name: String,
    /// `beforeShellExecution`: the shell command, at the top level.
    #[serde(default)]
    command: String,
    /// `beforeReadFile`: the file being read.
    #[serde(default)]
    file_path: Option<String>,
    /// `beforeMCPExecution`: the MCP tool name (`mcp__server__tool`) and its
    /// arguments (an object, or a JSON string per Cursor's docs).
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    workspace_roots: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use tempfile::TempDir;

    fn run_payload(payload: &str) -> (i32, Value) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = evaluate(payload.as_bytes(), &mut stdout, &mut stderr);
        let value = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
        (code, value)
    }

    fn permission(value: &Value) -> &str {
        value["permission"].as_str().unwrap_or("")
    }

    /// A project sandbox with a `.git` boundary and a single deny rule, so
    /// discovery finds it from `cwd` or `workspace_roots`.
    fn sandbox_with_deny() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".allowlister.json"),
            r#"{"rules":[{"name":"deny touch","match":"touch *","action":"deny"}]}"#,
        )
        .unwrap();
        dir
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
    fn missing_command_field_defaults_empty_and_does_not_panic() {
        let (code, value) = run_payload(r#"{"cwd":"/tmp"}"#);
        assert_eq!(code, 0);
        // A missing command serde-defaults to empty; an empty command has no
        // fragments, so it is vacuously allowed (and is a harmless no-op anyway).
        assert_eq!(permission(&value), "allow");
    }

    #[test]
    fn unknown_command_maps_defer_to_ask() {
        let (code, value) = run_payload(r#"{"command":"some_unknown_tool --x","cwd":"/tmp"}"#);
        assert_eq!(code, 0);
        assert_eq!(permission(&value), "ask");
    }

    #[test]
    fn empty_cwd_falls_back_to_workspace_root() {
        let dir = sandbox_with_deny();
        let payload = format!(
            r#"{{"command":"touch /tmp/x","cwd":"","workspace_roots":[{}]}}"#,
            serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap()
        );
        let (code, value) = run_payload(&payload);
        assert_eq!(code, 0);
        assert_eq!(
            permission(&value),
            "deny",
            "the workspace-root project config must be consulted"
        );
    }

    #[test]
    fn empty_cwd_and_no_roots_uses_dot_without_panic() {
        let (code, value) =
            run_payload(r#"{"command":"some_unknown_tool","cwd":"","workspace_roots":[]}"#);
        assert_eq!(code, 0);
        assert_eq!(permission(&value), "ask");
    }

    #[test]
    fn denied_command_maps_to_deny() {
        let dir = sandbox_with_deny();
        let payload = format!(
            r#"{{"command":"touch /tmp/x","cwd":{}}}"#,
            serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap()
        );
        let (code, value) = run_payload(&payload);
        assert_eq!(code, 0);
        assert_eq!(permission(&value), "deny");
        // The reason is carried under both casings so whichever key Cursor reads
        // delivers it.
        for key in ["agentMessage", "agent_message"] {
            assert!(
                value[key]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("allowlister:"),
                "missing reason under {key}"
            );
        }
    }

    /// A project sandbox whose tool rule denies reading `.ssh` paths.
    fn sandbox_with_read_deny() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".allowlister.json"),
            r#"{"rules":[{"name":"no ssh","tool":"read","action":"deny","params":{"path":["**/.ssh/**"]}}]}"#,
        )
        .unwrap();
        dir
    }

    fn root(dir: &TempDir) -> String {
        serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap()
    }

    #[test]
    fn before_read_file_event_denies_secret() {
        let dir = sandbox_with_read_deny();
        let payload = format!(
            r#"{{"hook_event_name":"beforeReadFile","file_path":"/home/u/.ssh/id_rsa","workspace_roots":[{}]}}"#,
            root(&dir)
        );
        let (code, value) = run_payload(&payload);
        assert_eq!(code, 0);
        assert_eq!(permission(&value), "deny");
    }

    #[test]
    fn before_read_file_event_outside_rule_maps_defer_to_allow() {
        let dir = sandbox_with_read_deny();
        let payload = format!(
            r#"{{"hook_event_name":"beforeReadFile","file_path":"/repo/a.txt","workspace_roots":[{}]}}"#,
            root(&dir)
        );
        let (code, value) = run_payload(&payload);
        assert_eq!(code, 0);
        // Cursor rejects `ask` for beforeReadFile; an unmatched read must fall
        // through by allowing the read instead of producing an invalid response.
        assert_eq!(permission(&value), "allow");
    }

    #[test]
    fn before_mcp_execution_event_denies_destructive() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".allowlister.json"),
            r#"{"rules":[{"name":"no destroy","tool":"mcp","action":"deny","params":{"mcp_tool":["delete*"]}}]}"#,
        )
        .unwrap();
        let payload = format!(
            r#"{{"hook_event_name":"beforeMCPExecution","tool_name":"mcp__linear__delete_issue","tool_input":{{}},"workspace_roots":[{}]}}"#,
            root(&dir)
        );
        let (code, value) = run_payload(&payload);
        assert_eq!(code, 0);
        assert_eq!(permission(&value), "deny");
    }
}

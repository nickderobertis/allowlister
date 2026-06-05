//! Cursor `beforeShellExecution` hook adapter.
//!
//! Reads the hook JSON from stdin, evaluates the shell command, and writes a
//! decision JSON on stdout. Exit code is always `0` for normal operation; a
//! malformed payload writes a stderr note and exits `1` (Cursor treats any
//! non-zero exit as a fail-open, so the agent proceeds). The hook never denies on
//! a parse failure or internal error.
//!
//! Unlike Claude Code's `PreToolUse`, the event carries the command at the top
//! level with no `tool_name`, so there is no non-shell branch — the adapter
//! always evaluates. Cursor has no "defer" permission, so a deferred verdict maps
//! to `ask` (its safest escalation: surface to the user), never to `allow`.

use std::io::{Read, Write};
use std::path::Path;

use serde::Deserialize;

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

    let dir = discovery_dir(&input);
    let loaded = config::load(Path::new(dir));
    let result = domain::evaluate(&input.command, &loaded.rules);

    let permission = match result.verdict {
        Verdict::Allow => "allow",
        Verdict::Deny => "deny",
        // Cursor has no "defer": escalate an undecided command to the user.
        Verdict::Ask | Verdict::Defer => "ask",
    };
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

fn write_decision<W: Write>(stdout: &mut W, permission: &str, message: &str) {
    // Carry the reason under both `agentMessage` (Cursor's published hook types,
    // camelCase) and `agent_message` (Cursor's hooks docs, snake_case): the two
    // disagree and we cannot tell which a given Cursor build reads, so emit both
    // (Cursor ignores unknown keys). The message surfaces to the agent on `ask`;
    // on `deny` Cursor substitutes its own generic "blocked by a hook" text and
    // drops ours, which a hook cannot override. `permission` is the field that
    // gates and is unambiguous. If writing fails Cursor treats the missing output
    // as a fail-open (the command proceeds), which is the safe fallback.
    let output = serde_json::json!({
        "permission": permission,
        "agentMessage": message,
        "agent_message": message,
    });
    let _ = writeln!(stdout, "{output}");
}

#[derive(Debug, Default, Deserialize)]
struct HookInput {
    #[serde(default)]
    command: String,
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
}

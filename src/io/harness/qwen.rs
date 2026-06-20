//! Qwen Code `PreToolUse` hook adapter.
//!
//! Reads the hook JSON from stdin, evaluates the shell command, and — only when
//! the verdict is **deny** — writes a `PreToolUse` decision on stdout. Qwen Code
//! is a fork of Gemini CLI but carries a Claude-Code-style hook schema, so the
//! output shares Claude Code's field names (`permissionDecision` /
//! `permissionDecisionReason`). Three protocol facts shape the rest:
//!
//! 1. **`PreToolUse` fires for every tool call, in every approval mode** —
//!    including `--yolo`/`--approval-mode yolo`. The hook runs after scheduling
//!    but before the tool executes, so a `deny` here is authoritative even when
//!    the agent auto-approves everything unattended.
//! 2. **A non-deny verdict emits nothing.** Qwen *does* accept a bare
//!    `permissionDecision:"allow"`, but that auto-approves the command and
//!    short-circuits its later confirmation. An allowlister gate only wants to
//!    block, so an allow or defer verdict emits *nothing*: empty stdout is the
//!    conservative "no objection" that lets Qwen's own approval flow continue,
//!    and a `deny` is the only thing we assert.
//! 3. **Exit code is always `0`.** Qwen treats exit `2` (with a stderr reason) as
//!    a block and fails *open* on any error, timeout, or empty non-zero exit, so
//!    our own read/parse failure must never exit `2`. A deny is expressed only as
//!    JSON; on any internal failure we exit `0` with empty stdout, a no-op that
//!    lets the call through (fail open).
//!
//! The command arrives under `tool_input.command`. Qwen names its shell tool
//! `run_shell_command` (Gemini-style), not `Bash`; any other tool emits nothing.

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{gate, normalize};
use crate::config;
use crate::domain::Verdict;
use crate::errors::Result;

/// The canonical tool name Qwen Code uses for shell commands. Any other tool is
/// not one we gate, so it emits nothing.
const SHELL_TOOL: &str = "run_shell_command";

/// Wire the adapter to the process's standard streams.
pub fn run() -> Result<i32> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    Ok(evaluate(stdin.lock(), stdout.lock(), stderr.lock()))
}

/// Run the adapter against explicit streams. Returns the process exit code, which
/// is **always `0`**: a deny is expressed only as JSON, never via the exit code,
/// so our own failures cannot become a Qwen block (Qwen treats exit `2` as a
/// block and fails open otherwise). Separated from [`run`] so the protocol can be
/// exercised in-memory by tests.
pub fn evaluate<R: Read, W: Write, E: Write>(mut stdin: R, mut stdout: W, mut stderr: E) -> i32 {
    let mut buffer = String::new();
    if let Err(err) = stdin.read_to_string(&mut buffer) {
        // Never deny on our own failure: empty stdout + exit 0 lets Qwen run its
        // normal approval flow (fail open).
        let _ = writeln!(stderr, "allowlister: failed to read stdin: {err}");
        return 0;
    }

    let input: HookInput = match serde_json::from_str(&buffer) {
        Ok(input) => input,
        Err(err) => {
            // Fail open: a parse failure is a no-op (empty stdout), never a deny.
            let _ = writeln!(stderr, "allowlister: invalid hook JSON: {err}");
            return 0;
        }
    };

    let dir = discovery_dir(&input);
    let loaded = config::load(Path::new(dir));
    // The shell tool keeps its structural path; every other tool is normalized
    // and gated by the tool-rule engine. An unrecognized tool with no matching
    // rule defers, emitting nothing — exactly the prior non-shell behavior.
    let result = if input.tool_name == SHELL_TOOL {
        let command = command_from(&input.tool_input);
        gate::evaluate_shell(&loaded, "qwen", dir, input.session_id.as_deref(), &command)
    } else {
        let call = normalize::qwen(&input.tool_name, &input.tool_input);
        gate::evaluate_tool(&loaded, "qwen", dir, input.session_id.as_deref(), &call)
    };

    // Only `deny` is asserted. An allow or defer verdict emits nothing — a true
    // fall-through to Qwen's own approval flow — because an explicit `allow` would
    // auto-approve and short-circuit confirmation, which an allowlister gate must
    // not do.
    if matches!(result.verdict, Verdict::Deny) {
        write_deny(&mut stdout, &format!("allowlister: {}", result.reason));
    }
    0
}

/// The directory used for project-config discovery. Qwen sends the session `cwd`;
/// fall back to the current directory if it is missing or empty.
fn discovery_dir(input: &HookInput) -> &str {
    input
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or(".")
}

/// Extract the shell command from `tool_input`. Qwen sends it as a JSON object
/// (`{"command": "..."}`); any other shape yields an empty command, which the
/// engine vacuously allows (a no-op).
fn command_from(tool_input: &Value) -> String {
    tool_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Write a `PreToolUse` deny. The reason is required: Qwen surfaces
/// `permissionDecisionReason` to the model as the block reason, and we always
/// supply one.
fn write_deny<W: Write>(stdout: &mut W, reason: &str) {
    let output = HookOutput {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PreToolUse",
            permission_decision: "deny",
            permission_decision_reason: reason.to_string(),
        },
    };
    // This small fixed shape cannot fail to serialize; if the write fails Qwen
    // sees empty stdout and falls through to its normal approval flow — never a
    // deny on our error.
    if let Ok(json) = serde_json::to_string(&output) {
        let _ = writeln!(stdout, "{json}");
    }
}

#[derive(Debug, Default, Deserialize)]
struct HookInput {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    cwd: Option<String>,
    /// Qwen's per-session id, a common field on every hook payload (Claude-style
    /// schema). Threaded to plugins.
    #[serde(default)]
    session_id: Option<String>,
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
    permission_decision: &'static str,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use tempfile::TempDir;

    /// Run the adapter and return `(exit_code, raw_stdout)`. Allow/defer emit empty
    /// stdout, so callers that expect JSON parse it themselves.
    fn run_payload(payload: &str) -> (i32, Vec<u8>) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = evaluate(payload.as_bytes(), &mut stdout, &mut stderr);
        (code, stdout)
    }

    fn decision(stdout: &[u8]) -> String {
        let value: Value = serde_json::from_slice(stdout).unwrap_or(Value::Null);
        value["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// A project sandbox with a `.git` boundary and a single deny rule, so
    /// discovery finds it from `cwd`.
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

    /// A `PreToolUse` payload for the `run_shell_command` tool, command under
    /// `tool_input.command`.
    fn payload(command: &str, cwd: &Path) -> String {
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"run_shell_command","tool_input":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&cwd.to_string_lossy().into_owned()).unwrap()
        )
    }

    #[test]
    fn invalid_json_defers_via_exit_zero_and_empty_stdout() {
        // The fail-open path: a parse error must NOT exit 2 (which Qwen would
        // treat as a block). It exits 0 with empty stdout, a no-op.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = evaluate(&b"{not json"[..], &mut stdout, &mut stderr);
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(!stderr.is_empty());
    }

    #[test]
    fn non_shell_tool_emits_nothing() {
        let (code, stdout) =
            run_payload(r#"{"tool_name":"read_file","tool_input":{"command":"x"}}"#);
        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "a non-shell tool must emit nothing");
    }

    #[test]
    fn unknown_command_defers_with_empty_stdout() {
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&payload("some_unknown_tool --x", dir.path()));
        assert_eq!(code, 0);
        // Qwen falls through on empty stdout, so a defer emits nothing.
        assert!(stdout.is_empty(), "an undecided command must emit nothing");
    }

    #[test]
    fn allowed_command_emits_nothing() {
        // An allow verdict is a no-op (empty stdout): an explicit `allow` would
        // auto-approve and short-circuit Qwen's confirmation, which a gate must not.
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".allowlister.json"),
            r#"{"rules":[{"name":"allow echo","match":"echo *","action":"allow"}]}"#,
        )
        .unwrap();
        let (code, stdout) = run_payload(&payload("echo hi", dir.path()));
        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "an allow verdict must emit nothing");
    }

    #[test]
    fn denied_command_maps_to_deny() {
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&payload("touch /tmp/x", dir.path()));
        assert_eq!(code, 0);
        assert_eq!(decision(&stdout), "deny");
        let value: Value = serde_json::from_slice(&stdout).unwrap();
        assert!(value["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .starts_with("allowlister:"));
    }

    #[test]
    fn missing_command_field_defaults_empty_and_does_not_panic() {
        let dir = TempDir::new().unwrap();
        let (code, stdout) = run_payload(&format!(
            r#"{{"tool_name":"run_shell_command","tool_input":{{}},"cwd":{}}}"#,
            serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap()
        ));
        assert_eq!(code, 0);
        // A missing command is empty → vacuously allowed → no-op (empty stdout).
        assert!(stdout.is_empty());
    }

    #[test]
    fn empty_cwd_falls_back_to_dot_without_panic() {
        let (code, stdout) = run_payload(
            r#"{"tool_name":"run_shell_command","tool_input":{"command":"some_unknown_tool"},"cwd":""}"#,
        );
        assert_eq!(code, 0);
        // No project config under `.` in the test's cwd, so it defers (empty).
        assert!(stdout.is_empty());
    }
}

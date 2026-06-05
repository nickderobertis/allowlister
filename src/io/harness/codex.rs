//! OpenAI Codex CLI `PreToolUse` hook adapter.
//!
//! Reads the hook JSON from stdin, evaluates the `Bash` command, and — only when
//! the verdict is **deny** — writes a `PreToolUse` decision on stdout. Its output
//! shares Claude Code's field names (`permissionDecision` /
//! `permissionDecisionReason`); three protocol facts shape the rest:
//!
//! 1. **`PreToolUse` fires for every tool call, in every approval mode** —
//!    including `--ask-for-approval never` and the bypass modes. So a `deny` here
//!    is authoritative even when the agent runs unattended. That is why this
//!    adapter gates on `PreToolUse` and not `PermissionRequest`: the latter only
//!    fires when an approval prompt would otherwise appear, so it cannot block in
//!    a bypass run.
//! 2. **Only `deny` is honored.** Codex rejects a bare `permissionDecision:"allow"`
//!    (and `"ask"`) as unsupported — `allow` is accepted only paired with an
//!    `updatedInput` command rewrite, which we never do. So an allow or defer
//!    verdict emits *nothing*: empty stdout is a true fall-through to Codex's
//!    normal approval flow, and a `deny` is the only thing we ever assert.
//! 3. **Exit code is always `0`.** Codex treats exit `2` with a stderr message as
//!    a block, so our own read/parse failure must never exit `2` (or it would deny
//!    on our error). A deny is expressed only as JSON; on any internal failure we
//!    exit `0` with empty stdout, a no-op that lets the call through (fail open).
//!
//! The command arrives under `tool_input.command`. Only the `Bash` tool is
//! evaluated — any other tool emits nothing.

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config;
use crate::domain::{self, Verdict};
use crate::errors::Result;

/// The canonical tool name Codex uses for shell commands. Any other tool is not
/// one we gate, so it emits nothing.
const SHELL_TOOL: &str = "Bash";

/// Wire the adapter to the process's standard streams.
pub fn run() -> Result<i32> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    Ok(evaluate(stdin.lock(), stdout.lock(), stderr.lock()))
}

/// Run the adapter against explicit streams. Returns the process exit code, which
/// is **always `0`**: a deny is expressed only as JSON, never via the exit code,
/// so our own failures cannot become a Codex block. Separated from [`run`] so the
/// protocol can be exercised in-memory by tests.
pub fn evaluate<R: Read, W: Write, E: Write>(mut stdin: R, mut stdout: W, mut stderr: E) -> i32 {
    let mut buffer = String::new();
    if let Err(err) = stdin.read_to_string(&mut buffer) {
        // Never deny on our own failure: empty stdout + exit 0 lets Codex run its
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

    if input.tool_name != SHELL_TOOL {
        // Not a shell command — let Codex's normal flow handle it (emit nothing).
        return 0;
    }

    let dir = discovery_dir(&input);
    let loaded = config::load(Path::new(dir));
    let command = command_from(&input.tool_input);
    let result = domain::evaluate(&command, &loaded.rules);

    // Codex honors only `deny` on `PreToolUse`. An allow or defer verdict emits
    // nothing — a true fall-through to Codex's own approval flow — because a bare
    // `allow` is rejected as unsupported and would only log an error.
    if matches!(result.verdict, Verdict::Deny) {
        write_deny(&mut stdout, &format!("allowlister: {}", result.reason));
    }
    0
}

/// The directory used for project-config discovery. Codex sends the session `cwd`;
/// fall back to the current directory if it is missing or empty.
fn discovery_dir(input: &HookInput) -> &str {
    input
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or(".")
}

/// Extract the shell command from `tool_input`. Codex sends it as a JSON object
/// (`{"command": "..."}`); any other shape yields an empty command, which the
/// engine vacuously allows (a no-op).
fn command_from(tool_input: &Value) -> String {
    tool_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Write a `PreToolUse` deny. The reason is required: Codex ignores a `deny` whose
/// `permissionDecisionReason` is empty, and we always supply one.
fn write_deny<W: Write>(stdout: &mut W, reason: &str) {
    let output = HookOutput {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "PreToolUse",
            permission_decision: "deny",
            permission_decision_reason: reason.to_string(),
        },
    };
    // This small fixed shape cannot fail to serialize; if the write fails Codex
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

    /// A `PreToolUse` payload for the `Bash` tool with `tool_input` as an object.
    fn payload(command: &str, cwd: &Path) -> String {
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&cwd.to_string_lossy().into_owned()).unwrap()
        )
    }

    #[test]
    fn invalid_json_defers_via_exit_zero_and_empty_stdout() {
        // The fail-open path: a parse error must NOT exit 2 (which Codex would
        // treat as a block). It exits 0 with empty stdout, a no-op.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = evaluate(&b"{not json"[..], &mut stdout, &mut stderr);
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(!stderr.is_empty());
    }

    #[test]
    fn non_bash_tool_emits_nothing() {
        let (code, stdout) =
            run_payload(r#"{"tool_name":"apply_patch","tool_input":{"command":"x"}}"#);
        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "a non-shell tool must emit nothing");
    }

    #[test]
    fn unknown_command_defers_with_empty_stdout() {
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&payload("some_unknown_tool --x", dir.path()));
        assert_eq!(code, 0);
        // Codex falls through on empty stdout, so a defer emits nothing.
        assert!(stdout.is_empty(), "an undecided command must emit nothing");
    }

    #[test]
    fn allowed_command_emits_nothing() {
        // The Codex divergence from Claude Code: a bare `allow` is unsupported, so
        // an allow verdict is a no-op (empty stdout), not an `allow` decision.
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
            r#"{{"tool_name":"Bash","tool_input":{{}},"cwd":{}}}"#,
            serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap()
        ));
        assert_eq!(code, 0);
        // A missing command is empty → vacuously allowed → no-op (empty stdout).
        assert!(stdout.is_empty());
    }

    #[test]
    fn empty_cwd_falls_back_to_dot_without_panic() {
        let (code, stdout) = run_payload(
            r#"{"tool_name":"Bash","tool_input":{"command":"some_unknown_tool"},"cwd":""}"#,
        );
        assert_eq!(code, 0);
        // No project config under `.` in the test's cwd, so it defers (empty).
        assert!(stdout.is_empty());
    }
}

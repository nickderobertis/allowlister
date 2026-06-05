//! GitHub Copilot CLI `preToolUse` hook adapter.
//!
//! Reads the hook JSON from stdin, evaluates the shell command, and writes a
//! `preToolUse` decision JSON on stdout. Its output shares Claude Code's field
//! names (`permissionDecision` / `permissionDecisionReason`); the protocol
//! differences are two:
//!
//! 1. **Exit code is always `0`.** Copilot's `preToolUse` hook is *fail-closed*:
//!    a non-zero exit (or a crash, or a timeout) denies the tool call. So unlike
//!    the Claude Code and Cursor adapters — where a non-zero exit fails *open* —
//!    this adapter must never exit non-zero, or our own stdin/parse failure would
//!    silently deny. A deny is expressed only as `"permissionDecision":"deny"`
//!    JSON, never via the exit code. On any internal failure we exit `0` with
//!    empty stdout, which Copilot treats as "no decision" and falls through to
//!    its normal permission flow — never a deny on our error.
//! 2. **Defer is a true fall-through.** Copilot has no defer token, but empty
//!    stdout *is* the fall-through: Copilot runs its normal permission handling
//!    (rules, session approvals, the user prompt). So a deferred verdict emits
//!    nothing — a genuine "let the harness decide", not an escalation to `ask`
//!    the way Cursor's no-defer protocol forces.
//!
//! The event carries the command under `toolArgs`, which Copilot may send either
//! as a JSON object (`{"command": "..."}`) or as a stringified JSON value; both
//! shapes are handled. Only the `bash` tool is evaluated — any other tool defers.

use std::io::{Read, Write};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::config;
use crate::domain::{self, Verdict};
use crate::errors::Result;

/// The tool name Copilot uses for shell commands. Any other tool is not a shell
/// command we gate, so it defers.
const SHELL_TOOL: &str = "bash";

/// Wire the adapter to the process's standard streams.
pub fn run() -> Result<i32> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    Ok(evaluate(stdin.lock(), stdout.lock(), stderr.lock()))
}

/// Run the adapter against explicit streams. Returns the process exit code, which
/// is **always `0`**: Copilot's `preToolUse` is fail-closed, so any non-zero exit
/// would deny the call. Separated from [`run`] so the protocol can be exercised
/// in-memory by tests.
pub fn evaluate<R: Read, W: Write, E: Write>(mut stdin: R, mut stdout: W, mut stderr: E) -> i32 {
    let mut buffer = String::new();
    if let Err(err) = stdin.read_to_string(&mut buffer) {
        // Never deny on our own failure: empty stdout + exit 0 falls through to
        // Copilot's normal permission flow (its fail-open path).
        let _ = writeln!(stderr, "allowlister: failed to read stdin: {err}");
        return 0;
    }

    let input: HookInput = match serde_json::from_str(&buffer) {
        Ok(input) => input,
        Err(err) => {
            // Fail open: a parse failure defers (empty stdout), never denies.
            let _ = writeln!(stderr, "allowlister: invalid hook JSON: {err}");
            return 0;
        }
    };

    if input.tool_name != SHELL_TOOL {
        // Not a shell command — defer to Copilot's normal flow (emit nothing).
        return 0;
    }

    let dir = discovery_dir(&input);
    let loaded = config::load(Path::new(dir));
    let command = command_from(&input.tool_args);
    let result = domain::evaluate(&command, &loaded.rules);

    if matches!(result.verdict, Verdict::Defer) {
        // Emit nothing: Copilot's native fall-through runs its normal permission
        // handling. A true defer, not an escalation to `ask`.
        return 0;
    }
    // `as_str` yields exactly Copilot's permission values for Allow/Deny/Ask.
    write_decision(
        &mut stdout,
        result.verdict.as_str(),
        &format!("allowlister: {}", result.reason),
    );
    0
}

/// The directory used for project-config discovery. Copilot sends the repository
/// path in `cwd`; fall back to the current directory if it is missing or empty.
fn discovery_dir(input: &HookInput) -> &str {
    input
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or(".")
}

/// Extract the shell command from `toolArgs`. Copilot sends it either as a JSON
/// object (`{"command": "..."}`) or as a stringified JSON value that must be
/// parsed again; both are handled. Anything else yields an empty command, which
/// the engine vacuously allows (a no-op).
fn command_from(tool_args: &Value) -> String {
    let command = |obj: &Value| {
        obj.get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match tool_args {
        Value::Object(_) => command(tool_args),
        Value::String(raw) => serde_json::from_str::<Value>(raw)
            .ok()
            .as_ref()
            .map(command)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn write_decision<W: Write>(stdout: &mut W, decision: &str, reason: &str) {
    // `permissionDecision` is the field that gates; `permissionDecisionReason` is
    // fed back to the model. This small fixed shape cannot fail to serialize; if
    // the write fails Copilot sees empty stdout and falls through to its normal
    // permission flow, which is the safe fallback (never a deny on our error).
    let output = serde_json::json!({
        "permissionDecision": decision,
        "permissionDecisionReason": reason,
    });
    let _ = writeln!(stdout, "{output}");
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookInput {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    tool_args: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use tempfile::TempDir;

    /// Run the adapter and return `(exit_code, raw_stdout)`. Defer emits empty
    /// stdout, so callers that expect JSON parse it themselves.
    fn run_payload(payload: &str) -> (i32, Vec<u8>) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = evaluate(payload.as_bytes(), &mut stdout, &mut stderr);
        (code, stdout)
    }

    fn decision(stdout: &[u8]) -> String {
        let value: Value = serde_json::from_slice(stdout).unwrap_or(Value::Null);
        value["permissionDecision"]
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

    /// A `preToolUse` payload for the `bash` tool with `toolArgs` as an object.
    fn payload(command: &str, cwd: &Path) -> String {
        format!(
            r#"{{"toolName":"bash","toolArgs":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&cwd.to_string_lossy().into_owned()).unwrap()
        )
    }

    #[test]
    fn invalid_json_defers_via_exit_zero_and_empty_stdout() {
        // The fail-closed inversion: a parse error must NOT exit non-zero (that
        // would deny). It exits 0 with empty stdout so Copilot falls through.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = evaluate(&b"{not json"[..], &mut stdout, &mut stderr);
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(!stderr.is_empty());
    }

    #[test]
    fn non_bash_tool_defers_with_empty_stdout() {
        let (code, stdout) = run_payload(r#"{"toolName":"read","toolArgs":{"path":"/etc/hosts"}}"#);
        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "a non-shell tool must defer (no output)");
    }

    #[test]
    fn unknown_command_defers_with_empty_stdout() {
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&payload("some_unknown_tool --x", dir.path()));
        assert_eq!(code, 0);
        // Copilot has a native fall-through, so defer emits nothing (a true
        // defer), unlike Cursor which has to escalate to `ask`.
        assert!(stdout.is_empty(), "an undecided command must emit nothing");
    }

    #[test]
    fn denied_command_maps_to_deny() {
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&payload("touch /tmp/x", dir.path()));
        assert_eq!(code, 0);
        assert_eq!(decision(&stdout), "deny");
        let value: Value = serde_json::from_slice(&stdout).unwrap();
        assert!(value["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .starts_with("allowlister:"));
    }

    #[test]
    fn allowed_command_maps_to_allow() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".allowlister.json"),
            r#"{"rules":[{"name":"allow echo","match":"echo *","action":"allow"}]}"#,
        )
        .unwrap();
        let (code, stdout) = run_payload(&payload("echo hi", dir.path()));
        assert_eq!(code, 0);
        assert_eq!(decision(&stdout), "allow");
    }

    #[test]
    fn tool_args_as_stringified_json_is_parsed() {
        // Some Copilot builds double-encode `toolArgs` as a JSON string; the
        // command must still be extracted from it.
        let dir = sandbox_with_deny();
        let cwd = serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap();
        let payload = format!(
            r#"{{"toolName":"bash","toolArgs":"{{\"command\":\"touch /tmp/x\"}}","cwd":{cwd}}}"#
        );
        let (code, stdout) = run_payload(&payload);
        assert_eq!(code, 0);
        assert_eq!(decision(&stdout), "deny");
    }

    #[test]
    fn missing_command_field_defaults_empty_and_does_not_panic() {
        let dir = TempDir::new().unwrap();
        let (code, stdout) = run_payload(&format!(
            r#"{{"toolName":"bash","toolArgs":{{}},"cwd":{}}}"#,
            serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap()
        ));
        assert_eq!(code, 0);
        // A missing command serde-defaults to empty; an empty command has no
        // fragments, so it is vacuously allowed (and is a harmless no-op anyway).
        assert_eq!(decision(&stdout), "allow");
    }

    #[test]
    fn empty_cwd_falls_back_to_dot_without_panic() {
        let (code, stdout) = run_payload(
            r#"{"toolName":"bash","toolArgs":{"command":"some_unknown_tool"},"cwd":""}"#,
        );
        assert_eq!(code, 0);
        // No project config under `.` in the test's cwd, so it defers (empty).
        assert!(stdout.is_empty());
    }
}

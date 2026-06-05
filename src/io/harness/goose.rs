//! Goose `PreToolUse` hook adapter.
//!
//! Reads the hook JSON from stdin, evaluates the shell command, and — only when
//! the verdict is **deny** — writes Goose's native block decision on stdout
//! (`{"decision":"block","reason":"…"}`). Three protocol facts shape the rest:
//!
//! 1. **`PreToolUse` is Goose's only *blocking* tool event, and it fires in every
//!    approval mode** — including `GOOSE_MODE=auto`, the headless default. It runs
//!    at the single tool-dispatch chokepoint before the tool executes, so a block
//!    here is authoritative even when the agent auto-approves everything.
//!    (`BeforeShellExecution` also exists but is observational — its verdict is
//!    discarded — so we gate on `PreToolUse`.)
//! 2. **The block keyword is `block`, not `deny`.** Goose recognizes a deliberate
//!    block only as `{"decision":"block"}` (or exit `2` with a stderr reason); a
//!    `"deny"` decision is ignored. A non-block verdict emits *nothing* — empty
//!    stdout is a true fall-through to Goose's normal flow.
//! 3. **Exit code is always `0`.** Goose treats exit `2` (with a stderr reason) as
//!    a block and fails *open* on every other outcome — any other exit code,
//!    crash, or timeout lets the command proceed. So a block must travel as JSON,
//!    never as an exit code: always exiting `0` means our own read/parse failure
//!    is a no-op that lets the call through, and an internal error can neither
//!    become a block nor be mistaken for one.
//!
//! The command arrives under `tool_input.command`; the working directory is the
//! top-level `working_dir`. Goose's shell tool is the developer extension's
//! shell, exposed as a bare `shell` (builtin) or `developer__shell` (namespaced);
//! any other tool emits nothing.

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::normalize;
use crate::config;
use crate::domain::{self, Verdict};
use crate::errors::Result;

/// True for Goose's shell tool. The developer extension exposes it as a bare
/// `shell` when loaded as a builtin (e.g. `--with-builtin developer`) and as
/// `developer__shell` when namespaced, so gate on both (and any `<ext>__shell`).
/// Any other tool is not one we gate, so it emits nothing.
fn is_shell_tool(name: &str) -> bool {
    name == "shell" || name.ends_with("__shell")
}

/// Wire the adapter to the process's standard streams.
pub fn run() -> Result<i32> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    Ok(evaluate(stdin.lock(), stdout.lock(), stderr.lock()))
}

/// Run the adapter against explicit streams. Returns the process exit code, which
/// is **always `0`**: a block is expressed only as JSON, never via the exit code,
/// so our own failures cannot become a Goose block (Goose treats exit `2` as a
/// block and fails open otherwise). Separated from [`run`] so the protocol can be
/// exercised in-memory by tests.
pub fn evaluate<R: Read, W: Write, E: Write>(mut stdin: R, mut stdout: W, mut stderr: E) -> i32 {
    let mut buffer = String::new();
    if let Err(err) = stdin.read_to_string(&mut buffer) {
        // Never block on our own failure: empty stdout + exit 0 lets Goose run its
        // normal flow (fail open).
        let _ = writeln!(stderr, "allowlister: failed to read stdin: {err}");
        return 0;
    }

    let input: HookInput = match serde_json::from_str(&buffer) {
        Ok(input) => input,
        Err(err) => {
            // Fail open: a parse failure is a no-op (empty stdout), never a block.
            let _ = writeln!(stderr, "allowlister: invalid hook JSON: {err}");
            return 0;
        }
    };

    let dir = discovery_dir(&input);
    let loaded = config::load(Path::new(dir));
    // The shell tool keeps its structural path; every other tool is normalized
    // and gated by the tool-rule engine. An unrecognized tool with no matching
    // rule defers, emitting nothing — exactly the prior non-shell behavior.
    let result = if is_shell_tool(&input.tool_name) {
        domain::evaluate(&command_from(&input.tool_input), &loaded.rules)
    } else {
        let call = normalize::goose(&input.tool_name, &input.tool_input);
        domain::evaluate_tool_call(&call, &loaded.tool_rules)
    };

    // Goose honors only a `block`. An allow or defer verdict emits nothing — a
    // true fall-through to Goose's own flow.
    if matches!(result.verdict, Verdict::Deny) {
        write_block(&mut stdout, &format!("allowlister: {}", result.reason));
    }
    0
}

/// The directory used for project-config discovery. Goose sends the session
/// `working_dir`; fall back to the current directory if it is missing or empty.
fn discovery_dir(input: &HookInput) -> &str {
    input
        .working_dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
        .unwrap_or(".")
}

/// Extract the shell command from `tool_input`. Goose sends it as a JSON object
/// (`{"command": "..."}`); any other shape yields an empty command, which the
/// engine vacuously allows (a no-op).
fn command_from(tool_input: &Value) -> String {
    tool_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Write Goose's native `block` decision. The reason is carried so the model sees
/// why the command was blocked.
fn write_block<W: Write>(stdout: &mut W, reason: &str) {
    let output = HookOutput {
        decision: "block",
        reason: reason.to_string(),
    };
    // This small fixed shape cannot fail to serialize; if the write fails Goose
    // sees empty stdout and falls through to its normal flow — never a block on
    // our error.
    if let Ok(json) = serde_json::to_string(&output) {
        let _ = writeln!(stdout, "{json}");
    }
}

#[derive(Debug, Default, Deserialize)]
struct HookInput {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    tool_input: Value,
}

#[derive(Debug, Serialize)]
struct HookOutput {
    decision: &'static str,
    reason: String,
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
        value["decision"].as_str().unwrap_or("").to_string()
    }

    /// A project sandbox with a `.git` boundary and a single deny rule, so
    /// discovery finds it from `working_dir`.
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

    /// A `PreToolUse` payload for the `developer__shell` tool, command under
    /// `tool_input.command` and the cwd under `working_dir`.
    fn payload(command: &str, working_dir: &Path) -> String {
        format!(
            r#"{{"event":"PreToolUse","tool_name":"developer__shell","tool_input":{{"command":{}}},"working_dir":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&working_dir.to_string_lossy().into_owned()).unwrap()
        )
    }

    #[test]
    fn invalid_json_defers_via_exit_zero_and_empty_stdout() {
        // The fail-open path: a parse error must NOT exit 2 (which Goose would
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
            run_payload(r#"{"tool_name":"developer__text_editor","tool_input":{"command":"x"}}"#);
        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "a non-shell tool must emit nothing");
    }

    #[test]
    fn unknown_command_defers_with_empty_stdout() {
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&payload("some_unknown_tool --x", dir.path()));
        assert_eq!(code, 0);
        // Goose falls through on empty stdout, so a defer emits nothing.
        assert!(stdout.is_empty(), "an undecided command must emit nothing");
    }

    #[test]
    fn allowed_command_emits_nothing() {
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
    fn denied_command_maps_to_block() {
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&payload("touch /tmp/x", dir.path()));
        assert_eq!(code, 0);
        // Goose's block keyword is `block`, not `deny`.
        assert_eq!(decision(&stdout), "block");
        let value: Value = serde_json::from_slice(&stdout).unwrap();
        assert!(value["reason"]
            .as_str()
            .unwrap()
            .starts_with("allowlister:"));
    }

    #[test]
    fn bare_shell_tool_name_is_gated() {
        // Goose's builtin developer extension names the tool `shell` (no
        // `developer__` prefix); the gate must still fire on it.
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&format!(
            r#"{{"event":"PreToolUse","tool_name":"shell","tool_input":{{"command":"touch /tmp/x"}},"working_dir":{}}}"#,
            serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap()
        ));
        assert_eq!(code, 0);
        assert_eq!(decision(&stdout), "block");
    }

    #[test]
    fn missing_command_field_defaults_empty_and_does_not_panic() {
        let dir = TempDir::new().unwrap();
        let (code, stdout) = run_payload(&format!(
            r#"{{"tool_name":"developer__shell","tool_input":{{}},"working_dir":{}}}"#,
            serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap()
        ));
        assert_eq!(code, 0);
        // A missing command is empty → vacuously allowed → no-op (empty stdout).
        assert!(stdout.is_empty());
    }

    #[test]
    fn empty_working_dir_falls_back_to_dot_without_panic() {
        let (code, stdout) = run_payload(
            r#"{"tool_name":"developer__shell","tool_input":{"command":"some_unknown_tool"},"working_dir":""}"#,
        );
        assert_eq!(code, 0);
        // No project config under `.` in the test's cwd, so it defers (empty).
        assert!(stdout.is_empty());
    }
}

//! OpenCode hook adapter.
//!
//! OpenCode has no subprocess hook: it gates a tool call only through an
//! in-process JS/TS plugin (`tool.execute.before`, which blocks by throwing). So
//! allowlister ships a tiny plugin shim (written by `init` into
//! `.opencode/plugin/`) that, before any tool call, spawns `allowlister hook
//! opencode`, pipes the tool name and arguments as JSON to this adapter, and
//! throws when we say deny. This adapter is the Rust half of that bridge — the
//! plugin↔binary contract is entirely ours, so it mirrors the other deny-only
//! adapters:
//!
//! - **Input** (sent by the shim): `{"tool_name":"<tool>","tool_input":{…},
//!   "cwd":"…"}` — the real tool id (`read`/`write`/`bash`/`server_tool`…) and its
//!   arguments.
//! - **Output**: a flat `{"decision":"deny","reason":"…"}` only on a deny; an
//!   allow or defer verdict emits *nothing*, which the shim treats as "no
//!   objection" and lets the call proceed.
//! - **Exit code is always `0`.** The shim decides whether to block purely from
//!   the decision JSON, so an internal read/parse failure here is a no-op
//!   (empty stdout) that fails open — an internal error can never become a block.
//!
//! The shim forwards every tool call with its real id; this adapter routes a shell
//! call (the `bash`/`shell` tool, or any call carrying a `command`) to the command
//! engine and every other tool to the tool-rule engine. `tool.execute.before`
//! fires for sub-agent (`task`-tool) calls too, so those are gated as well.

use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::normalize;
use crate::config;
use crate::domain::{self, Verdict};
use crate::errors::Result;

/// Whether this call is a shell command: the shim labels OpenCode's shell tool
/// `bash` (some builds expose `shell`), and any shell-like tool carries a
/// `command` argument. Everything else is a non-shell tool, gated by the
/// tool-rule engine.
fn is_shell_call(input: &HookInput) -> bool {
    matches!(input.tool_name.as_str(), "bash" | "shell")
        || input
            .tool_input
            .get("command")
            .and_then(Value::as_str)
            .is_some()
}

/// Wire the adapter to the process's standard streams.
pub fn run() -> Result<i32> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    Ok(evaluate(stdin.lock(), stdout.lock(), stderr.lock()))
}

/// Run the adapter against explicit streams. Returns the process exit code, which
/// is **always `0`**: a deny is expressed only as JSON, and the plugin shim
/// blocks solely on that JSON, so our own failures cannot become a block.
/// Separated from [`run`] so the protocol can be exercised in-memory by tests.
pub fn evaluate<R: Read, W: Write, E: Write>(mut stdin: R, mut stdout: W, mut stderr: E) -> i32 {
    let mut buffer = String::new();
    if let Err(err) = stdin.read_to_string(&mut buffer) {
        // Never block on our own failure: empty stdout lets the shim proceed.
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
    // The shim forwards every tool call. A shell call keeps its structural path;
    // every other tool is normalized and gated by the tool-rule engine. An
    // unrecognized tool with no matching rule emits nothing — the prior behavior.
    let result = if is_shell_call(&input) {
        domain::evaluate(&command_from(&input.tool_input), &loaded.rules)
    } else {
        let call = normalize::opencode(&input.tool_name, &input.tool_input);
        domain::evaluate_tool_call(&call, &loaded.tool_rules)
    };

    // Only `deny` is asserted. An allow or defer verdict emits nothing — the shim
    // reads empty stdout as "no objection" and lets the call run.
    if matches!(result.verdict, Verdict::Deny) {
        write_deny(&mut stdout, &format!("allowlister: {}", result.reason));
    }
    0
}

/// The directory used for project-config discovery. The shim sends OpenCode's
/// working directory; fall back to the current directory if missing or empty.
fn discovery_dir(input: &HookInput) -> &str {
    input
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or(".")
}

/// Extract the shell command from `tool_input`. The shim sends it as a JSON object
/// (`{"command": "..."}`); any other shape yields an empty command, which the
/// engine vacuously allows (a no-op).
fn command_from(tool_input: &Value) -> String {
    tool_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Write the flat `deny` decision the plugin shim throws on. The reason is carried
/// so the model sees why the command was blocked.
fn write_deny<W: Write>(stdout: &mut W, reason: &str) {
    let output = HookOutput {
        decision: "deny",
        reason: reason.to_string(),
    };
    // This small fixed shape cannot fail to serialize; if the write fails the shim
    // sees empty stdout and lets the call proceed — never a block on our error.
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

    /// A shim payload for the `bash` tool with `tool_input` as an object.
    fn payload(command: &str, cwd: &Path) -> String {
        format!(
            r#"{{"tool_name":"bash","tool_input":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&cwd.to_string_lossy().into_owned()).unwrap()
        )
    }

    #[test]
    fn invalid_json_defers_via_exit_zero_and_empty_stdout() {
        // Fail open: a parse error exits 0 with empty stdout, never a block.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = evaluate(&b"{not json"[..], &mut stdout, &mut stderr);
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(!stderr.is_empty());
    }

    #[test]
    fn non_bash_tool_emits_nothing() {
        let (code, stdout) = run_payload(r#"{"tool_name":"read","tool_input":{"command":"x"}}"#);
        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "a non-shell tool must emit nothing");
    }

    #[test]
    fn unknown_command_defers_with_empty_stdout() {
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&payload("some_unknown_tool --x", dir.path()));
        assert_eq!(code, 0);
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
    fn denied_command_maps_to_deny() {
        let dir = sandbox_with_deny();
        let (code, stdout) = run_payload(&payload("touch /tmp/x", dir.path()));
        assert_eq!(code, 0);
        assert_eq!(decision(&stdout), "deny");
        let value: Value = serde_json::from_slice(&stdout).unwrap();
        assert!(value["reason"]
            .as_str()
            .unwrap()
            .starts_with("allowlister:"));
    }

    #[test]
    fn missing_command_field_defaults_empty_and_does_not_panic() {
        let dir = TempDir::new().unwrap();
        let (code, stdout) = run_payload(&format!(
            r#"{{"tool_name":"bash","tool_input":{{}},"cwd":{}}}"#,
            serde_json::to_string(&dir.path().to_string_lossy().into_owned()).unwrap()
        ));
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
    }

    #[test]
    fn empty_cwd_falls_back_to_dot_without_panic() {
        let (code, stdout) = run_payload(
            r#"{"tool_name":"bash","tool_input":{"command":"some_unknown_tool"},"cwd":""}"#,
        );
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
    }

    /// A sandbox that denies reading `.ssh` paths via a tool rule.
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

    fn read_payload(file_path: &str, cwd: &Path) -> String {
        format!(
            r#"{{"tool_name":"read","tool_input":{{"filePath":{}}},"cwd":{}}}"#,
            serde_json::to_string(file_path).unwrap(),
            serde_json::to_string(&cwd.to_string_lossy().into_owned()).unwrap()
        )
    }

    #[test]
    fn read_tool_of_ssh_key_is_denied() {
        let dir = sandbox_with_read_deny();
        let (code, stdout) = run_payload(&read_payload("/home/u/.ssh/id_rsa", dir.path()));
        assert_eq!(code, 0);
        assert_eq!(decision(&stdout), "deny");
    }

    #[test]
    fn read_tool_outside_any_rule_emits_nothing() {
        let dir = sandbox_with_read_deny();
        let (code, stdout) = run_payload(&read_payload("/repo/a.txt", dir.path()));
        assert_eq!(code, 0);
        assert!(stdout.is_empty(), "an undecided read must emit nothing");
    }
}

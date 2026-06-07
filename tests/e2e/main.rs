//! End-to-end tests that execute the compiled `allowlister` binary and verify
//! behavior from a user's perspective: exit codes, stdout, stderr, the
//! stdin/stdout hook contract, JSON output, and file effects of `init`.
//!
//! Each test builds a hermetic config environment in temp dirs (a user config
//! under `XDG_CONFIG_HOME` and a project config in a `.git`-rooted cwd) so the
//! ambient machine config never leaks in.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A hermetic config sandbox: user config under XDG, project config in a
/// `.git`-rooted working directory.
struct Sandbox {
    xdg: TempDir,
    cwd: TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let user_cfg = fs::read_to_string(manifest.join("examples/user-config.json")).unwrap();
        let project_cfg =
            fs::read_to_string(manifest.join("examples/project-config.json")).unwrap();

        let xdg = TempDir::new().unwrap();
        let allowlister_dir = xdg.path().join("allowlister");
        fs::create_dir_all(&allowlister_dir).unwrap();
        fs::write(allowlister_dir.join("config.json"), user_cfg).unwrap();

        let cwd = TempDir::new().unwrap();
        // A `.git` marker stops project-config discovery at this directory.
        fs::create_dir_all(cwd.path().join(".git")).unwrap();
        fs::write(cwd.path().join(".allowlister.json"), project_cfg).unwrap();

        Sandbox { xdg, cwd }
    }

    fn cwd(&self) -> &Path {
        self.cwd.path()
    }

    /// A command with the hermetic environment applied.
    fn command(&self) -> Command {
        let mut cmd = Command::cargo_bin("allowlister").unwrap();
        cmd.env("XDG_CONFIG_HOME", self.xdg.path())
            .env("HOME", self.cwd.path());
        cmd
    }

    /// A `PreToolUse` payload whose `cwd` points at the sandbox project dir.
    fn payload(&self, command: &str) -> String {
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&self.cwd().to_string_lossy()).unwrap()
        )
    }

    /// A Cursor `beforeShellExecution` payload whose `cwd` points at the sandbox
    /// project dir.
    fn cursor_payload(&self, command: &str) -> String {
        let dir = serde_json::to_string(&self.cwd().to_string_lossy()).unwrap();
        format!(
            r#"{{"hook_event_name":"beforeShellExecution","command":{},"cwd":{},"workspace_roots":[{}]}}"#,
            serde_json::to_string(command).unwrap(),
            dir,
            dir
        )
    }

    /// A Cursor payload with an empty `cwd`, so discovery must fall back to
    /// `workspace_roots`.
    fn cursor_payload_empty_cwd(&self, command: &str) -> String {
        format!(
            r#"{{"hook_event_name":"beforeShellExecution","command":{},"cwd":"","workspace_roots":[{}]}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&self.cwd().to_string_lossy()).unwrap()
        )
    }

    /// A Copilot `preToolUse` payload for the `bash` tool whose `cwd` points at
    /// the sandbox project dir.
    fn copilot_payload(&self, command: &str) -> String {
        format!(
            r#"{{"toolName":"bash","toolArgs":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&self.cwd().to_string_lossy()).unwrap()
        )
    }

    /// A Codex `PreToolUse` payload whose `cwd` points at the sandbox project dir.
    /// The shell command rides under `tool_input.command`, like Claude Code.
    fn codex_payload(&self, command: &str) -> String {
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&self.cwd().to_string_lossy()).unwrap()
        )
    }

    /// A Crush `PreToolUse` payload whose `cwd` points at the sandbox project dir.
    /// Crush names its shell tool `bash` (lowercase) and rides the command under
    /// `tool_input.command`.
    fn crush_payload(&self, command: &str) -> String {
        format!(
            r#"{{"event":"PreToolUse","tool_name":"bash","tool_input":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&self.cwd().to_string_lossy()).unwrap()
        )
    }

    /// A Qwen Code `PreToolUse` payload whose `cwd` points at the sandbox project
    /// dir. Qwen names its shell tool `run_shell_command` (Gemini-style) and rides
    /// the command under `tool_input.command`.
    fn qwen_payload(&self, command: &str) -> String {
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"run_shell_command","tool_input":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&self.cwd().to_string_lossy()).unwrap()
        )
    }

    /// A Goose `PreToolUse` payload whose `working_dir` points at the sandbox
    /// project dir. Goose names its shell tool `shell` (builtin) or
    /// `developer__shell` (namespaced) — both are gated; this exercises the
    /// namespaced form — and carries the cwd under `working_dir` (not `cwd`).
    fn goose_payload(&self, command: &str) -> String {
        format!(
            r#"{{"event":"PreToolUse","tool_name":"developer__shell","tool_input":{{"command":{}}},"working_dir":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&self.cwd().to_string_lossy()).unwrap()
        )
    }

    /// An OpenCode shim payload whose `cwd` points at the sandbox project dir. The
    /// shim labels every shell command `bash` and rides it under
    /// `tool_input.command`.
    fn opencode_payload(&self, command: &str) -> String {
        format!(
            r#"{{"tool_name":"bash","tool_input":{{"command":{}}},"cwd":{}}}"#,
            serde_json::to_string(command).unwrap(),
            serde_json::to_string(&self.cwd().to_string_lossy()).unwrap()
        )
    }
}

fn decision_of(stdout: &[u8]) -> String {
    let value: Value = serde_json::from_slice(stdout).expect("hook stdout must be valid JSON");
    value["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Read the `permission` field Cursor's hook adapter writes.
fn permission_of(stdout: &[u8]) -> String {
    let value: Value = serde_json::from_slice(stdout).expect("hook stdout must be valid JSON");
    value["permission"].as_str().unwrap().to_string()
}

/// Read the `permissionDecision` field Copilot's hook adapter writes.
fn copilot_decision_of(stdout: &[u8]) -> String {
    let value: Value = serde_json::from_slice(stdout).expect("hook stdout must be valid JSON");
    value["permissionDecision"].as_str().unwrap().to_string()
}

/// Read the flat `decision` field Crush's hook adapter writes.
fn crush_decision_of(stdout: &[u8]) -> String {
    let value: Value = serde_json::from_slice(stdout).expect("hook stdout must be valid JSON");
    value["decision"].as_str().unwrap().to_string()
}

/// Read the top-level `decision` field Goose's hook adapter writes for a block.
fn goose_decision_of(stdout: &[u8]) -> String {
    let value: Value = serde_json::from_slice(stdout).expect("hook stdout must be valid JSON");
    value["decision"].as_str().unwrap().to_string()
}

/// Read the top-level `decision` field OpenCode's hook adapter writes for a deny.
fn opencode_decision_of(stdout: &[u8]) -> String {
    let value: Value = serde_json::from_slice(stdout).expect("hook stdout must be valid JSON");
    value["decision"].as_str().unwrap().to_string()
}

#[test]
fn help_succeeds_and_lists_subcommands() {
    Command::cargo_bin("allowlister")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("hook"))
        .stdout(predicate::str::contains("check"))
        .stdout(predicate::str::contains("explain"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("install"));
}

#[test]
fn version_prints_package_version() {
    Command::cargo_bin("allowlister")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn hook_allow_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("gh pr list | head -20"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "allow");
}

#[test]
fn hook_deny_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "deny");
}

#[test]
fn hook_defer_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("some_unknown_tool --flag"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "defer");
}

#[test]
fn hook_redirection_allow_uses_project_rule() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("echo hi > /tmp/x.txt"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "allow");
}

#[test]
fn hook_invalid_json_exits_one_and_writes_nothing_to_stdout() {
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "claude-code"])
        .write_stdin("{ this is not json")
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

#[test]
fn cursor_hook_allow_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "cursor"])
        .write_stdin(sandbox.cursor_payload("gh pr list | head -20"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(permission_of(&output), "allow");
}

#[test]
fn cursor_hook_deny_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "cursor"])
        .write_stdin(sandbox.cursor_payload("rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(permission_of(&output), "deny");
}

#[test]
fn cursor_hook_defer_maps_to_ask() {
    // Cursor has no "defer" token: an undecided command escalates to "ask".
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "cursor"])
        .write_stdin(sandbox.cursor_payload("some_unknown_tool --flag"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(permission_of(&output), "ask");
}

#[test]
fn cursor_hook_empty_cwd_uses_workspace_root() {
    // An empty `cwd` (common from Cursor) must fall back to `workspace_roots`,
    // so the project config still gates the command.
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "cursor"])
        .write_stdin(sandbox.cursor_payload_empty_cwd("rm -rf /var"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(permission_of(&output), "deny");
}

#[test]
fn cursor_hook_invalid_json_exits_one_and_writes_nothing_to_stdout() {
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "cursor"])
        .write_stdin("{ this is not json")
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

#[test]
fn codex_hook_deny_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "codex"])
        .write_stdin(sandbox.codex_payload("rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    // Codex shares Claude Code's PreToolUse decision shape for a deny.
    assert_eq!(decision_of(&output), "deny");
}

#[test]
fn codex_hook_allow_emits_empty_stdout() {
    // Codex rejects a bare `allow`, so an allow verdict is a no-op: empty stdout
    // hands the call back to Codex's normal approval flow.
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "codex"])
        .write_stdin(sandbox.codex_payload("gh pr list | head -20"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn codex_hook_defer_emits_empty_stdout() {
    // A deferred verdict also emits nothing — a true fall-through, no `defer`
    // token (which Codex's PreToolUse does not accept).
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "codex"])
        .write_stdin(sandbox.codex_payload("some_unknown_tool --flag"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn codex_hook_invalid_json_exits_zero_and_writes_nothing_to_stdout() {
    // The fail-open inversion from Claude/Cursor: Codex treats exit 2 as a block,
    // so a parse failure must exit 0 (not 1/2) with empty stdout — never a deny.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "codex"])
        .write_stdin("{ this is not json")
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

#[test]
fn crush_hook_deny_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "crush"])
        .write_stdin(sandbox.crush_payload("rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    // Crush's native deny shape is a flat `{"decision":"deny",...}`.
    assert_eq!(crush_decision_of(&output), "deny");
}

#[test]
fn crush_hook_allow_emits_empty_stdout() {
    // An explicit `allow` would pre-approve and skip Crush's prompt, so an allow
    // verdict is a no-op: empty stdout hands the call back to Crush's normal flow.
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "crush"])
        .write_stdin(sandbox.crush_payload("gh pr list | head -20"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn crush_hook_defer_emits_empty_stdout() {
    // A deferred verdict also emits nothing — Crush treats empty stdout as "no
    // opinion" and falls through to its normal flow.
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "crush"])
        .write_stdin(sandbox.crush_payload("some_unknown_tool --flag"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn crush_hook_invalid_json_exits_zero_and_writes_nothing_to_stdout() {
    // Like Codex, Crush blocks on exit 2 and fails open otherwise, so a parse
    // failure must exit 0 (not 1/2) with empty stdout — never a block.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "crush"])
        .write_stdin("{ this is not json")
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

#[test]
fn qwen_hook_deny_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "qwen"])
        .write_stdin(sandbox.qwen_payload("rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    // Qwen shares Claude Code's PreToolUse decision shape for a deny.
    assert_eq!(decision_of(&output), "deny");
}

#[test]
fn qwen_hook_allow_emits_empty_stdout() {
    // A non-deny verdict is a no-op: empty stdout hands the call back to Qwen's
    // normal approval flow rather than auto-approving via an explicit allow.
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "qwen"])
        .write_stdin(sandbox.qwen_payload("gh pr list | head -20"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn qwen_hook_defer_emits_empty_stdout() {
    // A deferred verdict also emits nothing — a true fall-through to Qwen's own
    // approval flow.
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "qwen"])
        .write_stdin(sandbox.qwen_payload("some_unknown_tool --flag"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn qwen_hook_invalid_json_exits_zero_and_writes_nothing_to_stdout() {
    // The fail-open inversion: Qwen treats exit 2 as a block, so a parse failure
    // must exit 0 (not 1/2) with empty stdout — never a deny.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "qwen"])
        .write_stdin("{ this is not json")
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

#[test]
fn goose_hook_deny_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "goose"])
        .write_stdin(sandbox.goose_payload("rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    // Goose's native block keyword is `block`, not `deny`.
    assert_eq!(goose_decision_of(&output), "block");
}

#[test]
fn goose_hook_allow_emits_empty_stdout() {
    // A non-block verdict is a no-op: empty stdout hands the call back to Goose's
    // normal flow.
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "goose"])
        .write_stdin(sandbox.goose_payload("gh pr list | head -20"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn goose_hook_defer_emits_empty_stdout() {
    // A deferred verdict also emits nothing — a true fall-through to Goose's flow.
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "goose"])
        .write_stdin(sandbox.goose_payload("some_unknown_tool --flag"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn goose_hook_invalid_json_exits_zero_and_writes_nothing_to_stdout() {
    // The fail-open inversion: Goose treats exit 2 as a block, so a parse failure
    // must exit 0 (not 1/2) with empty stdout — never a block.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "goose"])
        .write_stdin("{ this is not json")
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

#[test]
fn opencode_hook_deny_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "opencode"])
        .write_stdin(sandbox.opencode_payload("rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    // The shim throws on a flat `{"decision":"deny",...}`.
    assert_eq!(opencode_decision_of(&output), "deny");
}

#[test]
fn opencode_hook_allow_emits_empty_stdout() {
    // A non-deny verdict is a no-op: empty stdout tells the shim not to throw.
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "opencode"])
        .write_stdin(sandbox.opencode_payload("gh pr list | head -20"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn opencode_hook_defer_emits_empty_stdout() {
    // A deferred verdict also emits nothing — the shim lets the call run.
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "opencode"])
        .write_stdin(sandbox.opencode_payload("some_unknown_tool --flag"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn opencode_hook_invalid_json_exits_zero_and_writes_nothing_to_stdout() {
    // The shim blocks only on a deny JSON, so a parse failure must exit 0 with
    // empty stdout — never a block.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "opencode"])
        .write_stdin("{ this is not json")
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

#[test]
fn copilot_hook_allow_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "copilot"])
        .write_stdin(sandbox.copilot_payload("gh pr list | head -20"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(copilot_decision_of(&output), "allow");
}

#[test]
fn copilot_hook_deny_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "copilot"])
        .write_stdin(sandbox.copilot_payload("rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(copilot_decision_of(&output), "deny");
}

#[test]
fn copilot_hook_defer_emits_empty_stdout() {
    // Copilot has a native fall-through: an undecided command emits nothing, so
    // Copilot runs its own permission flow (a true defer, not an escalation).
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "copilot"])
        .write_stdin(sandbox.copilot_payload("some_unknown_tool --flag"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn copilot_hook_invalid_json_defers_via_exit_zero_and_empty_stdout() {
    // Copilot's preToolUse is fail-CLOSED: a non-zero exit would DENY. So unlike
    // the Claude/Cursor adapters, a parse failure here must exit 0 with empty
    // stdout (defer to Copilot's normal flow), never deny on our own error.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "copilot"])
        .write_stdin("{ this is not json")
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

// ---- ask verdict across every harness adapter ----------------------------
//
// `npm publish` matches the example config's ask rule. Adapters with a native
// "ask"/"confirm" state must emit it; deny-only adapters (which honor only a
// block) must degrade to the same empty-stdout fall-through as a defer — a
// prompt, never a silent allow and never a hard block. This is the binary-driven
// proxy for the live CLIs, whose own scripts cannot assert ask (it hands control
// to the agent's interactive permission prompt).

#[test]
fn claude_code_hook_ask_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("npm publish --access public"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "ask");
}

#[test]
fn cursor_hook_ask_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "cursor"])
        .write_stdin(sandbox.cursor_payload("npm publish --access public"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(permission_of(&output), "ask");
}

#[test]
fn copilot_hook_ask_routes_through_stdin_stdout() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["hook", "copilot"])
        .write_stdin(sandbox.copilot_payload("npm publish --access public"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(copilot_decision_of(&output), "ask");
}

#[test]
fn codex_hook_ask_emits_empty_stdout() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "codex"])
        .write_stdin(sandbox.codex_payload("npm publish --access public"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn crush_hook_ask_emits_empty_stdout() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "crush"])
        .write_stdin(sandbox.crush_payload("npm publish --access public"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn qwen_hook_ask_emits_empty_stdout() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "qwen"])
        .write_stdin(sandbox.qwen_payload("npm publish --access public"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn goose_hook_ask_emits_empty_stdout() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "goose"])
        .write_stdin(sandbox.goose_payload("npm publish --access public"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn opencode_hook_ask_emits_empty_stdout() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["hook", "opencode"])
        .write_stdin(sandbox.opencode_payload("npm publish --access public"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn check_ask_returns_exit_code_zero() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["check", "npm publish --access public", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ASK"));
}

#[test]
fn check_deny_returns_exit_code_two() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["check", "rm -rf /var", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .code(2)
        .stdout(predicate::str::starts_with("DENY"));
}

#[test]
fn check_allow_returns_exit_code_zero() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["check", "gh pr list | head -20", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn check_json_emits_machine_readable_object() {
    let sandbox = Sandbox::new();
    let output = sandbox
        .command()
        .args(["check", "rm -rf /var", "--json", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["verdict"], "deny");
    assert!(value["reason"].as_str().unwrap().contains("rm -rf"));
}

#[test]
fn explain_prints_fragment_table_and_verdict() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["explain", "gh pr list | head -20 | wc -l", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::contains("fragments (3)"))
        .stdout(predicate::str::contains("[pipe_source]"))
        .stdout(predicate::str::contains("[pipe_filter]"))
        .stdout(predicate::str::contains("verdict: ALLOW"));
}

#[test]
fn init_local_writes_config_and_registers_hook() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister hook claude-code"))
        .stdout(predicate::str::contains("do NOT add"));
    // Both the config and the registered hook land under the project dir.
    assert!(dir.path().join(".allowlister.json").is_file());
    let settings = dir.path().join(".claude/settings.json");
    assert!(settings.is_file(), "the Bash hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "allowlister hook claude-code"
    );
}

#[test]
fn init_no_hooks_skips_settings_and_prints_snippet() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Add this to ~/.claude/settings.json",
        ));
    assert!(dir.path().join(".allowlister.json").is_file());
    assert!(
        !dir.path().join(".claude/settings.json").exists(),
        "--no-hooks must not touch settings.json"
    );
}

#[test]
fn init_cursor_local_registers_hooks_json() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "cursor"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister hook cursor"))
        // Cursor has no allow list, so the Claude-specific warning must not show.
        .stdout(predicate::str::contains("do NOT add").not());
    assert!(dir.path().join(".allowlister.json").is_file());
    // Cursor wires hooks.json, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let hooks = dir.path().join(".cursor/hooks.json");
    assert!(hooks.is_file(), "the cursor hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(hooks).unwrap()).unwrap();
    assert_eq!(doc["version"], 1);
    // All three gateable Cursor events are registered: shell, file read, and MCP.
    for event in [
        "beforeShellExecution",
        "beforeReadFile",
        "beforeMCPExecution",
    ] {
        assert_eq!(
            doc["hooks"][event][0]["command"], "allowlister hook cursor",
            "the {event} hook must be auto-registered"
        );
    }
}

#[test]
fn init_cursor_no_hooks_prints_cursor_snippet() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "cursor", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Add this to ~/.cursor/hooks.json"));
    assert!(dir.path().join(".allowlister.json").is_file());
    assert!(
        !dir.path().join(".cursor/hooks.json").exists(),
        "--no-hooks must not write hooks.json"
    );
}

#[test]
fn init_codex_local_registers_hooks_json() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "codex"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister hook codex"))
        // Codex has no allow list, so the Claude-specific warning must not show.
        .stdout(predicate::str::contains("do NOT add").not());
    assert!(dir.path().join(".allowlister.json").is_file());
    // Codex wires .codex/hooks.json, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let hooks = dir.path().join(".codex/hooks.json");
    assert!(hooks.is_file(), "the codex hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(hooks).unwrap()).unwrap();
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["matcher"],
        "^(Bash|apply_patch)$|^mcp__"
    );
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "allowlister hook codex"
    );
}

#[test]
fn init_codex_no_hooks_prints_codex_snippet() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "codex", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Add this to ~/.codex/hooks.json"));
    assert!(dir.path().join(".allowlister.json").is_file());
    assert!(
        !dir.path().join(".codex/hooks.json").exists(),
        "--no-hooks must not write hooks.json"
    );
}

#[test]
fn init_crush_local_registers_crush_json() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "crush"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister hook crush"))
        // Crush has no allow list, so the Claude-specific warning must not show.
        .stdout(predicate::str::contains("do NOT add").not());
    assert!(dir.path().join(".allowlister.json").is_file());
    // Crush wires crush.json, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let config = dir.path().join("crush.json");
    assert!(config.is_file(), "the crush hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(config).unwrap()).unwrap();
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["matcher"],
        "^(bash|view|write|edit|multiedit|fetch|web_fetch|web_search|glob|grep)$|^mcp_"
    );
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["command"],
        "allowlister hook crush"
    );
}

#[test]
fn init_crush_no_hooks_prints_crush_snippet() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "crush", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Add this to crush.json"));
    assert!(dir.path().join(".allowlister.json").is_file());
    assert!(
        !dir.path().join("crush.json").exists(),
        "--no-hooks must not write crush.json"
    );
}

#[test]
fn init_qwen_local_registers_settings_json() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "qwen"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister hook qwen"))
        // Qwen has no allow list, so the Claude-specific warning must not show.
        .stdout(predicate::str::contains("do NOT add").not());
    assert!(dir.path().join(".allowlister.json").is_file());
    // Qwen wires .qwen/settings.json, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let settings = dir.path().join(".qwen/settings.json");
    assert!(settings.is_file(), "the qwen hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["matcher"],
        "^(run_shell_command|read_file|write_file|edit|glob|grep_search|web_fetch)$|^mcp__"
    );
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "allowlister hook qwen"
    );
}

#[test]
fn init_qwen_no_hooks_prints_qwen_snippet() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "qwen", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Add this to ~/.qwen/settings.json",
        ));
    assert!(dir.path().join(".allowlister.json").is_file());
    assert!(
        !dir.path().join(".qwen/settings.json").exists(),
        "--no-hooks must not write settings.json"
    );
}

#[test]
fn init_goose_local_registers_plugin() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "goose"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister hook goose"))
        // Goose has no allow list, so the Claude-specific warning must not show.
        .stdout(predicate::str::contains("do NOT add").not());
    assert!(dir.path().join(".allowlister.json").is_file());
    // Goose wires a plugin directory, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let plugin = dir.path().join(".agents/plugins/allowlister");
    assert!(
        plugin.join("plugin.json").is_file(),
        "the manifest is written"
    );
    let hooks = plugin.join("hooks/hooks.json");
    assert!(hooks.is_file(), "the goose hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(hooks).unwrap()).unwrap();
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["matcher"],
        "^(shell|read|write|edit|text_editor)$|__"
    );
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "allowlister hook goose"
    );
}

#[test]
fn init_goose_no_hooks_prints_goose_snippet() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "goose", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(".agents/plugins/allowlister"));
    assert!(dir.path().join(".allowlister.json").is_file());
    assert!(
        !dir.path().join(".agents").exists(),
        "--no-hooks must not write the plugin directory"
    );
}

#[test]
fn init_opencode_local_writes_plugin() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "opencode"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister hook opencode"))
        // OpenCode has no allow list, so the Claude-specific warning must not show.
        .stdout(predicate::str::contains("do NOT add").not());
    assert!(dir.path().join(".allowlister.json").is_file());
    // OpenCode writes a plugin file, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let plugin = dir.path().join(".opencode/plugin/allowlister.js");
    assert!(plugin.is_file(), "the opencode plugin must be auto-written");
    let text = fs::read_to_string(plugin).unwrap();
    assert!(text.contains("tool.execute.before"));
    assert!(text.contains("allowlister hook opencode"));
}

#[test]
fn init_opencode_no_hooks_prints_opencode_plugin() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "opencode", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(".opencode/plugin/allowlister.js"));
    assert!(dir.path().join(".allowlister.json").is_file());
    assert!(
        !dir.path().join(".opencode").exists(),
        "--no-hooks must not write the plugin"
    );
}

#[test]
fn init_copilot_local_registers_github_hooks_file() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "copilot"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister hook copilot"))
        // Copilot has no allow list, so the Claude-specific warning must not show.
        .stdout(predicate::str::contains("do NOT add").not());
    assert!(dir.path().join(".allowlister.json").is_file());
    // Copilot wires its own file under .github/hooks, never the other harnesses'.
    assert!(!dir.path().join(".claude/settings.json").exists());
    assert!(!dir.path().join(".cursor/hooks.json").exists());
    let hooks = dir.path().join(".github/hooks/allowlister.json");
    assert!(hooks.is_file(), "the copilot hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(hooks).unwrap()).unwrap();
    assert_eq!(doc["version"], 1);
    assert_eq!(
        doc["hooks"]["preToolUse"][0]["bash"],
        "allowlister hook copilot"
    );
}

#[test]
fn init_copilot_no_hooks_prints_copilot_snippet() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "copilot", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(".github/hooks/allowlister.json"));
    assert!(dir.path().join(".allowlister.json").is_file());
    assert!(
        !dir.path().join(".github/hooks/allowlister.json").exists(),
        "--no-hooks must not write the hooks file"
    );
}

#[test]
fn init_profile_installs_a_curated_ruleset() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--profile", "read-only", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("read-only"));
    // The installed profile may carry explanatory `//` comments, so strip them
    // the way the loader does before a strict JSON parse.
    let raw = fs::read_to_string(dir.path().join(".allowlister.json")).unwrap();
    let doc: Value =
        serde_json::from_str(&allowlister::config::strip_jsonc_comments(&raw)).unwrap();
    assert!(
        doc["rules"].as_array().unwrap().len() > 30,
        "the read-only profile carries many rules"
    );

    // The freshly initialized profile actually gates: a pure read allows.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["check", "git status", "--cwd"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn init_interactive_flow_reads_answers_from_stdin() {
    let dir = TempDir::new().unwrap();
    // Answers: 2 = project-local, 2 = read-only, n = skip hooks.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--interactive"])
        .current_dir(dir.path())
        .write_stdin("2\n2\nn\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Which starting ruleset?"));
    // The installed profile may carry explanatory `//` comments, so strip them
    // the way the loader does before a strict JSON parse.
    let raw = fs::read_to_string(dir.path().join(".allowlister.json")).unwrap();
    let doc: Value =
        serde_json::from_str(&allowlister::config::strip_jsonc_comments(&raw)).unwrap();
    assert!(
        doc["rules"].as_array().unwrap().len() > 30,
        "chose read-only"
    );
    assert!(
        !dir.path().join(".claude/settings.json").exists(),
        "answered 'n' to the hook prompt"
    );
}

#[test]
fn init_force_overwrites_an_existing_config() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".allowlister.json"), "{}").unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args([
            "init",
            "--local",
            "--force",
            "--profile",
            "read-only",
            "--no-hooks",
        ])
        .current_dir(dir.path())
        .assert()
        .success();
    let text = fs::read_to_string(dir.path().join(".allowlister.json")).unwrap();
    assert!(text.contains("\"rules\""), "the empty config was replaced");
}

#[test]
fn init_merges_the_hook_into_existing_settings() {
    let dir = TempDir::new().unwrap();
    let claude = dir.path().join(".claude");
    fs::create_dir_all(&claude).unwrap();
    // A settings file the user already owns: must be preserved, not clobbered.
    fs::write(
        claude.join("settings.json"),
        r#"{"$schema":"x","model":"opus","permissions":{"allow":["Bash(ls *)"]}}"#,
    )
    .unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local"])
        .current_dir(dir.path())
        .assert()
        .success();
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(claude.join("settings.json")).unwrap()).unwrap();
    assert_eq!(doc["model"], "opus", "existing keys are preserved");
    assert_eq!(doc["permissions"]["allow"][0], "Bash(ls *)");
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "allowlister hook claude-code"
    );
}

#[test]
fn init_refuses_to_overwrite_existing_config() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".allowlister.json"), "{}").unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .arg("init")
        .arg("--local")
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("refusing to overwrite"));
}

#[test]
fn init_global_writes_under_xdg_and_registers_hook_under_home() {
    let xdg = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--global"])
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("HOME", home.path())
        .assert()
        .success();
    // The config follows XDG; the Claude hook is always under HOME/.claude.
    assert!(xdg.path().join("allowlister/config.json").is_file());
    assert!(home.path().join(".claude/settings.json").is_file());
}

#[test]
fn init_global_falls_back_to_home_config() {
    let home = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--global"])
        .env_remove("XDG_CONFIG_HOME")
        .env("HOME", home.path())
        .assert()
        .success();
    assert!(home
        .path()
        .join(".config/allowlister/config.json")
        .is_file());
    assert!(home.path().join(".claude/settings.json").is_file());
}

#[test]
fn check_without_cwd_uses_current_directory() {
    let sandbox = Sandbox::new();
    // No --cwd: discovery walks from the process's current directory.
    sandbox
        .command()
        .current_dir(sandbox.cwd())
        .args(["check", "rm -rf /opt"])
        .assert()
        .code(2)
        .stdout(predicate::str::starts_with("DENY"));
}

#[test]
fn explain_reports_unsupported_construct() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["explain", "f() { rm -rf /; }; f", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::contains("unsupported"))
        .stdout(predicate::str::contains("verdict: DEFER"));
}

#[test]
fn install_global_writes_a_user_config_that_gates() {
    let xdg = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "read-only", "--global"])
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("HOME", home.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("rule(s) added"));
    assert!(xdg.path().join("allowlister/config.json").is_file());

    // The freshly installed profile is the source of truth: a pure read allows.
    let cwd = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("HOME", home.path())
        .args(["check", "git status", "--cwd"])
        .arg(cwd.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn install_local_writes_a_project_config() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "read-only", "--local"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"));
    assert!(dir.path().join(".allowlister.json").is_file());
}

#[test]
fn install_is_idempotent_across_runs() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("cfg.json");
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "read-only", "--output"])
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"));
    // A second install touches the same file but adds nothing new.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "read-only", "--output"])
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated"))
        .stdout(predicate::str::contains("0 rule(s) added"));
}

#[test]
fn install_unknown_source_fails() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "no-such-thing", "--output"])
        .arg(dir.path().join("cfg.json"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a file or a built-in profile"));
}

#[test]
fn install_from_a_file_source_via_the_binary() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("custom.json");
    fs::write(
        &src,
        r#"{"rules":[{"name":"allow-ls","match":"ls*","action":"allow"}]}"#,
    )
    .unwrap();
    let out = dir.path().join("config.json");
    Command::cargo_bin("allowlister")
        .unwrap()
        .arg("install")
        .arg(&src)
        .arg("--output")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"))
        .stdout(predicate::str::contains("1 rule(s) added"));
    assert!(out.is_file());
}

#[test]
fn check_defer_returns_exit_zero() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["check", "some_unknown_tool --flag", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("DEFER"));
}

#[test]
fn init_global_without_home_or_xdg_fails() {
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--global"])
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "could not locate a home/config directory",
        ));
}

#[test]
fn init_global_reports_a_write_failure() {
    // Point XDG at a regular file so creating the config's parent directory
    // fails; the error must surface as a write failure, not a panic.
    let dir = TempDir::new().unwrap();
    let xdg_is_a_file = dir.path().join("xdg");
    fs::write(&xdg_is_a_file, "x").unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--global"])
        .env("XDG_CONFIG_HOME", &xdg_is_a_file)
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to write"));
}

// ---- Non-shell tool-call gating (built-in + MCP tools) ----

/// A project dir whose config gates non-shell tool calls: reads scoped to the
/// repo (and `.ssh`/`.pem` denied), web_fetch to github only, and a portable MCP
/// rule pair.
fn tool_project() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let allow_glob = format!("{}/**", dir.path().to_string_lossy());
    let cfg = serde_json::json!({
        "rules": [
            { "name": "reads in repo", "tool": "read", "action": "allow",
              "params": { "path": [allow_glob] } },
            { "name": "no secrets", "tool": "read", "action": "deny",
              "params": { "path": ["**/.ssh/**", "**/*.pem"] } },
            { "name": "web github only", "tool": "web_fetch", "action": "allow",
              "params": { "url": ["https://github.com/**"] } },
            { "name": "mcp linear read-only", "tool": "mcp", "action": "allow",
              "params": { "mcp_server": ["linear"], "mcp_tool": ["@(list|get)*"] } },
            { "name": "mcp deny destructive", "tool": "mcp", "action": "deny",
              "params": { "mcp_tool": ["delete*"] } }
        ]
    })
    .to_string();
    fs::write(dir.path().join(".allowlister.json"), cfg).unwrap();
    dir
}

/// A binary command with an empty hermetic HOME/XDG so no ambient user config
/// leaks into a tool-gating test.
fn hermetic_cmd(empty: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("allowlister").unwrap();
    cmd.env("XDG_CONFIG_HOME", empty.path())
        .env("HOME", empty.path());
    cmd
}

#[test]
fn check_tool_read_allows_inside_repo() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    let path = format!("{}/src/main.rs", project.path().to_string_lossy());
    hermetic_cmd(&empty)
        .args(["check", "--tool", "read", "--param"])
        .arg(format!("path={path}"))
        .arg("--cwd")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn check_tool_read_denies_ssh_key_with_exit_two() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    hermetic_cmd(&empty)
        .args([
            "check",
            "--tool",
            "read",
            "--param",
            "path=/home/u/.ssh/id_rsa",
            "--cwd",
        ])
        .arg(project.path())
        .assert()
        .code(2)
        .stdout(predicate::str::starts_with("DENY"));
}

#[test]
fn check_tool_read_outside_rules_defers() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    hermetic_cmd(&empty)
        .args([
            "check",
            "--tool",
            "read",
            "--param",
            "path=/etc/hosts",
            "--cwd",
        ])
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("DEFER"));
}

#[test]
fn check_tool_mcp_portable_rule_denies_destructive() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    hermetic_cmd(&empty)
        .args(["check", "--tool", "mcp__linear__delete_issue", "--cwd"])
        .arg(project.path())
        .assert()
        .code(2)
        .stdout(predicate::str::starts_with("DENY"));
}

#[test]
fn check_tool_bad_param_is_a_usage_error() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    hermetic_cmd(&empty)
        .args(["check", "--tool", "read", "--param", "nokey", "--cwd"])
        .arg(project.path())
        .assert()
        .code(1)
        .stderr(predicate::str::contains("must be key=value"));
}

#[test]
fn claude_hook_read_tool_denies_secret_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Read",
        "tool_input": { "file_path": "/home/u/.ssh/id_rsa" },
        "cwd": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "claude-code"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    // The hook always exits 0; the deny rides in the decision JSON.
    assert_eq!(decision_of(&output), "deny");
}

#[test]
fn claude_hook_read_tool_allows_inside_repo_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    let file = format!("{}/a.txt", project.path().to_string_lossy());
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Read",
        "tool_input": { "file_path": file },
        "cwd": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "claude-code"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "allow");
}

#[test]
fn qwen_hook_read_tool_denies_secret_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    // Qwen's read tool is `read_file` with `file_path`, args as an object.
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "read_file",
        "tool_input": { "file_path": "/home/u/.ssh/id_rsa" },
        "cwd": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "qwen"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "deny");
}

#[test]
fn copilot_hook_read_tool_denies_secret_with_stringified_args() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    // Copilot's read tool is `view` with `path`, and `toolArgs` is a JSON *string*.
    let tool_args = serde_json::to_string(&serde_json::json!({
        "path": "/home/u/.ssh/id_rsa"
    }))
    .unwrap();
    let payload = serde_json::json!({
        "toolName": "view",
        "toolArgs": tool_args,
        "cwd": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "copilot"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(copilot_decision_of(&output), "deny");
}

#[test]
fn cursor_before_read_file_denies_secret_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    let payload = serde_json::json!({
        "hook_event_name": "beforeReadFile",
        "file_path": "/home/u/.ssh/id_rsa",
        "workspace_roots": [project.path().to_string_lossy()],
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "cursor"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(permission_of(&output), "deny");
}

#[test]
fn cursor_before_mcp_execution_denies_destructive_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    let payload = serde_json::json!({
        "hook_event_name": "beforeMCPExecution",
        "tool_name": "mcp__linear__delete_issue",
        "tool_input": {},
        "workspace_roots": [project.path().to_string_lossy()],
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "cursor"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(permission_of(&output), "deny");
}

#[test]
fn opencode_read_tool_denies_secret_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    // The shim forwards the real tool id (`read`) and camelCase args.
    let payload = serde_json::json!({
        "tool_name": "read",
        "tool_input": { "filePath": "/home/u/.ssh/id_rsa" },
        "cwd": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "opencode"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(opencode_decision_of(&output), "deny");
}

// ---- usage history ---------------------------------------------------------
//
// Recording is opt-in. These drive the real binary with `ALLOWLISTER_HISTORY=1`
// (the env override) or a config that turns it on, then assert the `history`
// command reports what the hooks recorded. The store lives under the hermetic
// `XDG_CONFIG_HOME`, so nothing touches the host.

#[test]
fn history_records_hook_evaluations_and_reports_them() {
    let sandbox = Sandbox::new();
    // One of each verdict: allow, defer, deny.
    for command in [
        "gh pr list | head -20",
        "some_unknown_tool --flag",
        "rm -rf /",
    ] {
        sandbox
            .command()
            .env("ALLOWLISTER_HISTORY", "1")
            .args(["hook", "claude-code"])
            .write_stdin(sandbox.payload(command))
            .assert()
            .success();
    }

    // The JSON report aggregates the three evaluations.
    let out = sandbox
        .command()
        .args(["history", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["events_total"], 3);
    assert_eq!(value["overall"]["allow"], 1);
    assert_eq!(value["overall"]["deny"], 1);
    assert_eq!(value["overall"]["defer"], 1);

    // The text report names a deferred subcommand and offers the refine tip.
    let text = sandbox
        .command()
        .args(["history"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("3 event(s) recorded"), "{text}");
    assert!(text.contains("some_unknown_tool --flag"), "{text}");
    assert!(text.contains("Tip:"), "{text}");
}

#[test]
fn history_filter_recent_path_compact_and_clear() {
    let sandbox = Sandbox::new();
    for command in ["some_unknown_tool --flag", "gh pr list | head -20"] {
        sandbox
            .command()
            .env("ALLOWLISTER_HISTORY", "1")
            .args(["hook", "claude-code"])
            .write_stdin(sandbox.payload(command))
            .assert()
            .success();
    }

    // --verdict defer keeps only deferred subcommands.
    let out = sandbox
        .command()
        .args(["history", "--verdict", "defer", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    let rows = value["rows"].as_array().unwrap();
    assert!(rows.iter().all(|r| r["defer"].as_u64().unwrap() > 0));
    assert!(rows.iter().any(|r| r["key"] == "some_unknown_tool --flag"));

    // `path` prints where the store lives; `recent` lists the raw events.
    sandbox
        .command()
        .args(["history", "path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister"));
    sandbox
        .command()
        .args(["history", "recent", "--harness", "claude-code"])
        .assert()
        .success()
        .stdout(predicate::str::contains("some_unknown_tool"));

    // `compact` folds into the summary; `clear` wipes everything.
    sandbox
        .command()
        .args(["history", "compact"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2 event(s)"));
    sandbox
        .command()
        .args(["history", "clear", "-y"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Cleared"));
    sandbox
        .command()
        .args(["history"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No usage recorded yet"));
}

#[test]
fn history_is_off_by_default_and_records_nothing() {
    let sandbox = Sandbox::new();
    // No env override and the sandbox config does not opt in, so nothing records.
    sandbox
        .command()
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("gh pr list | head -20"))
        .assert()
        .success();
    sandbox
        .command()
        .args(["history"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No usage recorded yet"));
}

#[test]
fn init_history_flag_persists_the_toggle_and_drives_recording() {
    let home = TempDir::new().unwrap();
    let xdg = TempDir::new().unwrap();
    let cmd = || {
        let mut c = Command::cargo_bin("allowlister").unwrap();
        c.env("XDG_CONFIG_HOME", xdg.path())
            .env("HOME", home.path());
        c
    };

    // Opt in at init time (no env override).
    cmd()
        .args([
            "init",
            "--global",
            "--profile",
            "starter",
            "--no-hooks",
            "--history",
            "-y",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("history recording is ON"));
    let config = fs::read_to_string(xdg.path().join("allowlister/config.json")).unwrap();
    assert!(config.contains("\"history\""), "{config}");

    // The config flag alone (no env) now drives recording on the next hook run.
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "ls -la" },
        "cwd": home.path().to_string_lossy(),
    })
    .to_string();
    cmd()
        .args(["hook", "claude-code"])
        .write_stdin(payload)
        .assert()
        .success();
    let out = cmd()
        .args(["history", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["events_total"], 1);
    assert_eq!(value["overall"]["allow"], 1);
}

#[test]
fn history_records_tool_calls_and_a_second_harness() {
    let sandbox = Sandbox::new();
    // A non-shell tool call (Subject::Tool) through claude-code, and a shell call
    // through a different harness (codex) — both must record via the shared gate.
    let read = serde_json::json!({
        "tool_name": "Read",
        "tool_input": { "file_path": "/etc/hosts" },
        "cwd": sandbox.cwd().to_string_lossy(),
    })
    .to_string();
    sandbox
        .command()
        .env("ALLOWLISTER_HISTORY", "1")
        .args(["hook", "claude-code"])
        .write_stdin(read)
        .assert()
        .success();
    sandbox
        .command()
        .env("ALLOWLISTER_HISTORY", "1")
        .args(["hook", "codex"])
        .write_stdin(sandbox.codex_payload("gh pr list"))
        .assert()
        .success();

    // The tool call is recorded as a subcommand keyed by its tool name.
    let out = sandbox
        .command()
        .args(["history", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["events_total"], 2);
    let rows = value["rows"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["key"] == "Read"));

    // recent --json carries the kind and the originating harness for each event.
    let recent = sandbox
        .command()
        .args(["history", "recent", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let events: Value = serde_json::from_slice(&recent).unwrap();
    let events = events.as_array().unwrap();
    assert!(events
        .iter()
        .any(|e| e["kind"] == "tool" && e["harness"] == "claude-code" && e["command"] == "Read"));
    assert!(events
        .iter()
        .any(|e| e["kind"] == "shell" && e["harness"] == "codex"));
}

#[test]
fn history_views_and_recent_project_filter() {
    let sandbox = Sandbox::new();
    for command in ["gh pr list | head -20", "some_unknown_tool --flag"] {
        sandbox
            .command()
            .env("ALLOWLISTER_HISTORY", "1")
            .args(["hook", "claude-code"])
            .write_stdin(sandbox.payload(command))
            .assert()
            .success();
    }

    // --view programs collapses subcommands to their leading program.
    sandbox
        .command()
        .args(["history", "--view", "programs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PROGRAM").and(predicate::str::contains("gh")));
    // --view commands shows whole command lines.
    sandbox
        .command()
        .args(["history", "--view", "commands"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gh pr list | head -20"));

    // recent --project keeps only events whose project tag matches; a bogus tag
    // matches nothing.
    let project = sandbox.cwd().to_string_lossy().into_owned();
    sandbox
        .command()
        .args(["history", "recent", "--project", &project])
        .assert()
        .success()
        .stdout(predicate::str::contains("some_unknown_tool"));
    sandbox
        .command()
        .args(["history", "recent", "--project", "/no/such/project"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No recent events"));
}

#[test]
fn init_no_history_persists_the_disabled_toggle() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let out = Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", home.path())
        .env("HOME", home.path())
        .current_dir(project.path())
        .args([
            "init",
            "--local",
            "--profile",
            "starter",
            "--no-hooks",
            "--no-history",
            "-y",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(String::from_utf8(out).unwrap().contains("OFF"));
    // The choice is persisted explicitly as disabled.
    let config = fs::read_to_string(project.path().join(".allowlister.json")).unwrap();
    assert!(config.contains("\"enabled\": false"), "{config}");
}

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

    fn write_project_config(&self, body: &str) {
        fs::write(self.cwd().join(".allowlister.json"), body).unwrap();
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
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("config"));
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
fn plugin_can_dynamically_allow_a_deferred_command() {
    let sandbox = Sandbox::new();
    let plugin = assert_cmd::cargo::cargo_bin("allowlister");
    let plugin = serde_json::to_string(&plugin.to_string_lossy()).unwrap();
    sandbox.write_project_config(&format!(
        r#"{{"rules":[],"plugins":[{{"name":"ticket approver","command":[{plugin},"example-plugin"]}}]}}"#
    ));

    sandbox
        .command()
        .args(["check", "deploy --ticket=APPROVED", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ALLOW: plugin 'ticket approver': approved ticket tag present",
        ));
}

#[test]
fn plugin_ask_overrides_a_static_allow() {
    let sandbox = Sandbox::new();
    let plugin = assert_cmd::cargo::cargo_bin("allowlister");
    let plugin = serde_json::to_string(&plugin.to_string_lossy()).unwrap();
    sandbox.write_project_config(&format!(
        r#"{{"rules":[{{"name":"deploys","match":"deploy*","action":"allow"}}],"plugins":[{{"name":"prod reviewer","command":[{plugin},"example-plugin"]}}]}}"#
    ));

    sandbox
        .command()
        .args(["check", "deploy prod", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ASK: plugin 'prod reviewer': production needs review",
        ));
}

#[test]
fn plugin_deny_blocks_a_hook_command() {
    let sandbox = Sandbox::new();
    let plugin = assert_cmd::cargo::cargo_bin("allowlister");
    let plugin = serde_json::to_string(&plugin.to_string_lossy()).unwrap();
    sandbox.write_project_config(&format!(
        r#"{{"rules":[{{"name":"deploys","match":"deploy*","action":"allow"}}],"plugins":[{{"name":"prod blocker","command":[{plugin},"example-plugin"]}}]}}"#
    ));

    let output = sandbox
        .command()
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("deploy block-prod"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "deny");
}

#[test]
fn static_deny_remains_final_even_when_plugin_would_allow() {
    let sandbox = Sandbox::new();
    let plugin = assert_cmd::cargo::cargo_bin("allowlister");
    let plugin = serde_json::to_string(&plugin.to_string_lossy()).unwrap();
    sandbox.write_project_config(&format!(
        r#"{{"rules":[],"plugins":[{{"name":"ticket approver","command":[{plugin},"example-plugin"]}}]}}"#
    ));

    sandbox
        .command()
        .args(["check", "rm -rf /var --ticket=APPROVED", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .code(2)
        .stdout(predicate::str::contains("DENY"))
        .stdout(predicate::str::contains("rm -rf"));
}

#[test]
fn plugin_invalid_json_is_non_fatal_and_preserves_static_allow() {
    let sandbox = Sandbox::new();
    let plugin = assert_cmd::cargo::cargo_bin("allowlister");
    let plugin = serde_json::to_string(&plugin.to_string_lossy()).unwrap();
    sandbox.write_project_config(&format!(
        r#"{{"rules":[{{"name":"deploys","match":"deploy*","action":"allow"}}],"plugins":[{{"name":"broken plugin","command":[{plugin},"example-plugin"]}}]}}"#
    ));

    sandbox
        .command()
        .args(["check", "deploy plugin-bad-json", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn plugin_timeout_is_non_fatal_and_preserves_static_allow() {
    let sandbox = Sandbox::new();
    let plugin = assert_cmd::cargo::cargo_bin("allowlister");
    let plugin = serde_json::to_string(&plugin.to_string_lossy()).unwrap();
    sandbox.write_project_config(&format!(
        r#"{{"rules":[{{"name":"deploys","match":"deploy*","action":"allow"}}],"plugins":[{{"name":"slow plugin","command":[{plugin},"example-plugin"],"timeout_ms":10}}]}}"#
    ));

    sandbox
        .command()
        .args(["check", "deploy plugin-slow", "--cwd"])
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
    let settings = dir.path().join(".claude/settings.json");
    assert!(settings.is_file(), "the Bash hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
    assert!(doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("hook claude-code"));
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
        assert!(
            doc["hooks"][event][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("hook cursor"),
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
    // Codex wires .codex/hooks.json, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let hooks = dir.path().join(".codex/hooks.json");
    assert!(hooks.is_file(), "the codex hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(hooks).unwrap()).unwrap();
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["matcher"],
        "^(Bash|apply_patch)$|^mcp__"
    );
    assert!(doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("hook codex"));
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
    // Crush wires crush.json, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let config = dir.path().join("crush.json");
    assert!(config.is_file(), "the crush hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(config).unwrap()).unwrap();
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["matcher"],
        "^(bash|view|write|edit|multiedit|fetch|web_fetch|web_search|glob|grep)$|^mcp_"
    );
    assert!(doc["hooks"]["PreToolUse"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("hook crush"));
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
    // Qwen wires .qwen/settings.json, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let settings = dir.path().join(".qwen/settings.json");
    assert!(settings.is_file(), "the qwen hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["matcher"],
        "^(run_shell_command|read_file|write_file|edit|glob|grep_search|web_fetch)$|^mcp__"
    );
    assert!(doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("hook qwen"));
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
    // On Windows the goose command is the absolute exe path (its plugin runner
    // spawns it directly, where a bare name wouldn't resolve); elsewhere it is the
    // bare name. Assert the gate subcommand, not the program token.
    assert!(doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("hook goose"));
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
    // OpenCode writes a plugin file, never Claude Code's settings.json.
    assert!(!dir.path().join(".claude/settings.json").exists());
    let plugin = dir.path().join(".opencode/plugin/allowlister.js");
    assert!(plugin.is_file(), "the opencode plugin must be auto-written");
    let text = fs::read_to_string(plugin).unwrap();
    assert!(text.contains("tool.execute.before"));
    // The installed shim spawns the gate command as a JSON argv array.
    // On Windows the program token is the absolute exe path; match the gate
    // subcommand tail of the argv array, not the program element.
    assert!(text.contains(r#","hook","opencode"]"#));
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
    // Copilot wires its own file under .github/hooks, never the other harnesses'.
    assert!(!dir.path().join(".claude/settings.json").exists());
    assert!(!dir.path().join(".cursor/hooks.json").exists());
    let hooks = dir.path().join(".github/hooks/allowlister.json");
    assert!(hooks.is_file(), "the copilot hook must be auto-registered");
    let doc: Value = serde_json::from_str(&fs::read_to_string(hooks).unwrap()).unwrap();
    assert_eq!(doc["version"], 1);
    assert!(doc["hooks"]["preToolUse"][0]["bash"]
        .as_str()
        .unwrap()
        .ends_with("hook copilot"));
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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
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
    let raw = fs::read_to_string(dir.path().join(".allowlister.jsonc")).unwrap();
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
    let raw = fs::read_to_string(dir.path().join(".allowlister.jsonc")).unwrap();
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
    assert!(doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("hook claude-code"));
}

#[test]
fn init_keeps_existing_config_and_still_wires_the_hook() {
    // Re-running `init` over an existing config (no --force) is idempotent: the
    // config is kept verbatim and the hook is (re-)registered, never an error.
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".allowlister.json"), "{}").unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("already exists"))
        .stdout(predicate::str::contains("--force"));
    // The existing config survives byte-for-byte; the requested profile is not applied.
    assert_eq!(
        fs::read_to_string(dir.path().join(".allowlister.json")).unwrap(),
        "{}",
        "the existing config must not be clobbered"
    );
    // The hook is still wired even though the config was kept.
    assert!(dir.path().join(".claude/settings.json").is_file());
}

#[test]
fn init_second_harness_after_a_first_keeps_config_and_wires_both_hooks() {
    // The motivating journey: init one harness, then init a *different* harness
    // against the same config. The second run must keep the config from the first
    // and wire the new harness's hook alongside it — no --force, no clobber.
    let dir = TempDir::new().unwrap();

    // First harness: Claude Code. Writes the config and wires its hook.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "claude-code"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote"));
    let config = dir.path().join(".allowlister.jsonc");
    assert!(config.is_file());
    let after_first = fs::read_to_string(&config).unwrap();
    assert!(dir.path().join(".claude/settings.json").is_file());

    // Second harness: Cursor. The config already exists, so it is kept as-is and
    // only Cursor's hook is added.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--harness", "cursor"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("already exists"))
        .stdout(predicate::str::contains("allowlister hook cursor"));

    // The config from the first init is untouched, and both harness hooks exist.
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        after_first,
        "the second harness init must not rewrite the config"
    );
    assert!(
        dir.path().join(".claude/settings.json").is_file(),
        "the first harness hook must remain"
    );
    let cursor = dir.path().join(".cursor/hooks.json");
    assert!(cursor.is_file(), "the second harness hook must be wired");
    let doc: Value = serde_json::from_str(&fs::read_to_string(cursor).unwrap()).unwrap();
    assert!(doc["hooks"]["beforeShellExecution"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("hook cursor"));
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
    assert!(xdg.path().join("allowlister/config.jsonc").is_file());
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
        .join(".config/allowlister/config.jsonc")
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
    assert!(xdg.path().join("allowlister/config.jsonc").is_file());

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
    assert!(dir.path().join(".allowlister.jsonc").is_file());
}

#[test]
fn install_into_an_existing_json_config_updates_it_in_place_keeping_comments() {
    let dir = TempDir::new().unwrap();
    // A legacy-named, hand-commented project config: `--local` must keep
    // updating this file (no .jsonc twin appears) and keep its comments.
    let existing = dir.path().join(".allowlister.json");
    fs::write(
        &existing,
        "{\n  // hand-written note\n  \"rules\": [\n    { \"name\": \"mine\", \"match\": \"ls*\", \"action\": \"allow\" } // keep\n  ]\n}\n",
    )
    .unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "starter", "--local"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated"));
    assert!(
        !dir.path().join(".allowlister.jsonc").exists(),
        "the existing .json config is the update target, not a new .jsonc"
    );
    let text = fs::read_to_string(&existing).unwrap();
    // The comment keeps its exact position; a leading "$schema" is backfilled
    // before the rules, and the separating comma attaches to the rule, before
    // its trailing comment. Everything else is byte-for-byte untouched.
    let expected_prefix = format!(
        "{{\n  // hand-written note\n  \"$schema\": \"{url}\",\n  \"rules\": [\n    {{ \"name\": \"mine\", \"match\": \"ls*\", \"action\": \"allow\" }}, // keep\n",
        url = allowlister::config::SCHEMA_URL,
    );
    assert!(
        text.starts_with(&expected_prefix),
        "comments must keep their positions: {text}"
    );
    let doc: Value =
        serde_json::from_str(&allowlister::config::strip_jsonc_comments(&text)).unwrap();
    assert_eq!(doc["rules"][0]["name"], "mine");
    assert!(doc["rules"].as_array().unwrap().len() > 1);

    // A second, fully redundant install must leave the file byte-identical.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "starter", "--local"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("0 rule(s) added"));
    assert_eq!(fs::read_to_string(&existing).unwrap(), text);
}

#[test]
fn init_history_keeps_a_commented_profiles_comments_in_place() {
    let dir = TempDir::new().unwrap();
    // A commented profile file: `init` writes it verbatim, then persisting the
    // history toggle must splice the `history` member in without disturbing a
    // single byte of what came before it.
    let profile = dir.path().join("team.jsonc");
    let body = "{\n  // team notes\n  \"rules\": [\n    { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" } // why\n  ]\n}\n";
    fs::write(&profile, body).unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args([
            "init",
            "--local",
            "--no-hooks",
            "--history",
            "-y",
            "--profile",
        ])
        .arg(&profile)
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("history recording is ON"));
    let written = fs::read_to_string(dir.path().join(".allowlister.jsonc")).unwrap();
    // The profile's comment survives; init backfills a leading "$schema", then
    // the history member is spliced in at the end — every original byte else
    // keeps its position.
    let expected = format!(
        "{{\n  // team notes\n  \"$schema\": \"{url}\",\n  \"rules\": [\n    {{ \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" }} // why\n  ],\n  \"history\": {{\n    \"enabled\": true\n  }}\n}}\n",
        url = allowlister::config::SCHEMA_URL,
    );
    assert_eq!(
        written, expected,
        "the profile text must survive byte-for-byte around the spliced $schema and history members"
    );
}

#[test]
fn a_jsonc_project_config_is_discovered_and_gates() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join(".allowlister.jsonc"),
        "{\n  // comments are first-class here\n  \"rules\": [\n    { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" }\n  ]\n}\n",
    )
    .unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["check", "ls -la", "--cwd"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
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

// ---- $schema stamping: init/install write the published schema key ----------
//
// init and install stamp the canonical "$schema" onto the configs they write, so
// a file validates and autocompletes in an editor out of the box. New files lead
// with it; an existing file is backfilled only when it lacks one (a user's own
// value is never overwritten or duplicated); and the "kept as-is" init path adds
// nothing. The key is inert to the engine, so a stamped config still gates.

/// Count the top-level `"$schema"` keys in a config's text.
fn schema_key_count(text: &str) -> usize {
    text.matches("\"$schema\"").count()
}

/// Parse a (possibly commented) config file into a JSON value.
fn read_config_doc(path: &Path) -> Value {
    let raw = fs::read_to_string(path).unwrap();
    serde_json::from_str(&allowlister::config::strip_jsonc_comments(&raw)).unwrap()
}

#[test]
fn install_new_config_leads_with_the_schema_key() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("cfg.jsonc");
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "read-only", "--output"])
        .arg(&out)
        .assert()
        .success();
    let text = fs::read_to_string(&out).unwrap();
    assert!(
        text.starts_with(&format!(
            "{{\n  \"$schema\": \"{}\",\n",
            allowlister::config::SCHEMA_URL
        )),
        "a fresh config must lead with the $schema key: {text}"
    );
    assert_eq!(schema_key_count(&text), 1, "exactly one $schema key");
    assert_eq!(
        read_config_doc(&out)["$schema"],
        allowlister::config::SCHEMA_URL
    );
}

#[test]
fn install_local_new_config_is_stamped_and_still_gates() {
    let dir = TempDir::new().unwrap();
    // A `.git` marker stops project-config discovery at this directory.
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "read-only", "--local"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"));
    let config = dir.path().join(".allowlister.jsonc");
    assert_eq!(
        read_config_doc(&config)["$schema"],
        allowlister::config::SCHEMA_URL
    );
    // The inert key does not disturb gating: a pure read still allows, proving the
    // stamped config loads cleanly through the real engine.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["check", "git status", "--cwd"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn install_from_a_file_source_stamps_the_schema_key() {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("custom.json");
    // A user's own ruleset with no $schema of its own.
    fs::write(
        &src,
        r#"{"rules":[{"name":"mine","match":"my_tool *","action":"allow"}]}"#,
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
        .success();
    let doc = read_config_doc(&out);
    assert_eq!(doc["$schema"], allowlister::config::SCHEMA_URL);
    assert_eq!(
        doc["rules"][0]["name"], "mine",
        "the source's rule survives"
    );
}

#[test]
fn install_backfills_the_schema_key_on_an_uncommented_existing_config() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("cfg.json");
    fs::write(
        &out,
        r#"{"rules":[{"name":"keep","match":"ls*","action":"allow"}]}"#,
    )
    .unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "read-only", "--output"])
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated"));
    let text = fs::read_to_string(&out).unwrap();
    assert_eq!(
        schema_key_count(&text),
        1,
        "exactly one $schema key was added"
    );
    let doc = read_config_doc(&out);
    assert_eq!(doc["$schema"], allowlister::config::SCHEMA_URL);
    let names: Vec<&str> = doc["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"keep"), "the existing rule survives");
    assert!(names.len() > 30, "the profile rules were merged in");
}

#[test]
fn install_leaves_an_existing_custom_schema_untouched() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("cfg.json");
    let custom = "https://example.com/my-own.schema.json";
    fs::write(
        &out,
        format!(
            r#"{{"$schema":"{custom}","rules":[{{"name":"mine","match":"ls*","action":"allow"}}]}}"#
        ),
    )
    .unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "read-only", "--output"])
        .arg(&out)
        .assert()
        .success();
    let text = fs::read_to_string(&out).unwrap();
    // The user's own $schema is preserved, never overwritten with ours, and never
    // duplicated by a second key.
    assert_eq!(schema_key_count(&text), 1, "no duplicate $schema key");
    assert_eq!(
        read_config_doc(&out)["$schema"],
        custom,
        "an existing $schema is left exactly as the user set it"
    );
}

#[test]
fn install_does_not_duplicate_the_schema_key_on_reinstall() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("cfg.jsonc");
    let run = || {
        Command::cargo_bin("allowlister")
            .unwrap()
            .args(["install", "read-only", "--output"])
            .arg(&out)
            .assert()
            .success();
    };
    run();
    let after_first = fs::read_to_string(&out).unwrap();
    run();
    let after_second = fs::read_to_string(&out).unwrap();
    // A redundant re-install (rules present, $schema present) leaves the file
    // byte-identical with a single $schema key.
    assert_eq!(after_first, after_second, "re-install is a no-op write");
    assert_eq!(schema_key_count(&after_second), 1);
}

#[test]
fn init_local_default_config_is_stamped_with_the_schema_key() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success();
    let config = dir.path().join(".allowlister.jsonc");
    let text = fs::read_to_string(&config).unwrap();
    assert!(
        text.starts_with(&format!(
            "{{\n  \"$schema\": \"{}\",\n",
            allowlister::config::SCHEMA_URL
        )),
        "the default starter config must lead with $schema: {text}"
    );
    assert_eq!(schema_key_count(&text), 1);
}

#[test]
fn init_force_overwrite_stamps_the_schema_key() {
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
    let doc = read_config_doc(&dir.path().join(".allowlister.json"));
    assert_eq!(
        doc["$schema"],
        allowlister::config::SCHEMA_URL,
        "a forced overwrite writes a stamped config"
    );
}

#[test]
fn init_kept_as_is_config_is_not_backfilled_with_a_schema_key() {
    // `init` over an existing config (no --force) keeps it byte-for-byte and only
    // wires the hook — it never touches the config, so it adds no $schema either.
    let dir = TempDir::new().unwrap();
    let config = dir.path().join(".allowlister.jsonc");
    let original = r#"{"rules":[{"name":"keep","match":"ls*","action":"allow"}]}"#;
    fs::write(&config, original).unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("already exists"));
    let text = fs::read_to_string(&config).unwrap();
    assert_eq!(text, original, "the kept config is untouched");
    assert_eq!(schema_key_count(&text), 0, "no $schema is forced onto it");
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
    // Time survives aggregation: the report carries its decay anchor and every
    // row keeps first/last use plus a fresh (just-recorded) recency weight.
    assert!(value["as_of"].as_u64().unwrap() > 0);
    let row = value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "some_unknown_tool --flag")
        .unwrap();
    assert!(row["last_ts"].as_u64().unwrap() > 0, "{row}");
    assert!(row["first_ts"].as_u64().unwrap() > 0, "{row}");
    assert!(row["recent_total"].as_f64().unwrap() > 0.9, "{row}");
    assert!(row["recent"]["defer"].as_f64().unwrap() > 0.9, "{row}");

    // The text report names a deferred subcommand, shows the recency columns,
    // and offers the refine tip.
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
    assert!(text.contains("RECENT"), "{text}");
    assert!(text.contains("LAST"), "{text}");
    assert!(text.contains("<1h"), "{text}");
    assert!(text.contains("Tip:"), "{text}");
}

#[test]
fn history_reports_the_project_dimension() {
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

    // The default report carries the per-subcommand project-count column.
    sandbox
        .command()
        .args(["history"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PROJECTS"));

    // `--by-project --json` exposes the full per-project verdict breakdown. Both
    // events ran in the one sandbox project, so every row names exactly one.
    let out = sandbox
        .command()
        .args(["history", "--by-project", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    let rows = value["rows"].as_array().unwrap();
    assert!(!rows.is_empty());
    for row in rows {
        assert_eq!(row["project_count"], 1, "{row}");
        let projects = row["projects"].as_object().unwrap();
        assert_eq!(projects.len(), 1, "{row}");
        let counts = projects.values().next().unwrap();
        assert!(counts["allow"].as_u64().unwrap() + counts["defer"].as_u64().unwrap() > 0);
    }
}

/// A shared user-global store (XDG home) whose user config allows `ls`, so the
/// git-identity history tests record an `allow` for every `ls` they run.
fn history_xdg() -> TempDir {
    let xdg = TempDir::new().unwrap();
    let dir = xdg.path().join("allowlister");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("config.json"),
        r#"{"rules":[{"name":"ls","match":"ls*","action":"allow"}]}"#,
    )
    .unwrap();
    xdg
}

/// A git checkout whose `.git/config` names `origin` = `remote` (no remote when
/// `remote` is empty).
fn git_checkout(remote: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let git = dir.path().join(".git");
    fs::create_dir_all(&git).unwrap();
    let body = if remote.is_empty() {
        "[core]\n\tbare = false\n".to_string()
    } else {
        format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {remote}\n")
    };
    fs::write(git.join("config"), body).unwrap();
    dir
}

/// Record one `claude-code` hook evaluation of `command` run in `cwd`, into the
/// history store under `xdg`.
fn record_in(xdg: &Path, cwd: &Path, command: &str) {
    let payload = format!(
        r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":{}}},"cwd":{}}}"#,
        serde_json::to_string(command).unwrap(),
        serde_json::to_string(&cwd.to_string_lossy()).unwrap()
    );
    Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", xdg)
        .env("ALLOWLISTER_HISTORY", "1")
        .args(["hook", "claude-code"])
        .write_stdin(payload)
        .assert()
        .success();
}

/// The `--by-project --json` `projects` map for the `ls -la` row — the per-project
/// tags the store recorded.
fn ls_projects(xdg: &Path) -> serde_json::Map<String, Value> {
    let out = Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", xdg)
        .args(["history", "--by-project", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "ls -la")
        .expect("an `ls -la` row")["projects"]
        .as_object()
        .expect("a projects map under --by-project")
        .clone()
}

#[test]
fn history_aggregates_clones_of_one_repo_by_remote() {
    // Two separate checkouts of the same repository (one origin remote, two
    // different folders) must collapse to a single project in the user-global
    // store — the whole point of git-based tracking.
    let xdg = history_xdg();
    let clone_a = git_checkout("https://github.com/octocat/Hello-World.git");
    let clone_b = git_checkout("https://github.com/octocat/Hello-World.git");
    record_in(xdg.path(), clone_a.path(), "ls -la");
    record_in(xdg.path(), clone_b.path(), "ls -la");

    // Both folders report under the one repository identity, with both runs.
    let projects = ls_projects(xdg.path());
    assert_eq!(projects.len(), 1, "{projects:?}");
    assert_eq!(
        projects["github.com/octocat/Hello-World"]["allow"], 2,
        "both clones aggregate to the remote identity: {projects:?}"
    );
}

#[test]
fn history_keeps_distinct_repos_and_non_git_dirs_separate() {
    // The flip side of aggregation: different repositories — and a directory that
    // is not a repository at all — must stay distinct, so project breadth is not
    // silently collapsed.
    let xdg = history_xdg();
    let repo_x = git_checkout("https://github.com/octocat/Hello-World.git");
    let repo_y = git_checkout("git@gitlab.com:group/other.git");
    let plain = TempDir::new().unwrap(); // no `.git`: a non-repo folder

    record_in(xdg.path(), repo_x.path(), "ls -la");
    record_in(xdg.path(), repo_y.path(), "ls -la");
    record_in(xdg.path(), plain.path(), "ls -la");

    let projects = ls_projects(xdg.path());
    // Two repos keyed by remote identity, plus the non-git folder by its path.
    assert_eq!(projects.len(), 3, "{projects:?}");
    assert!(projects.contains_key("github.com/octocat/Hello-World"));
    assert!(projects.contains_key("gitlab.com/group/other"));
    // A non-git cwd keeps its literal folder tag (the path the harness passed,
    // unchanged — the fallback never rewrites it).
    let folder = plain.path().to_string_lossy().into_owned();
    assert!(
        projects.contains_key(&folder),
        "non-git cwd keeps the folder tag: {projects:?}"
    );
}

#[test]
fn history_tags_a_subdirectory_by_its_repo() {
    // A command run deep inside a checkout must walk up to the repo and tag by its
    // identity — not by the subdirectory it happened to run in.
    let xdg = history_xdg();
    let repo = git_checkout("https://github.com/octocat/Hello-World.git");
    let nested = repo.path().join("crates/core/src");
    fs::create_dir_all(&nested).unwrap();

    record_in(xdg.path(), &nested, "ls -la");

    let projects = ls_projects(xdg.path());
    assert_eq!(projects.len(), 1, "{projects:?}");
    assert!(
        projects.contains_key("github.com/octocat/Hello-World"),
        "a subdirectory still tags as the one repository: {projects:?}"
    );
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
    let config = fs::read_to_string(xdg.path().join("allowlister/config.jsonc")).unwrap();
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

// ---- broken config: skipped with a warning, never a deny or a crash --------
//
// Config loading never fails the caller: a malformed file is skipped and
// recorded as a warning. These pin the user-visible halves of that contract —
// remaining valid configs still gate, nothing escalates to a deny, and
// `explain` names the skipped file.

#[test]
fn check_broken_project_config_is_skipped_but_user_rules_still_gate() {
    let sandbox = Sandbox::new();
    fs::write(sandbox.cwd().join(".allowlister.json"), "{ not json").unwrap();
    // The user config's deny still fires...
    sandbox
        .command()
        .args(["check", "rm -rf /", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .code(2)
        .stdout(predicate::str::starts_with("DENY"));
    // ...while a command only the (now skipped) project config allowed defers.
    sandbox
        .command()
        .args(["check", "npm list", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("DEFER"));
}

#[test]
fn check_with_only_a_broken_config_defers_everything() {
    let empty = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join(".git")).unwrap();
    fs::write(project.path().join(".allowlister.json"), "{ not json").unwrap();
    // No usable rules anywhere: even a scary command defers (exit 0), it is
    // never denied on our own config error.
    hermetic_cmd(&empty)
        .args(["check", "rm -rf /", "--cwd"])
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("DEFER"));
}

#[test]
fn claude_hook_broken_config_defers_not_denies() {
    let sandbox = Sandbox::new();
    fs::write(sandbox.cwd().join(".allowlister.json"), "{ not json").unwrap();
    // Hide the user config too, so no rules load at all.
    let empty = TempDir::new().unwrap();
    let output = sandbox
        .command()
        .env("XDG_CONFIG_HOME", empty.path())
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "defer");
}

#[test]
fn goose_hook_broken_config_emits_nothing() {
    let sandbox = Sandbox::new();
    fs::write(sandbox.cwd().join(".allowlister.json"), "{ not json").unwrap();
    let empty = TempDir::new().unwrap();
    // Goose blocks only on a `block` JSON: with no usable config the verdict
    // defers and stdout stays empty — a config error can never become a block.
    sandbox
        .command()
        .env("XDG_CONFIG_HOME", empty.path())
        .args(["hook", "goose"])
        .write_stdin(sandbox.goose_payload("rm -rf /"))
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty());
}

#[test]
fn explain_names_the_skipped_config_and_warns() {
    let sandbox = Sandbox::new();
    fs::write(sandbox.cwd().join(".allowlister.json"), "{ not json").unwrap();
    sandbox
        .command()
        .args(["explain", "ls", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::contains("skipped: invalid JSON"))
        .stdout(predicate::str::contains("warnings:"));
}

// ---- hook stdin edge cases --------------------------------------------------

#[test]
fn hook_empty_stdin_exits_one_and_writes_nothing_to_stdout() {
    // EOF with no payload is the same fail-open path as malformed JSON for the
    // Claude adapter: a stderr note and a non-blocking exit, never a decision.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "claude-code"])
        .write_stdin("")
        .assert()
        .code(1)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

#[test]
fn goose_hook_empty_stdin_exits_zero_and_writes_nothing_to_stdout() {
    // Goose treats exit 2 as a block, so an empty payload must exit 0 with
    // empty stdout — a true no-op.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "goose"])
        .write_stdin("")
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("invalid hook JSON"));
}

// ---- check usage errors -----------------------------------------------------
//
// clap exits 2 on a usage error, the same code `check` uses for a deny. The
// channels disambiguate: a deny prints the verdict on stdout, a usage error
// prints only to stderr. Pin both halves so scripted callers can rely on it.

#[test]
fn check_without_command_or_tool_is_a_usage_error_with_empty_stdout() {
    Command::cargo_bin("allowlister")
        .unwrap()
        .arg("check")
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("required"));
}

#[test]
fn check_with_both_command_and_tool_is_a_usage_error_with_empty_stdout() {
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["check", "ls", "--tool", "read"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("cannot be used with"));
}

// ---- tool-rule schema through the binary: --raw, jsonpath, capabilities -----

#[test]
fn check_tool_raw_jsonpath_rule_denies_matching_server_param() {
    let empty = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join(".git")).unwrap();
    fs::write(
        project.path().join(".allowlister.json"),
        r#"{"rules":[{"name":"no evilcorp","tool":"mcp__github__*","action":"deny","jsonpath":{"owner":["evilcorp"]}}]}"#,
    )
    .unwrap();
    // The raw JSON carries the server-defined param the jsonpath rule reads.
    hermetic_cmd(&empty)
        .args([
            "check",
            "--tool",
            "mcp__github__create_issue",
            "--raw",
            r#"{"owner":"evilcorp"}"#,
            "--cwd",
        ])
        .arg(project.path())
        .assert()
        .code(2)
        .stdout(predicate::str::starts_with("DENY"));
    // A non-matching owner falls outside the rule and defers.
    hermetic_cmd(&empty)
        .args([
            "check",
            "--tool",
            "mcp__github__create_issue",
            "--raw",
            r#"{"owner":"acme"}"#,
            "--cwd",
        ])
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("DEFER"));
}

#[test]
fn check_tool_raw_invalid_json_is_a_usage_error() {
    let empty = TempDir::new().unwrap();
    hermetic_cmd(&empty)
        .args(["check", "--tool", "mcp", "--raw", "{not json"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("not valid JSON"));
}

#[test]
fn check_tool_mcp_portable_rule_allows_read_only_tool() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    // The allow half of the portable MCP pair: list*/get* on the linear server.
    hermetic_cmd(&empty)
        .args(["check", "--tool", "mcp__linear__list_issues", "--cwd"])
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn check_tool_web_fetch_url_rules_allow_and_defer() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    hermetic_cmd(&empty)
        .args([
            "check",
            "--tool",
            "web_fetch",
            "--param",
            "url=https://github.com/acme/repo",
            "--cwd",
        ])
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
    hermetic_cmd(&empty)
        .args([
            "check",
            "--tool",
            "web_fetch",
            "--param",
            "url=https://evil.example.com/x",
            "--cwd",
        ])
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("DEFER"));
}

/// A project dir whose config exercises the remaining capabilities: `write`,
/// `edit`, `glob`, `grep`, and `web_search`.
fn capability_project() -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let edit_glob = format!("{}/**", dir.path().to_string_lossy());
    let cfg = serde_json::json!({
        "rules": [
            { "name": "no env files", "tool": "write", "action": "deny",
              "params": { "path": ["**/*.env"] } },
            { "name": "edits in repo", "tool": "edit", "action": "allow",
              "params": { "path": [edit_glob] } },
            { "name": "globbing is free", "tool": "glob", "action": "allow" },
            { "name": "grepping is free", "tool": "grep", "action": "allow" },
            { "name": "confirm searches", "tool": "web_search", "action": "ask" }
        ]
    })
    .to_string();
    fs::write(dir.path().join(".allowlister.json"), cfg).unwrap();
    dir
}

#[test]
fn check_tool_write_edit_glob_grep_web_search_capabilities() {
    let empty = TempDir::new().unwrap();
    let project = capability_project();
    hermetic_cmd(&empty)
        .args([
            "check",
            "--tool",
            "write",
            "--param",
            "path=/app/prod/.env",
            "--cwd",
        ])
        .arg(project.path())
        .assert()
        .code(2)
        .stdout(predicate::str::starts_with("DENY"));
    let inside = format!("path={}/src/a.rs", project.path().to_string_lossy());
    hermetic_cmd(&empty)
        .args(["check", "--tool", "edit", "--param"])
        .arg(&inside)
        .arg("--cwd")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
    // A constraint-free capability rule matches every call of that capability.
    for tool in ["glob", "grep"] {
        hermetic_cmd(&empty)
            .args(["check", "--tool", tool, "--param", "pattern=TODO", "--cwd"])
            .arg(project.path())
            .assert()
            .success()
            .stdout(predicate::str::starts_with("ALLOW"));
    }
    hermetic_cmd(&empty)
        .args([
            "check",
            "--tool",
            "web_search",
            "--param",
            "query=anything",
            "--cwd",
        ])
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ASK"));
}

#[test]
fn check_tool_json_emits_machine_readable_object() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    let output = hermetic_cmd(&empty)
        .args([
            "check",
            "--tool",
            "read",
            "--param",
            "path=/home/u/.ssh/id_rsa",
            "--json",
            "--cwd",
        ])
        .arg(project.path())
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["verdict"], "deny");
    assert!(value["reason"].as_str().unwrap().contains("no secrets"));
}

// ---- config discovery and precedence ----------------------------------------

#[test]
fn check_discovers_project_config_from_nested_subdirectory() {
    let sandbox = Sandbox::new();
    // Discovery must walk up from a nested cwd to the `.git` root and pick up
    // the project config there ("npm list" matches a project rule).
    let nested = sandbox.cwd().join("src").join("deep");
    fs::create_dir_all(&nested).unwrap();
    sandbox
        .command()
        .args(["check", "npm list", "--cwd"])
        .arg(&nested)
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn nested_project_config_adds_rules_below_the_root() {
    let sandbox = Sandbox::new();
    let nested = sandbox.cwd().join("services").join("api");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join(".allowlister.json"),
        r#"{"rules":[{"name":"api tool","match":"my_custom_tool *","action":"allow"}]}"#,
    )
    .unwrap();
    // The nested config applies from inside its directory...
    sandbox
        .command()
        .args(["check", "my_custom_tool --serve", "--cwd"])
        .arg(&nested)
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
    // ...but not from the project root above it.
    sandbox
        .command()
        .args(["check", "my_custom_tool --serve", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("DEFER"));
}

#[test]
fn project_deny_wins_over_user_allow() {
    // The verdict is set-theoretic: any deny denies, regardless of merge order.
    let xdg = TempDir::new().unwrap();
    let allowlister_dir = xdg.path().join("allowlister");
    fs::create_dir_all(&allowlister_dir).unwrap();
    fs::write(
        allowlister_dir.join("config.json"),
        r#"{"rules":[{"name":"user allows","match":"danger_tool *","action":"allow"}]}"#,
    )
    .unwrap();
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join(".git")).unwrap();
    fs::write(
        project.path().join(".allowlister.json"),
        r#"{"rules":[{"name":"project denies","match":"danger_tool *","action":"deny"}]}"#,
    )
    .unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("HOME", project.path())
        .args(["check", "danger_tool --run", "--cwd"])
        .arg(project.path())
        .assert()
        .code(2)
        .stdout(predicate::str::starts_with("DENY"))
        .stdout(predicate::str::contains("project denies"));
}

#[test]
fn hook_pipeline_with_one_denied_fragment_denies_the_whole_command() {
    let sandbox = Sandbox::new();
    // The source fragment is allowed on its own; the denied filter fragment
    // must drag the composed verdict to deny through the binary boundary.
    let output = sandbox
        .command()
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("gh pr list | rm -rf /"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "deny");
}

// ---- non-shell tools on the remaining adapters --------------------------------

#[test]
fn codex_hook_apply_patch_gated_by_capability_edit_rule() {
    let empty = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join(".git")).unwrap();
    fs::write(
        project.path().join(".allowlister.json"),
        r#"{"rules":[{"name":"no edits","tool":"edit","action":"deny"}]}"#,
    )
    .unwrap();
    // `apply_patch` carries no discrete path, so only a capability-only `edit`
    // rule can gate it.
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "apply_patch",
        "tool_input": { "command": "*** Begin Patch" },
        "cwd": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "codex"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(decision_of(&output), "deny");
}

#[test]
fn codex_hook_apply_patch_allow_emits_empty_stdout() {
    let empty = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join(".git")).unwrap();
    fs::write(
        project.path().join(".allowlister.json"),
        r#"{"rules":[{"name":"edits ok","tool":"edit","action":"allow"}]}"#,
    )
    .unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "apply_patch",
        "tool_input": { "command": "*** Begin Patch" },
        "cwd": project.path().to_string_lossy(),
    })
    .to_string();
    // Codex rejects a bare allow, so an allowed edit is a no-op fall-through.
    hermetic_cmd(&empty)
        .args(["hook", "codex"])
        .write_stdin(payload)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn claude_hook_write_tool_denies_env_file_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = capability_project();
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "tool_input": { "file_path": "/app/prod/.env", "content": "SECRET=1" },
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
    assert_eq!(decision_of(&output), "deny");
}

#[test]
fn claude_hook_edit_tool_allows_inside_repo_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = capability_project();
    let file = format!("{}/src/lib.rs", project.path().to_string_lossy());
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Edit",
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
fn claude_hook_web_fetch_allows_github_url_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "WebFetch",
        "tool_input": { "url": "https://github.com/acme/repo" },
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
fn claude_hook_mcp_tool_allows_read_only_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "mcp__linear__list_issues",
        "tool_input": {},
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
fn crush_hook_view_tool_denies_secret_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    // Crush's read tool is `view` with `file_path`.
    let payload = serde_json::json!({
        "event": "PreToolUse",
        "tool_name": "view",
        "tool_input": { "file_path": "/home/u/.ssh/id_rsa" },
        "cwd": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "crush"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(crush_decision_of(&output), "deny");
}

#[test]
fn crush_hook_single_underscore_mcp_denies_destructive_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    // Crush names MCP tools `mcp_<server>_<tool>` (single underscores).
    let payload = serde_json::json!({
        "event": "PreToolUse",
        "tool_name": "mcp_linear_delete_issue",
        "tool_input": { "id": "1" },
        "cwd": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "crush"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(crush_decision_of(&output), "deny");
}

#[test]
fn goose_hook_bare_shell_tool_is_gated() {
    let sandbox = Sandbox::new();
    // Goose's builtin developer extension exposes the shell as a bare `shell`
    // (no `developer__` prefix); the gate must fire on it too.
    let payload = format!(
        r#"{{"event":"PreToolUse","tool_name":"shell","tool_input":{{"command":"rm -rf /"}},"working_dir":{}}}"#,
        serde_json::to_string(&sandbox.cwd().to_string_lossy()).unwrap()
    );
    let output = sandbox
        .command()
        .args(["hook", "goose"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(goose_decision_of(&output), "block");
}

#[test]
fn goose_hook_text_editor_view_denies_secret_read() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    // Older Goose's multi-purpose `text_editor`: `command: view` is a read.
    let payload = serde_json::json!({
        "event": "PreToolUse",
        "tool_name": "developer__text_editor",
        "tool_input": { "command": "view", "path": "/home/u/.ssh/id_rsa" },
        "working_dir": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "goose"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(goose_decision_of(&output), "block");
}

#[test]
fn goose_hook_bare_write_tool_denies_env_file() {
    let empty = TempDir::new().unwrap();
    let project = capability_project();
    // Goose delivers developer file tools under bare names with a `path` key.
    let payload = serde_json::json!({
        "event": "PreToolUse",
        "tool_name": "write",
        "tool_input": { "path": "/app/prod/.env", "content": "SECRET=1" },
        "working_dir": project.path().to_string_lossy(),
    })
    .to_string();
    let output = hermetic_cmd(&empty)
        .args(["hook", "goose"])
        .write_stdin(payload)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(goose_decision_of(&output), "block");
}

#[test]
fn cursor_before_mcp_execution_allows_read_only_via_stdin() {
    let empty = TempDir::new().unwrap();
    let project = tool_project();
    let payload = serde_json::json!({
        "hook_event_name": "beforeMCPExecution",
        "tool_name": "mcp__linear__list_issues",
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
    assert_eq!(permission_of(&output), "allow");
}

// ---- init: global registration per harness, file profiles, idempotency ------

#[test]
fn init_global_registers_each_harness_under_home_or_xdg() {
    // Each harness wires a different user-level file; the allowlister config
    // itself always lands under XDG. (claude-code's global path is covered by
    // its own test above.)
    let cases: &[(&str, &[&str], bool)] = &[
        ("cursor", &["home", ".cursor", "hooks.json"], false),
        ("codex", &["home", ".codex", "hooks.json"], false),
        ("crush", &["xdg", "crush", "crush.json"], false),
        ("qwen", &["home", ".qwen", "settings.json"], false),
        (
            "goose",
            &[
                "home",
                ".agents",
                "plugins",
                "allowlister",
                "hooks",
                "hooks.json",
            ],
            false,
        ),
        (
            "opencode",
            &["xdg", "opencode", "plugin", "allowlister.js"],
            true,
        ),
        (
            "copilot",
            &["home", ".copilot", "hooks", "allowlister.json"],
            false,
        ),
    ];
    for (harness, hook_file, is_plugin) in cases {
        let dir = TempDir::new().unwrap();
        let xdg = dir.path().join("xdg");
        let home = dir.path().join("home");
        Command::cargo_bin("allowlister")
            .unwrap()
            .args(["init", "--global", "--harness", harness])
            .env("XDG_CONFIG_HOME", &xdg)
            .env("HOME", &home)
            .assert()
            .success();
        assert!(
            xdg.join("allowlister/config.jsonc").is_file(),
            "{harness}: the config must land under XDG"
        );
        let hook_path: PathBuf = hook_file
            .iter()
            .fold(dir.path().to_path_buf(), |p, seg| p.join(seg));
        assert!(
            hook_path.is_file(),
            "{harness}: the global hook file must be written at {}",
            hook_path.display()
        );
        let text = fs::read_to_string(&hook_path).unwrap();
        if *is_plugin {
            // The OpenCode plugin shim spawns the gate command as a JSON argv
            // array rather than embedding the spaced command string.
            assert!(
                text.contains(&format!(r#","hook","{harness}"]"#)),
                "{harness}: the plugin shim must spawn the right adapter"
            );
        } else {
            // Match the gate subcommand, not the program token: on Windows the
            // goose command is the absolute exe path, every other harness the bare
            // name.
            assert!(
                text.contains(&format!("hook {harness}")),
                "{harness}: the hook file must invoke the right adapter"
            );
            // Every non-plugin hook file is JSON a harness will parse.
            serde_json::from_str::<Value>(&text).unwrap();
        }
    }
}

#[test]
fn init_profile_from_a_file_writes_the_source_and_gates() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("custom-profile.json");
    fs::write(
        &source,
        r#"{"rules":[{"name":"my tool","match":"my_company_tool *","action":"allow"}]}"#,
    )
    .unwrap();
    let project = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--no-hooks", "--profile"])
        .arg(&source)
        .current_dir(project.path())
        .assert()
        .success();
    // The source's rules land as the project config, stamped with a leading
    // "$schema" so the new file validates in an editor.
    let written = fs::read_to_string(project.path().join(".allowlister.jsonc")).unwrap();
    let doc: Value =
        serde_json::from_str(&allowlister::config::strip_jsonc_comments(&written)).unwrap();
    assert_eq!(doc["$schema"], allowlister::config::SCHEMA_URL);
    assert_eq!(doc["rules"][0]["name"], "my tool");
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["check", "my_company_tool --run", "--cwd"])
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn init_repo_write_profile_installs_and_gates() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--profile", "repo-write", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("repo-write"));
    // repo-write includes the read-only base, so a pure read allows.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["check", "git status", "--cwd"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn init_interactive_history_yes_persists_the_toggle() {
    let dir = TempDir::new().unwrap();
    // Answers: 2 = project-local, 2 = read-only, n = skip hooks, y = history on.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--interactive"])
        .current_dir(dir.path())
        .write_stdin("2\n2\nn\ny\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Record a local history"))
        .stdout(predicate::str::contains("history recording is ON"));
    let raw = fs::read_to_string(dir.path().join(".allowlister.jsonc")).unwrap();
    let doc: Value =
        serde_json::from_str(&allowlister::config::strip_jsonc_comments(&raw)).unwrap();
    assert_eq!(doc["history"]["enabled"], true);
}

#[test]
fn init_force_rerun_does_not_duplicate_the_hook_registration() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local"])
        .current_dir(dir.path())
        .assert()
        .success();
    let settings_path = dir.path().join(".claude/settings.json");
    let before: Value = serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    // Re-running (forcing past the existing config) must report a hook no-op
    // and leave the settings byte-for-byte equivalent — no duplicate entries.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--force"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("already registered"));
    let after: Value = serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(before, after, "re-running init must not change settings");
}

#[test]
fn install_preserves_existing_custom_rules_and_history_toggle() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("cfg.json");
    fs::write(
        &out,
        r#"{"history":{"enabled":true},"rules":[{"name":"mine","match":"my_tool *","action":"allow"}]}"#,
    )
    .unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["install", "read-only", "--output"])
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("rule(s) added"));
    let raw = fs::read_to_string(&out).unwrap();
    let doc: Value =
        serde_json::from_str(&allowlister::config::strip_jsonc_comments(&raw)).unwrap();
    assert_eq!(doc["history"]["enabled"], true, "the toggle survives");
    let rules = doc["rules"].as_array().unwrap();
    assert!(
        rules.iter().any(|r| r["name"] == "mine"),
        "the custom rule survives the merge"
    );
    assert!(rules.len() > 30, "the profile rules were merged in");
}

// ---- history: env-off override, clear confirmation, bounds and filters ------

#[test]
fn history_env_zero_overrides_a_config_that_enables_recording() {
    let home = TempDir::new().unwrap();
    let xdg = TempDir::new().unwrap();
    let cmd = || {
        let mut c = Command::cargo_bin("allowlister").unwrap();
        c.env("XDG_CONFIG_HOME", xdg.path())
            .env("HOME", home.path());
        c
    };
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
        .success();
    // The config opts in, but the env kill switch must win.
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": "ls -la" },
        "cwd": home.path().to_string_lossy(),
    })
    .to_string();
    cmd()
        .env("ALLOWLISTER_HISTORY", "0")
        .args(["hook", "claude-code"])
        .write_stdin(payload)
        .assert()
        .success();
    cmd()
        .args(["history"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No usage recorded yet"));
}

#[test]
fn history_clear_without_yes_confirms_via_stdin() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .env("ALLOWLISTER_HISTORY", "1")
        .args(["hook", "claude-code"])
        .write_stdin(sandbox.payload("gh pr list | head -20"))
        .assert()
        .success();

    // Answering 'n' (or anything but yes) aborts and keeps the data.
    sandbox
        .command()
        .args(["history", "clear"])
        .write_stdin("n\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Aborted"));
    sandbox
        .command()
        .args(["history"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 event(s) recorded"));

    // Answering 'y' clears.
    sandbox
        .command()
        .args(["history", "clear"])
        .write_stdin("y\n")
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
fn history_top_bounds_rows_and_recent_verdict_filters() {
    let sandbox = Sandbox::new();
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

    // --top bounds the report rows.
    let out = sandbox
        .command()
        .args(["history", "--top", "1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["rows"].as_array().unwrap().len(), 1);

    // recent --verdict keeps only matching events; --top bounds them.
    let recent = sandbox
        .command()
        .args(["history", "recent", "--verdict", "deny", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let events: Value = serde_json::from_slice(&recent).unwrap();
    let events = events.as_array().unwrap();
    assert!(!events.is_empty());
    assert!(events.iter().all(|e| e["verdict"] == "deny"));

    let bounded = sandbox
        .command()
        .args(["history", "recent", "--top", "1", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let bounded: Value = serde_json::from_slice(&bounded).unwrap();
    assert_eq!(bounded.as_array().unwrap().len(), 1);
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
    let config = fs::read_to_string(project.path().join(".allowlister.jsonc")).unwrap();
    assert!(config.contains("\"enabled\": false"), "{config}");
}

// ---- config management (add / remove / show) -------------------------------
//
// These drive the compiled binary the way a user tuning their allowlist would:
// add a single rule, confirm it gates, remove it, confirm it stops gating, and
// show the effective merged config with each rule's source. Each runs in a
// hermetic temp dir so the host config never leaks in.

#[test]
fn config_add_local_creates_a_rule_that_gates() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args([
            "config", "add", "--local", "--name", "allow-ls", "--match", "ls*", "--action", "allow",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"))
        .stdout(predicate::str::contains("1 rule(s) added"));
    assert!(dir.path().join(".allowlister.jsonc").is_file());

    // The freshly added rule actually gates.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["check", "ls -la", "--cwd"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("ALLOW"));
}

#[test]
fn config_add_dedupes_by_name_and_preserves_comments() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("cfg.jsonc");
    fs::write(
        &target,
        "{\n  // hand notes\n  \"rules\": [\n    { \"name\": \"keep\", \"match\": \"ls*\", \"action\": \"allow\" } // why\n  ]\n}\n",
    )
    .unwrap();

    // Add a new rule via an explicit output path.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args([
            "config", "add", "--name", "pwd", "--match", "pwd", "--output",
        ])
        .arg(&target)
        .assert()
        .success()
        .stdout(predicate::str::contains("1 rule(s) added"));
    let text = fs::read_to_string(&target).unwrap();
    assert!(text.contains("// hand notes"), "comments survive: {text}");
    assert!(text.contains("// why"));

    // Re-adding the same name is a no-op.
    Command::cargo_bin("allowlister")
        .unwrap()
        .args([
            "config", "add", "--name", "pwd", "--match", "pwd", "--output",
        ])
        .arg(&target)
        .assert()
        .success()
        .stdout(predicate::str::contains("0 rule(s) added"));
    let doc: Value = serde_json::from_str(
        &allowlister::config::strip_jsonc_comments(&fs::read_to_string(&target).unwrap())
            .to_string(),
    )
    .unwrap();
    assert_eq!(doc["rules"].as_array().unwrap().len(), 2);
}

#[test]
fn config_add_rejects_an_invalid_rule() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("cfg.json");
    Command::cargo_bin("allowlister")
        .unwrap()
        .args([
            "config", "add", "--name", "bad", "--match", "x", "--role", "nope", "--output",
        ])
        .arg(&target)
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown role"));
    assert!(!target.exists(), "a bad rule writes nothing");
}

#[test]
fn config_add_tool_rule_with_a_param() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("cfg.json");
    Command::cargo_bin("allowlister")
        .unwrap()
        .args([
            "config",
            "add",
            "--name",
            "reads",
            "--tool",
            "read",
            "--param",
            "path=/repo/**",
            "--output",
        ])
        .arg(&target)
        .assert()
        .success();
    let doc: Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(doc["rules"][0]["tool"], "read");
    assert_eq!(doc["rules"][0]["params"]["path"][0], "/repo/**");
}

#[test]
fn config_remove_deletes_a_rule_and_stops_gating() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    let target = dir.path().join(".allowlister.jsonc");
    fs::write(
        &target,
        "{\n  \"rules\": [\n    { \"name\": \"allow-ls\", \"match\": \"ls*\", \"action\": \"allow\" },\n    { \"name\": \"allow-pwd\", \"match\": \"pwd\", \"action\": \"allow\" }\n  ]\n}\n",
    )
    .unwrap();

    // Isolate user-global config discovery so the host machine's real allowlister
    // config can never leak in and turn the final DEFER into an ALLOW. An empty,
    // dedicated config home leaves only the project config under test.
    let empty_home = TempDir::new().unwrap();

    Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", empty_home.path())
        .env("HOME", empty_home.path())
        .args(["config", "remove", "allow-ls", "--local"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed rule 'allow-ls'"));

    let doc: Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    let names: Vec<&str> = doc["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["allow-pwd"]);

    // With its rule gone, `ls` no longer matches an allow (and nothing else
    // does), so it defers.
    Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", empty_home.path())
        .env("HOME", empty_home.path())
        .args(["check", "ls -la", "--cwd"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("DEFER"));
}

#[test]
fn config_remove_preserves_surrounding_comments_and_formatting() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("cfg.jsonc");
    // A hand-commented config: removing the middle rule must leave the header
    // comment and both siblings' trailing comments byte-for-byte in place.
    fs::write(
        &target,
        "{\n  // team allowlist\n  \"rules\": [\n    { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" }, // safe\n    { \"name\": \"drop\", \"match\": \"drop*\", \"action\": \"allow\" },\n    { \"name\": \"pwd\", \"match\": \"pwd\", \"action\": \"allow\" } // also safe\n  ]\n}\n",
    )
    .unwrap();

    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["config", "remove", "drop", "--output"])
        .arg(&target)
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed rule 'drop'"));

    let text = fs::read_to_string(&target).unwrap();
    assert!(
        text.contains("// team allowlist"),
        "header survives: {text}"
    );
    assert!(
        text.contains("\"allow\" }, // safe"),
        "sibling comment survives: {text}"
    );
    assert!(
        text.contains("// also safe"),
        "sibling comment survives: {text}"
    );
    assert!(
        !text.contains("\"name\": \"drop\""),
        "the rule is gone: {text}"
    );
    // The surviving rules still parse and gate correctly.
    let doc: Value =
        serde_json::from_str(&allowlister::config::strip_jsonc_comments(&text)).unwrap();
    let names: Vec<&str> = doc["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["ls", "pwd"]);
}

#[test]
fn config_add_then_remove_global_under_xdg() {
    let xdg = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    // Add to the user-global config (resolves under XDG_CONFIG_HOME).
    Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("HOME", home.path())
        .args([
            "config", "add", "--global", "--name", "allow-ls", "--match", "ls*",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"));
    let config = xdg.path().join("allowlister/config.jsonc");
    assert!(config.is_file());

    // Remove it again from the same global config.
    Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("HOME", home.path())
        .args(["config", "remove", "allow-ls", "--global"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed rule 'allow-ls'"));
    let doc: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    assert!(doc["rules"].as_array().unwrap().is_empty());
}

#[test]
fn config_remove_absent_name_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("cfg.json");
    let body = r#"{"rules":[{"name":"a","match":"a*","action":"allow"}]}"#;
    fs::write(&target, body).unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["config", "remove", "nope", "--output"])
        .arg(&target)
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing changed"));
    assert_eq!(fs::read_to_string(&target).unwrap(), body);
}

#[test]
fn config_show_combined_lists_rules_with_sources() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["config", "show", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        // Both scopes contribute, and each rule is annotated with its source.
        .stdout(predicate::str::contains("combined (user + project)"))
        .stdout(predicate::str::contains("config.json"))
        .stdout(predicate::str::contains(".allowlister.json"))
        // A user rule and a project rule both appear.
        .stdout(predicate::str::contains("rm -rf"))
        .stdout(predicate::str::contains("gh api scoped to myorg"));
}

#[test]
fn config_show_json_is_machine_readable_with_per_rule_source() {
    let sandbox = Sandbox::new();
    let out = sandbox
        .command()
        .args(["config", "show", "--json", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["scope"], "combined (user + project)");
    let rules = value["rules"].as_array().unwrap();
    assert!(!rules.is_empty());
    // Every rule carries the file it came from.
    assert!(rules.iter().all(|r| r["source"].is_string()));
    // A project rule is attributed to the project config file.
    let proj_rule = rules
        .iter()
        .find(|r| r["name"] == "gh api scoped to myorg")
        .unwrap();
    assert!(proj_rule["source"]
        .as_str()
        .unwrap()
        .contains(".allowlister.json"));
}

#[test]
fn config_show_global_scope_only_shows_user_rules() {
    let sandbox = Sandbox::new();
    let out = sandbox
        .command()
        .args(["config", "show", "--global", "--json", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["scope"], "user-global");
    let rules = value["rules"].as_array().unwrap();
    // The user config's `rm -rf` deny is present; the project-only rule is not.
    assert!(rules.iter().any(|r| r["name"] == "rm -rf — never"));
    assert!(!rules.iter().any(|r| r["name"] == "gh api scoped to myorg"));
}

#[test]
fn config_show_surfaces_warnings_for_a_malformed_config() {
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join(".git")).unwrap();
    // Valid JSON, but a rule that cannot compile (no match/argv/tool) — the
    // loader records a warning and the raw read still lists what is configured.
    fs::write(
        dir.path().join(".allowlister.json"),
        r#"{"rules":[{"name":"broken","action":"allow"}]}"#,
    )
    .unwrap();
    let empty = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", empty.path())
        .env("HOME", empty.path())
        .args(["config", "show", "--cwd"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("warnings:"))
        .stdout(predicate::str::contains("broken"));
}

#[test]
fn config_show_local_scope_only_shows_project_rules() {
    let sandbox = Sandbox::new();
    let out = sandbox
        .command()
        .args(["config", "show", "--local", "--json", "--cwd"])
        .arg(sandbox.cwd())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["scope"], "project-local");
    let rules = value["rules"].as_array().unwrap();
    // Only project rules; the user-config-only `rm -rf` deny must be absent.
    assert!(rules.iter().any(|r| r["name"] == "gh api scoped to myorg"));
    assert!(!rules.iter().any(|r| r["name"] == "rm -rf — never"));
}

#[test]
fn config_show_empty_when_no_config_found() {
    let empty = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    fs::create_dir_all(cwd.path().join(".git")).unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .env("XDG_CONFIG_HOME", empty.path())
        .env("HOME", empty.path())
        .args(["config", "show", "--cwd"])
        .arg(cwd.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("(none found)"))
        .stdout(predicate::str::contains("rules (0)"));
}

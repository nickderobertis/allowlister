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
}

fn decision_of(stdout: &[u8]) -> String {
    let value: Value = serde_json::from_slice(stdout).expect("hook stdout must be valid JSON");
    value["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap()
        .to_string()
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
        .stdout(predicate::str::contains("init"));
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
fn hook_unimplemented_harness_errors() {
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "cursor"])
        .write_stdin("{}")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
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
fn init_local_writes_config_and_prints_snippet() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .arg("init")
        .arg("--local")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allowlister hook claude-code"))
        .stdout(predicate::str::contains("do NOT add"));
    assert!(dir.path().join(".allowlister.json").is_file());
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
fn init_global_writes_under_xdg() {
    let xdg = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .arg("init")
        .arg("--global")
        .env("XDG_CONFIG_HOME", xdg.path())
        .assert()
        .success();
    assert!(xdg.path().join("allowlister/config.json").is_file());
}

#[test]
fn init_global_falls_back_to_home_config() {
    let home = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .arg("init")
        .arg("--global")
        .env_remove("XDG_CONFIG_HOME")
        .env("HOME", home.path())
        .assert()
        .success();
    assert!(home
        .path()
        .join(".config/allowlister/config.json")
        .is_file());
}

#[test]
fn hook_copilot_is_unimplemented() {
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["hook", "copilot"])
        .write_stdin("{}")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not yet implemented"));
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

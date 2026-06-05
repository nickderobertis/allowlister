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
    assert_eq!(
        doc["hooks"]["beforeShellExecution"][0]["command"],
        "allowlister hook cursor"
    );
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
fn init_profile_installs_a_curated_ruleset() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("allowlister")
        .unwrap()
        .args(["init", "--local", "--profile", "read-only", "--no-hooks"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("read-only"));
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".allowlister.json")).unwrap())
            .unwrap();
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
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join(".allowlister.json")).unwrap())
            .unwrap();
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

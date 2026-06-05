//! `allowlister init` — set allowlister up: write a config from a chosen ruleset
//! and (by default) register the Bash hook in Claude Code's settings.json.
//!
//! One command does the whole first-time setup. It runs interactively — a
//! step-by-step flow that reads answers from stdin — or entirely from CLI flags.
//! Interactive mode engages when `--interactive` is passed, or when stdin is a
//! terminal and `--yes` was not; otherwise the flags and their defaults decide
//! everything, so the same command scripts cleanly in CI.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::cli::Harness;
use crate::commands::profile;
use crate::errors::{Error, Result};
use crate::io::configfs::{self, Env};
use crate::io::{claude_settings, cursor_settings};

/// Parsed `init` inputs. Scope/profile/hooks are each optional so the
/// interactive flow only fills in what the user did not already pin on the
/// command line.
pub struct InitArgs {
    /// `--global` was passed.
    pub global: bool,
    /// `--local` was passed.
    pub local: bool,
    /// `--profile <SOURCE>`: a built-in (`starter`, `read-only`, `repo-write`)
    /// or a path to an allowlist JSON file.
    pub profile: Option<String>,
    /// `--harness <NAME>`: which coding harness to wire the hook into. Defaults
    /// to `claude-code` at the CLI layer.
    pub harness: Harness,
    /// `Some(true)` for `--hooks`, `Some(false)` for `--no-hooks`, `None` when
    /// neither was passed.
    pub hooks: Option<bool>,
    /// Force the step-by-step prompts even when stdin is not a terminal.
    pub interactive: bool,
    /// Accept defaults without prompting.
    pub yes: bool,
    /// Overwrite an existing config instead of refusing.
    pub force: bool,
}

/// The settings snippet `install` (and `init --no-hooks`) print for manual
/// wiring: register the hook for the `Bash` matcher, keep `permissions.allow`
/// and `permissions.ask` empty, and add a tiny nuclear-pattern deny.
const SETTINGS_SNIPPET: &str = r#"{
  "hooks": {
    "PreToolUse": [{
      "matcher": "Bash",
      "hooks": [{
        "type": "command",
        "command": "allowlister hook claude-code",
        "timeout": 10
      }]
    }]
  },
  "permissions": {
    "allow": [],
    "ask": [],
    "deny": [
      "Bash(rm -rf /)",
      "Bash(rm -rf ~)",
      "Bash(rm -rf /*)"
    ]
  }
}"#;

/// The hooks snippet `init --harness cursor --no-hooks` prints for manual wiring:
/// register the hook on Cursor's `beforeShellExecution` event. Cursor has no
/// permissions block, so there is nothing to deny here.
const CURSOR_SETTINGS_SNIPPET: &str = r#"{
  "version": 1,
  "hooks": {
    "beforeShellExecution": [
      { "command": "allowlister hook cursor" }
    ]
  }
}"#;

/// The one rule that matters wherever a config is landed: never broaden
/// `permissions.allow`, or the agent skips its prompt and the hook never runs.
const ALLOW_GUIDANCE: &str =
    "IMPORTANT: do NOT add \"Bash\" or \"Bash(*)\" to permissions.allow.\n\
A broad allow makes Claude Code skip its prompt on its own, which\n\
short-circuits the hook's per-fragment allow analysis — the whole\n\
point of allowlister. Let the hook be the source of allow truth.";

/// The resolved decisions an `init` run carries out.
struct Plan {
    /// User-global config (`true`) or project-local (`false`).
    global: bool,
    /// The ruleset source name or path.
    source: String,
    /// Which harness to wire the hook into.
    harness: Harness,
    /// Register the harness hook.
    hooks: bool,
}

/// Run `init`: resolve the plan from flags, prompts, or defaults, write the
/// config, and either register the hook or print the snippet to wire by hand.
pub fn run(args: InitArgs) -> Result<i32> {
    let interactive = args.interactive || (!args.yes && io::stdin().is_terminal());
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let plan = resolve_plan(&args, interactive, &mut input, &mut out)?;
    let env = Env::from_process();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    execute(&plan, args.force, &cwd, &env, &mut out)
}

/// Fill in each decision the user did not pin on the command line: from a prompt
/// when interactive, else from the default.
fn resolve_plan<R: BufRead, W: Write>(
    args: &InitArgs,
    interactive: bool,
    input: &mut R,
    out: &mut W,
) -> Result<Plan> {
    if interactive {
        let _ = writeln!(out, "Let's set allowlister up.\n");
    }
    let global = resolve_scope(args, interactive, input, out)?;
    let source = resolve_source(args, interactive, input, out)?;
    let hooks = resolve_hooks(args, interactive, input, out)?;
    Ok(Plan {
        global,
        source,
        // The harness comes from the flag (or its `claude-code` default), not a
        // prompt — keeping the interactive flow identical for Claude users.
        harness: args.harness,
        hooks,
    })
}

fn resolve_scope<R: BufRead, W: Write>(
    args: &InitArgs,
    interactive: bool,
    input: &mut R,
    out: &mut W,
) -> Result<bool> {
    if args.global {
        return Ok(true);
    }
    if args.local {
        return Ok(false);
    }
    if !interactive {
        return Ok(true);
    }
    let answer = ask(
        input,
        out,
        "Where should the configuration live?\n  \
         1) user-global   — applies to every project   [default]\n  \
         2) project-local — this repository only\n> ",
    )?;
    Ok(!matches!(
        answer.as_str(),
        "2" | "local" | "project" | "project-local"
    ))
}

fn resolve_source<R: BufRead, W: Write>(
    args: &InitArgs,
    interactive: bool,
    input: &mut R,
    out: &mut W,
) -> Result<String> {
    if let Some(profile) = &args.profile {
        return Ok(profile.clone());
    }
    if !interactive {
        return Ok("starter".to_string());
    }
    let answer = ask(
        input,
        out,
        "Which starting ruleset?\n  \
         1) starter    — minimal read-only inspection rules   [default]\n  \
         2) read-only  — curated profile that auto-allows pure reads\n  \
         3) repo-write — read-only plus the writes to manage a repo\n> ",
    )?;
    Ok(match answer.as_str() {
        "2" | "read-only" => "read-only",
        "3" | "repo-write" => "repo-write",
        _ => "starter",
    }
    .to_string())
}

fn resolve_hooks<R: BufRead, W: Write>(
    args: &InitArgs,
    interactive: bool,
    input: &mut R,
    out: &mut W,
) -> Result<bool> {
    if let Some(hooks) = args.hooks {
        return Ok(hooks);
    }
    if !interactive {
        return Ok(true);
    }
    let answer = ask(
        input,
        out,
        &format!(
            "Register the hook in {}'s settings now? [Y/n]\n> ",
            harness_label(args.harness)
        ),
    )?;
    Ok(!matches!(answer.to_ascii_lowercase().as_str(), "n" | "no"))
}

/// Write the prompt, flush it, and read one trimmed line. An empty line (the
/// user hit enter, or stdin reached EOF) means "take the default".
fn ask<R: BufRead, W: Write>(input: &mut R, out: &mut W, prompt: &str) -> Result<String> {
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Carry out the resolved plan: validate the ruleset, write the config, then
/// register the hook (or print the snippet for manual wiring). `cwd`/`env` are
/// injected so the whole flow is testable without touching the real environment.
fn execute<W: Write>(plan: &Plan, force: bool, cwd: &Path, env: &Env, out: &mut W) -> Result<i32> {
    // Vouch for the ruleset before writing anything: an untrusted file's rules
    // must compile, and any source must contain at least one rule.
    let source = profile::resolve_source(&plan.source)?;
    profile::validate(&source)?;
    profile::incoming_rules(&source)?;

    let config_path = config_target(plan.global, cwd, env)?;
    if config_path.exists() && !force {
        return Err(Error::ConfigExists(config_path));
    }
    // Write the source text verbatim so a built-in profile lands byte-for-byte
    // identical to the file the recommended-profile tests pin.
    profile::write_file(&config_path, &source.text)?;
    let _ = writeln!(
        out,
        "Wrote {} config: {}",
        source.label,
        config_path.display()
    );

    if plan.hooks {
        register_hook_for(plan.harness, plan.global, cwd, env, out)?;
    } else {
        let _ = writeln!(out);
        write_hook_setup_for(plan.harness, out)?;
    }
    Ok(0)
}

/// A human label for a harness, used in prompts and messages.
fn harness_label(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "Claude Code",
        Harness::Cursor => "Cursor",
        Harness::Copilot => "Copilot",
    }
}

/// Register the chosen harness's hook and report what changed. Copilot is not yet
/// implemented, so selecting it is a typed error rather than a half-wired setup.
fn register_hook_for<W: Write>(
    harness: Harness,
    global: bool,
    cwd: &Path,
    env: &Env,
    out: &mut W,
) -> Result<()> {
    match harness {
        Harness::ClaudeCode => {
            let path = claude_settings::settings_path(global, cwd, env)?;
            let change = claude_settings::register_hook(&path)?;
            report_claude_hook(out, &change);
            Ok(())
        }
        Harness::Cursor => {
            let path = cursor_settings::settings_path(global, cwd, env)?;
            let change = cursor_settings::register_hook(&path)?;
            report_cursor_hook(out, &change);
            Ok(())
        }
        Harness::Copilot => Err(Error::HarnessUnimplemented("copilot".to_string())),
    }
}

/// Print the manual-wiring snippet for the chosen harness (the `--no-hooks` path).
fn write_hook_setup_for<W: Write>(harness: Harness, out: &mut W) -> Result<()> {
    match harness {
        Harness::ClaudeCode => {
            let _ = write_hook_setup(out);
            Ok(())
        }
        Harness::Cursor => {
            let _ = write_cursor_hook_setup(out);
            Ok(())
        }
        Harness::Copilot => Err(Error::HarnessUnimplemented("copilot".to_string())),
    }
}

/// The config path for the chosen scope.
fn config_target(global: bool, cwd: &Path, env: &Env) -> Result<PathBuf> {
    if global {
        configfs::default_user_config_path(env).ok_or(Error::NoConfigHome)
    } else {
        Ok(configfs::local_config_path(cwd))
    }
}

/// Report what registering the Claude Code hook changed, then the one allow-list
/// warning.
fn report_claude_hook<W: Write>(out: &mut W, change: &claude_settings::SettingsChange) {
    let _ = writeln!(out);
    if change.was_noop() {
        let _ = writeln!(
            out,
            "Hook already registered in {} (nothing to change).",
            change.path.display()
        );
    } else {
        let verb = if change.created { "Created" } else { "Updated" };
        let _ = writeln!(
            out,
            "{verb} {}: registered '{}' as the Bash PreToolUse hook.",
            change.path.display(),
            claude_settings::hook_command()
        );
        if change.denies_added > 0 {
            let _ = writeln!(
                out,
                "  Added {} nuclear-pattern deny rule(s) as a backstop.",
                change.denies_added
            );
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "{ALLOW_GUIDANCE}");
}

/// Report what registering the Cursor hook changed. Cursor's `hooks.json` has no
/// permissions block to broaden, so there is no allow-list warning to print.
fn report_cursor_hook<W: Write>(out: &mut W, change: &cursor_settings::SettingsChange) {
    let _ = writeln!(out);
    if change.was_noop() {
        let _ = writeln!(
            out,
            "Hook already registered in {} (nothing to change).",
            change.path.display()
        );
    } else {
        let verb = if change.created { "Created" } else { "Updated" };
        let _ = writeln!(
            out,
            "{verb} {}: registered '{}' as the beforeShellExecution hook.",
            change.path.display(),
            cursor_settings::hook_command()
        );
    }
}

/// Print the manual-wiring snippet to stdout. Shared with `install`, which lands
/// a fresh config but does not touch harness settings itself.
pub(crate) fn print_hook_setup() {
    let _ = write_hook_setup(&mut io::stdout().lock());
}

/// Write the `~/.claude/settings.json` snippet and the allow-list warning.
fn write_hook_setup<W: Write>(out: &mut W) -> io::Result<()> {
    writeln!(
        out,
        "Add this to ~/.claude/settings.json (merge with any existing keys):"
    )?;
    writeln!(out)?;
    writeln!(out, "{SETTINGS_SNIPPET}")?;
    writeln!(out)?;
    writeln!(out, "{ALLOW_GUIDANCE}")
}

/// Write the `~/.cursor/hooks.json` snippet. Cursor has no allow list to broaden,
/// so there is no allow-list warning to print.
fn write_cursor_hook_setup<W: Write>(out: &mut W) -> io::Result<()> {
    writeln!(
        out,
        "Add this to ~/.cursor/hooks.json (merge with any existing keys):"
    )?;
    writeln!(out)?;
    writeln!(out, "{CURSOR_SETTINGS_SNIPPET}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build args with everything pinned non-interactively; tests override the
    /// fields they exercise.
    fn args() -> InitArgs {
        InitArgs {
            global: false,
            local: true,
            profile: Some("starter".to_string()),
            harness: Harness::ClaudeCode,
            hooks: Some(false),
            interactive: false,
            yes: true,
            force: false,
        }
    }

    #[test]
    fn interactive_defaults_take_global_starter_hooks() {
        // Empty answers (immediate EOF) means "default" for every prompt.
        let mut input = io::Cursor::new(b"\n\n\n");
        let mut out = Vec::new();
        let plan = resolve_plan(
            &InitArgs {
                global: false,
                local: false,
                profile: None,
                harness: Harness::ClaudeCode,
                hooks: None,
                interactive: true,
                yes: false,
                force: false,
            },
            true,
            &mut input,
            &mut out,
        )
        .unwrap();
        assert!(plan.global);
        assert_eq!(plan.source, "starter");
        assert!(plan.hooks);
    }

    #[test]
    fn non_interactive_with_no_flags_uses_defaults() {
        // No prompts and no input read: the defaults decide everything.
        let mut input = io::Cursor::new(b"");
        let mut out = Vec::new();
        let plan = resolve_plan(
            &InitArgs {
                global: false,
                local: false,
                profile: None,
                harness: Harness::ClaudeCode,
                hooks: None,
                interactive: false,
                yes: true,
                force: false,
            },
            false,
            &mut input,
            &mut out,
        )
        .unwrap();
        assert!(plan.global, "default scope is user-global");
        assert_eq!(plan.source, "starter");
        assert!(plan.hooks, "hooks default on");
        assert!(out.is_empty(), "non-interactive prints no prompts");
    }

    #[test]
    fn interactive_choices_are_honored() {
        // project-local, repo-write, no hooks.
        let mut input = io::Cursor::new(b"2\n3\nn\n");
        let mut out = Vec::new();
        let plan = resolve_plan(
            &InitArgs {
                global: false,
                local: false,
                profile: None,
                harness: Harness::ClaudeCode,
                hooks: None,
                interactive: true,
                yes: false,
                force: false,
            },
            true,
            &mut input,
            &mut out,
        )
        .unwrap();
        assert!(!plan.global);
        assert_eq!(plan.source, "repo-write");
        assert!(!plan.hooks);
    }

    #[test]
    fn flags_win_over_prompts() {
        // Even "interactive", pinned flags short-circuit every prompt and no
        // input is consumed.
        let mut input = io::Cursor::new(b"");
        let mut out = Vec::new();
        let plan = resolve_plan(
            &InitArgs {
                global: true,
                local: false,
                profile: Some("read-only".to_string()),
                harness: Harness::ClaudeCode,
                hooks: Some(false),
                interactive: true,
                yes: false,
                force: false,
            },
            true,
            &mut input,
            &mut out,
        )
        .unwrap();
        assert!(plan.global);
        assert_eq!(plan.source, "read-only");
        assert!(!plan.hooks);
    }

    /// An `Env` whose home and XDG both point inside `dir`, so global writes stay
    /// in the sandbox.
    fn sandbox_env(dir: &Path) -> Env {
        Env {
            home: Some(dir.join("home")),
            xdg_config_home: Some(dir.join("xdg")),
        }
    }

    #[test]
    fn execute_writes_local_config_without_hooks() {
        let dir = TempDir::new().unwrap();
        let mut out = Vec::new();
        let plan = Plan {
            global: false,
            source: "starter".to_string(),
            harness: Harness::ClaudeCode,
            hooks: false,
        };
        execute(&plan, false, dir.path(), &sandbox_env(dir.path()), &mut out).unwrap();
        assert!(dir.path().join(".allowlister.json").is_file());
        // No hooks: no settings.json is written, only the manual snippet printed.
        assert!(!dir.path().join(".claude/settings.json").exists());
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Wrote"));
        assert!(text.contains("allowlister hook claude-code"));
    }

    #[test]
    fn re_running_with_hooks_reports_an_already_registered_noop() {
        let dir = TempDir::new().unwrap();
        let env = sandbox_env(dir.path());
        let plan = Plan {
            global: false,
            source: "starter".to_string(),
            harness: Harness::ClaudeCode,
            hooks: true,
        };
        execute(&plan, false, dir.path(), &env, &mut Vec::new()).unwrap();
        // Second run (force past the existing config): the hook is already there,
        // so registration is a no-op the report calls out.
        let mut out = Vec::new();
        execute(&plan, true, dir.path(), &env, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("already registered"),
            "re-running must report the hook no-op: {text}"
        );
    }

    #[test]
    fn execute_local_with_hooks_writes_config_and_settings() {
        let dir = TempDir::new().unwrap();
        let mut out = Vec::new();
        let plan = Plan {
            global: false,
            source: "read-only".to_string(),
            harness: Harness::ClaudeCode,
            hooks: true,
        };
        execute(&plan, false, dir.path(), &sandbox_env(dir.path()), &mut out).unwrap();
        assert!(dir.path().join(".allowlister.json").is_file());
        let settings = dir.path().join(".claude/settings.json");
        assert!(settings.is_file(), "the hook must be registered locally");
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(settings).unwrap()).unwrap();
        assert_eq!(
            doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "allowlister hook claude-code"
        );
    }

    #[test]
    fn execute_global_writes_under_xdg_and_home_claude() {
        let dir = TempDir::new().unwrap();
        let env = sandbox_env(dir.path());
        let plan = Plan {
            global: true,
            source: "starter".to_string(),
            harness: Harness::ClaudeCode,
            hooks: true,
        };
        execute(&plan, false, dir.path(), &env, &mut Vec::new()).unwrap();
        assert!(dir.path().join("xdg/allowlister/config.json").is_file());
        assert!(dir.path().join("home/.claude/settings.json").is_file());
    }

    #[test]
    fn execute_refuses_existing_without_force() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".allowlister.json"), "{}").unwrap();
        let plan = Plan {
            global: false,
            source: "starter".to_string(),
            harness: Harness::ClaudeCode,
            hooks: false,
        };
        let err = execute(
            &plan,
            false,
            dir.path(),
            &sandbox_env(dir.path()),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ConfigExists(_)));
    }

    #[test]
    fn execute_force_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".allowlister.json"), "{}").unwrap();
        let plan = Plan {
            global: false,
            source: "read-only".to_string(),
            harness: Harness::ClaudeCode,
            hooks: false,
        };
        execute(
            &plan,
            true,
            dir.path(),
            &sandbox_env(dir.path()),
            &mut Vec::new(),
        )
        .unwrap();
        let written = fs::read_to_string(dir.path().join(".allowlister.json")).unwrap();
        assert!(written.contains("\"rules\""));
        assert!(written.len() > 2, "the empty config must be replaced");
    }

    #[test]
    fn args_helper_builds_non_interactive_inputs() {
        let a = args();
        assert!(a.yes && a.local && !a.global);
    }

    #[test]
    fn execute_cursor_local_registers_hooks_json() {
        let dir = TempDir::new().unwrap();
        let mut out = Vec::new();
        let plan = Plan {
            global: false,
            source: "read-only".to_string(),
            harness: Harness::Cursor,
            hooks: true,
        };
        execute(&plan, false, dir.path(), &sandbox_env(dir.path()), &mut out).unwrap();
        assert!(dir.path().join(".allowlister.json").is_file());
        // Cursor writes hooks.json, not Claude Code's settings.json.
        assert!(!dir.path().join(".claude/settings.json").exists());
        let hooks = dir.path().join(".cursor/hooks.json");
        assert!(
            hooks.is_file(),
            "the cursor hook must be registered locally"
        );
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(hooks).unwrap()).unwrap();
        assert_eq!(doc["version"], 1);
        assert_eq!(
            doc["hooks"]["beforeShellExecution"][0]["command"],
            "allowlister hook cursor"
        );
    }

    #[test]
    fn execute_cursor_no_hooks_prints_snippet_without_allow_guidance() {
        let dir = TempDir::new().unwrap();
        let mut out = Vec::new();
        let plan = Plan {
            global: false,
            source: "starter".to_string(),
            harness: Harness::Cursor,
            hooks: false,
        };
        execute(&plan, false, dir.path(), &sandbox_env(dir.path()), &mut out).unwrap();
        assert!(!dir.path().join(".cursor/hooks.json").exists());
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("~/.cursor/hooks.json"));
        assert!(text.contains("allowlister hook cursor"));
        // Cursor has no allow list, so the Claude-specific warning must not appear.
        assert!(!text.contains("permissions.allow"));
        assert!(!text.contains("do NOT add"));
    }

    #[test]
    fn execute_cursor_global_writes_under_home_cursor() {
        let dir = TempDir::new().unwrap();
        let env = sandbox_env(dir.path());
        let plan = Plan {
            global: true,
            source: "starter".to_string(),
            harness: Harness::Cursor,
            hooks: true,
        };
        execute(&plan, false, dir.path(), &env, &mut Vec::new()).unwrap();
        // The config still lands under XDG; only the hook wiring is harness-specific.
        assert!(dir.path().join("xdg/allowlister/config.json").is_file());
        assert!(dir.path().join("home/.cursor/hooks.json").is_file());
    }

    #[test]
    fn execute_copilot_harness_is_unimplemented() {
        let dir = TempDir::new().unwrap();
        let plan = Plan {
            global: false,
            source: "starter".to_string(),
            harness: Harness::Copilot,
            hooks: true,
        };
        let err = execute(
            &plan,
            false,
            dir.path(),
            &sandbox_env(dir.path()),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::HarnessUnimplemented(_)));
    }
}

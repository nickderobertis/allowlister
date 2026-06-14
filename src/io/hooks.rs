//! Install allowlister's pre-tool gate hook into a coding harness.
//!
//! allowlister owns the *policy* — which tools to gate (the matcher dialect per
//! harness) and the timeout — and hands it to the shared cross-harness installer
//! ([`oneharness_core`]) as a normalized [`HookSpec`]. oneharness renders that
//! spec into each harness's native shape and writes it where the harness reads
//! it (a shared settings file, a dedicated hooks file, or a plugin), atomically
//! and idempotently. So the eight per-harness install/merge/path implementations
//! live in one audited place instead of being duplicated here; this module is
//! only the mapping from allowlister's [`Harness`] to that spec plus Claude's
//! permissions-deny backstop, which is not part of the hook.

use std::path::{Path, PathBuf};

use oneharness_core::domain::harness::by_id;
use oneharness_core::domain::hooks::HookSpec;
use oneharness_core::io::hooks::{install, GlobalDirs, Scope};
use oneharness_core::io::sync::FileStatus;

use crate::cli::Harness;
use crate::errors::{Error, Result};
use crate::io::claude_settings;
use crate::io::configfs::Env;

/// The plugin/file identity allowlister claims wherever a harness names its hook
/// after the installing tool (the Goose plugin dir, the OpenCode shim, the
/// Copilot per-owner file), so a hook installed by another tool is never
/// clobbered. Ignored by harnesses that merge into a shared config file.
const PLUGIN_NAME: &str = "allowlister";

/// One harness's gate policy: its oneharness registry id, the matcher(s) that
/// select the tools to gate (each in that harness's dialect), and the per-hook
/// `timeout` for the harnesses whose schema carries one. An empty `matchers`
/// means a match-all hook (the harnesses that gate by event, not tool name:
/// Cursor, Copilot, OpenCode); two matchers means two blocks (Claude gates the
/// shell and the non-shell tools separately). `description` brands the plugin
/// metadata for harnesses that carry it (Goose).
struct HookPolicy {
    id: &'static str,
    matchers: &'static [&'static str],
    timeout: Option<u64>,
    description: Option<&'static str>,
}

/// The non-shell tools the tool-rule engine gates for Claude Code, kept as a
/// separate block from `Bash` so the shell path stays byte-identical and Bash is
/// never evaluated twice. MCP tools arrive as `mcp__<server>__<tool>`.
const CLAUDE_TOOL_MATCHER: &str =
    "Read|Edit|Write|Glob|Grep|WebFetch|WebSearch|NotebookEdit|mcp__.*";

/// The gate policy allowlister installs for `harness`. The matchers and timeouts
/// are allowlister's, sourced from each harness's known-good non-interactive
/// invocation; oneharness only renders and writes them.
fn policy_for(harness: Harness) -> HookPolicy {
    match harness {
        // Two matcher blocks: the shell, and the non-shell tools (built-ins +
        // MCP). Claude's schema carries a per-hook timeout.
        Harness::ClaudeCode => HookPolicy {
            id: "claude-code",
            matchers: &["Bash", CLAUDE_TOOL_MATCHER],
            timeout: Some(10),
            description: None,
        },
        // The shell, the `apply_patch` write tool, and MCP tools.
        Harness::Codex => HookPolicy {
            id: "codex",
            matchers: &["^(Bash|apply_patch)$|^mcp__"],
            timeout: None,
            description: None,
        },
        // The shell, the gateable built-ins, and MCP tools. Crush's flat schema
        // carries a timeout; 30s matches Crush's own default, made explicit.
        Harness::Crush => HookPolicy {
            id: "crush",
            matchers: &[
                "^(bash|view|write|edit|multiedit|fetch|web_fetch|web_search|glob|grep)$|^mcp_",
            ],
            timeout: Some(30),
            description: None,
        },
        // The shell, the gateable built-ins, and MCP tools.
        Harness::Qwen => HookPolicy {
            id: "qwen",
            matchers: &[
                "^(run_shell_command|read_file|write_file|edit|glob|grep_search|web_fetch)$|^mcp__",
            ],
            timeout: None,
            description: None,
        },
        // The shell (`shell` or `<ext>__shell`) and every namespaced tool — the
        // developer extension's `__write`/`__edit` and any `<server>__<tool>` MCP
        // call — via `__`. Goose's schema carries a timeout and a plugin
        // description, which allowlister brands as its own.
        Harness::Goose => HookPolicy {
            id: "goose",
            matchers: &["^(shell|read|write|edit|text_editor)$|__"],
            timeout: Some(10),
            description: Some("Gate AI-agent shell commands through allowlister."),
        },
        // Cursor, Copilot, and OpenCode gate by event, not tool name, so they
        // carry no matcher.
        Harness::Cursor => HookPolicy {
            id: "cursor",
            matchers: &[],
            timeout: None,
            description: None,
        },
        Harness::Copilot => HookPolicy {
            id: "copilot",
            matchers: &[],
            timeout: None,
            description: None,
        },
        Harness::OpenCode => HookPolicy {
            id: "opencode",
            matchers: &[],
            timeout: None,
            description: None,
        },
    }
}

/// The oneharness registry id allowlister uses for `harness` — also the suffix
/// of its gate command (`allowlister hook <id>`), so the init summary and the
/// installed hook always name the same command.
pub(crate) fn harness_id(harness: Harness) -> &'static str {
    policy_for(harness).id
}

/// What installing a harness's hook changed, for the init summary. Aggregated
/// across every file the install touched — Goose writes two, Claude writes two
/// matcher blocks into one file — plus Claude's nuclear-deny backstop.
#[derive(Debug)]
pub(crate) struct HookOutcome {
    /// The file (or, for Goose, the plugin directory) to name in the summary.
    pub path: PathBuf,
    /// The install created the target rather than merging into an existing one.
    pub created: bool,
    /// Nuclear-pattern denies added to Claude's `permissions.deny` (0 elsewhere).
    pub denies_added: usize,
    /// Whether anything changed at all. False is a clean re-run (all files
    /// already current and no deny added) the summary reports as a no-op.
    changed: bool,
}

impl HookOutcome {
    /// True when nothing changed: every file was already current and no nuclear
    /// deny needed adding. The summary calls this out as "already registered".
    pub fn was_noop(&self) -> bool {
        !self.changed
    }
}

/// Install the gate hook for `harness` at the chosen scope (`global` ⇒ the
/// harness's user-global location, else the project under `cwd`), returning what
/// changed. The write is non-destructive and idempotent: existing keys are
/// preserved, hook lists union, and a second run changes nothing.
pub(crate) fn install_hook(
    harness: Harness,
    global: bool,
    cwd: &Path,
    env: &Env,
) -> Result<HookOutcome> {
    let policy = policy_for(harness);
    let spec = by_id(policy.id).expect("oneharness registry has every harness allowlister wires");
    let command = format!("{} hook {}", gate_program(), policy.id);

    // `Scope::Global` borrows the resolved dirs; build them once and keep them
    // alive for the duration of every install call below.
    let dirs = GlobalDirs {
        home: env.home.clone(),
        config_home: env.xdg_config_home.clone(),
    };
    let scope = || {
        if global {
            Scope::Global(&dirs)
        } else {
            Scope::Project(cwd)
        }
    };

    // A matcher-less harness installs one match-all hook; otherwise one block per
    // matcher (Claude registers two). The deep-merge unions the blocks into one
    // file, so two install calls produce the same shape as one multi-block write.
    let mut writes = Vec::new();
    if policy.matchers.is_empty() {
        let hook = hook_spec(&command, None, &policy);
        writes
            .extend(install(scope(), spec, &hook, false).map_err(|e| install_error(policy.id, e))?);
    } else {
        for matcher in policy.matchers {
            let hook = hook_spec(&command, Some(matcher), &policy);
            writes.extend(
                install(scope(), spec, &hook, false).map_err(|e| install_error(policy.id, e))?,
            );
        }
    }

    let primary = writes
        .first()
        .map(|w| w.path.clone())
        .expect("an install of a supported harness writes at least one file");
    let created = writes
        .first()
        .map(|w| w.status == FileStatus::Created)
        .unwrap_or(false);
    let mut changed = writes.iter().any(|w| w.status != FileStatus::Unchanged);

    // Claude's nuclear-pattern denies are a permissions backstop, not part of the
    // hook, so they are merged separately into the settings file the install just
    // wrote. Order-independent with the hook merge; both are idempotent.
    let denies_added = if matches!(harness, Harness::ClaudeCode) {
        claude_settings::ensure_nuclear_denies(&primary)?
    } else {
        0
    };
    changed = changed || denies_added > 0;

    // Goose installs a plugin directory of files; name the directory, not the
    // first file inside it, in the summary.
    let path = if matches!(harness, Harness::Goose) {
        primary.parent().map(Path::to_path_buf).unwrap_or(primary)
    } else {
        primary
    };

    Ok(HookOutcome {
        path,
        created,
        denies_added,
        changed,
    })
}

/// The program token for the installed gate command (`<program> hook <id>`). On
/// Unix the bare `allowlister` resolves on PATH the way harnesses spawn the hook.
/// On Windows several harnesses spawn the hook *directly* (Goose's plugin runner,
/// Qwen, …) rather than through a shell, where a bare name with no extension isn't
/// found — so the adapter fails open and a denied command runs. Use the absolute
/// path to this executable, which resolves however the harness spawns it. The init
/// summary still prints the friendly bare form for every harness.
fn gate_program() -> String {
    gate_program_for(std::env::current_exe().ok(), cfg!(windows))
}

/// Pure core of [`gate_program`], split out so both platform branches are
/// testable on any host. Falls back to a bare `allowlister.exe` when the current
/// executable path can't be resolved on Windows.
fn gate_program_for(current_exe: Option<PathBuf>, windows: bool) -> String {
    if windows {
        current_exe
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "allowlister.exe".to_string())
    } else {
        "allowlister".to_string()
    }
}

/// Build the normalized hook for one matcher of a policy.
fn hook_spec(command: &str, matcher: Option<&str>, policy: &HookPolicy) -> HookSpec {
    HookSpec {
        command: command.to_string(),
        matcher: matcher.map(str::to_string),
        timeout: policy.timeout,
        plugin_name: Some(PLUGIN_NAME.to_string()),
        description: policy.description.map(str::to_string),
    }
}

/// Map an installer failure to allowlister's boundary error without leaking the
/// dependency's error type into the public API.
fn install_error(id: &str, source: oneharness_core::errors::OneharnessError) -> Error {
    Error::InvalidConfig {
        origin: format!("{id} hook"),
        message: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn local_env() -> Env {
        Env {
            home: None,
            xdg_config_home: None,
        }
    }

    /// A project install of the Claude hook creates the settings file with both
    /// matcher blocks and reports the three nuclear denies the backstop adds.
    #[test]
    fn claude_project_install_creates_hook_and_denies() {
        let dir = TempDir::new().unwrap();
        let outcome = install_hook(Harness::ClaudeCode, false, dir.path(), &local_env()).unwrap();
        assert!(outcome.created);
        assert!(!outcome.was_noop());
        assert_eq!(outcome.denies_added, 3);

        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&outcome.path).unwrap()).unwrap();
        let pre = doc["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2, "the shell and non-shell matcher blocks");
        assert_eq!(pre[0]["hooks"][0]["timeout"], 10);
        assert_eq!(
            doc["permissions"]["deny"].as_array().unwrap().len(),
            3,
            "the nuclear denies land beside the hook"
        );
    }

    /// Re-installing changes nothing: the hook merge and the deny merge are both
    /// idempotent, so the second run reports a no-op.
    #[test]
    fn reinstall_is_a_noop() {
        let dir = TempDir::new().unwrap();
        install_hook(Harness::ClaudeCode, false, dir.path(), &local_env()).unwrap();
        let again = install_hook(Harness::ClaudeCode, false, dir.path(), &local_env()).unwrap();
        assert!(again.was_noop());
        assert_eq!(again.denies_added, 0);
    }

    /// Goose installs a plugin directory of files; the outcome names the
    /// directory, and a non-Claude harness never adds denies.
    #[test]
    fn goose_project_install_names_the_plugin_dir() {
        let dir = TempDir::new().unwrap();
        let outcome = install_hook(Harness::Goose, false, dir.path(), &local_env()).unwrap();
        assert_eq!(outcome.path, dir.path().join(".agents/plugins/allowlister"));
        assert_eq!(outcome.denies_added, 0);
        assert!(outcome.path.join("plugin.json").is_file());
        assert!(outcome.path.join("hooks/hooks.json").is_file());
    }

    /// A global install anchors under the injected HOME, leaving the project dir
    /// untouched.
    #[test]
    fn global_install_anchors_under_home() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let env = Env {
            home: Some(home.clone()),
            xdg_config_home: None,
        };
        let outcome = install_hook(Harness::Cursor, true, dir.path(), &env).unwrap();
        assert_eq!(outcome.path, home.join(".cursor/hooks.json"));
        assert!(
            !dir.path().join(".cursor").exists(),
            "project dir untouched"
        );
    }

    /// An unparseable target is refused by the installer and surfaced as a typed
    /// boundary error rather than clobbering the file.
    #[test]
    fn malformed_target_is_a_typed_error() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".codex")).unwrap();
        fs::write(dir.path().join(".codex/hooks.json"), "{ not json").unwrap();
        let err = install_hook(Harness::Codex, false, dir.path(), &local_env()).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
    }

    /// A global install with no resolvable base directory is a loud error, never
    /// a write to a guessed path.
    #[test]
    fn global_without_home_is_an_error() {
        let dir = TempDir::new().unwrap();
        let err = install_hook(Harness::ClaudeCode, true, dir.path(), &local_env()).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
    }

    #[test]
    fn harness_id_maps_each_harness() {
        assert_eq!(harness_id(Harness::ClaudeCode), "claude-code");
        assert_eq!(harness_id(Harness::OpenCode), "opencode");
        assert_eq!(harness_id(Harness::Copilot), "copilot");
    }

    /// On Unix the gate command stays the bare name that resolves on PATH.
    #[test]
    fn gate_program_is_bare_name_on_unix() {
        assert_eq!(
            gate_program_for(Some(PathBuf::from("/opt/bin/allowlister")), false),
            "allowlister"
        );
    }

    /// On Windows the gate command is the absolute executable path, so a harness
    /// that spawns the hook directly can find it (a bare name with no extension
    /// would fail open). Falls back to `allowlister.exe` if unresolved.
    #[test]
    fn gate_program_is_absolute_exe_on_windows() {
        let exe = PathBuf::from(r"C:\Tools\allowlister.exe");
        assert_eq!(
            gate_program_for(Some(exe.clone()), true),
            exe.display().to_string()
        );
        assert_eq!(gate_program_for(None, true), "allowlister.exe");
    }
}

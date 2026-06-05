//! Wire allowlister into Goose as an Open-Plugins hook.
//!
//! `init` calls this to register the hook automatically instead of asking the
//! user to paste files. Unlike the other harnesses (one settings file), Goose
//! discovers a *plugin directory*: `<root>/.agents/plugins/allowlister/` holding a
//! `plugin.json` manifest and a `hooks/hooks.json`. This module writes the
//! manifest (once) and merges the `PreToolUse` hook into `hooks.json`. The merge
//! is additive and idempotent: it preserves every other key, never duplicates the
//! hook, and refuses to clobber a hooks file it cannot parse as a JSON object.
//! Unlike the hook read path (which must never crash), this is an explicit setup
//! action, so a malformed target is a hard, typed error rather than a silent skip.
//!
//! `hooks.json` carries Claude Code's nested hook shape: event groups under a
//! top-level `hooks` object, each `PreToolUse` group a tool-name `matcher` plus a
//! list of command hooks. Goose's shell tool is the developer extension's shell,
//! exposed as `shell` (builtin) or `developer__shell` (namespaced), so the matcher
//! catches both. A discovered plugin is active on the next `goose` start with no
//! enable flag or trust step.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::errors::{Error, Result};
use crate::io::configfs::Env;

/// The plugin directory name allowlister claims under `.agents/plugins/`.
const PLUGIN_NAME: &str = "allowlister";

/// The command allowlister registers as the hook. Resolved from `PATH`, matching
/// the snippet the docs tell users to install.
const HOOK_COMMAND: &str = "allowlister hook goose";

/// The lifecycle event allowlister gates on: Goose's only blocking tool event,
/// fired before the tool executes in every approval mode.
const HOOK_EVENT: &str = "PreToolUse";

/// The tool-name matcher (a regex, tested unanchored against the tool name). Goose
/// exposes the developer extension's shell tool as a bare `shell` when it is a
/// builtin (e.g. `--with-builtin developer`) and as `developer__shell` when
/// namespaced, so match both (and any `<ext>__shell`) without catching unrelated
/// tools.
const MATCHER: &str = "(^|__)shell$";

/// Seconds Goose waits for the hook before giving up (and failing open). Goose's
/// own default is 30; a tight value keeps a slow gate from stalling the agent.
const TIMEOUT_SECS: u64 = 10;

/// The Open-Plugins manifest written alongside the hook. Discovery keys off the
/// directory and the presence of `hooks/hooks.json`; the manifest is the
/// convention and what Goose's own example plugins ship.
const MANIFEST: &str = r#"{
  "name": "allowlister",
  "version": "0.1.0",
  "description": "Gate AI-agent shell commands through allowlister."
}"#;

/// The plugin directory Goose discovers: `~/.agents/plugins/allowlister` for the
/// user (global) scope, or `<cwd>/.agents/plugins/allowlister` for a project
/// (local) scope. Always under `HOME` for the global scope (Goose's `.agents`
/// tree is not XDG-based).
pub(crate) fn settings_path(global: bool, cwd: &Path, env: &Env) -> Result<PathBuf> {
    if global {
        env.home
            .as_ref()
            .map(|home| plugin_dir(home))
            .ok_or(Error::NoConfigHome)
    } else {
        Ok(plugin_dir(cwd))
    }
}

/// `<root>/.agents/plugins/allowlister`.
fn plugin_dir(root: &Path) -> PathBuf {
    root.join(".agents").join("plugins").join(PLUGIN_NAME)
}

/// What `register_hook` changed, for the user-facing summary and so tests can
/// assert idempotency (a second run adds nothing).
pub(crate) struct SettingsChange {
    pub path: PathBuf,
    pub created: bool,
    pub hook_added: bool,
    pub manifest_added: bool,
}

impl SettingsChange {
    /// True when the plugin already had the manifest and the hook — a re-run that
    /// touched nothing new.
    pub fn was_noop(&self) -> bool {
        !self.created && !self.hook_added && !self.manifest_added
    }
}

/// Write the manifest (once) and merge the `PreToolUse` hook into the plugin's
/// `hooks/hooks.json` under `plugin_dir`, creating both if absent. Idempotent: an
/// already-registered hook and an existing manifest are left in place.
pub(crate) fn register_hook(plugin_dir: &Path) -> Result<SettingsChange> {
    let manifest_path = plugin_dir.join("plugin.json");
    let hooks_path = plugin_dir.join("hooks").join("hooks.json");

    let manifest_added = ensure_manifest(&manifest_path)?;

    let created = !hooks_path.exists();
    let mut doc = read_settings(&hooks_path)?;
    let label = hooks_path.display().to_string();
    let obj = doc.as_object_mut().ok_or_else(|| Error::InvalidConfig {
        origin: label.clone(),
        message: "expected a JSON object".to_string(),
    })?;
    let hook_added = ensure_hook(obj, &label)?;
    write_text(&hooks_path, &serialize(&doc, &hooks_path)?)?;

    Ok(SettingsChange {
        path: hooks_path,
        created,
        hook_added,
        manifest_added,
    })
}

/// The hook command this binary registers, for messages.
pub(crate) fn hook_command() -> &'static str {
    HOOK_COMMAND
}

/// Write the manifest if it is not already there. Returns whether it was written.
fn ensure_manifest(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    let mut text = MANIFEST.to_string();
    text.push('\n');
    write_text(path, &text)?;
    Ok(true)
}

/// Serialize the merged hooks (pretty, trailing newline).
fn serialize(doc: &Value, path: &Path) -> Result<String> {
    let mut json = serde_json::to_string_pretty(doc).map_err(|err| Error::InvalidConfig {
        origin: path.display().to_string(),
        message: format!("could not serialize hooks: {err}"),
    })?;
    json.push('\n');
    Ok(json)
}

/// Write `text` to `path`, creating the parent directory as needed.
fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| Error::Write {
                path: parent.to_path_buf(),
                source: err,
            })?;
        }
    }
    fs::write(path, text).map_err(|err| Error::Write {
        path: path.to_path_buf(),
        source: err,
    })
}

/// Read the hooks file, or an empty object if it does not exist yet. A malformed
/// existing file is an error: never clobber what we cannot parse.
fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let text = fs::read_to_string(path).map_err(|err| Error::Read {
        path: path.to_path_buf(),
        source: err,
    })?;
    serde_json::from_str(&text).map_err(|err| Error::InvalidConfig {
        origin: path.display().to_string(),
        message: format!("invalid JSON: {err}"),
    })
}

/// Ensure a `PreToolUse` group running our command is present. Returns whether one
/// was added. Idempotency is keyed on our command appearing in any group's `hooks`
/// list, so a user's own group is left untouched and we append ours beside it.
fn ensure_hook(obj: &mut serde_json::Map<String, Value>, label: &str) -> Result<bool> {
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| type_error(label, "hooks", "an object"))?;
    let event = hooks
        .entry(HOOK_EVENT)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| type_error(label, "hooks.PreToolUse", "an array"))?;

    if event.iter().any(group_registers_our_hook) {
        return Ok(false);
    }
    event.push(json!({
        "matcher": MATCHER,
        "hooks": [{ "type": "command", "command": HOOK_COMMAND, "timeout": TIMEOUT_SECS }],
    }));
    Ok(true)
}

/// True if a `PreToolUse` group already runs our command in its `hooks` list.
fn group_registers_our_hook(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| hooks.iter().any(registers_our_command))
}

/// True if a single command-hook entry runs our command.
fn registers_our_command(entry: &Value) -> bool {
    entry.get("command").and_then(Value::as_str) == Some(HOOK_COMMAND)
}

/// A typed error for a hooks key whose JSON type is not what we can merge into —
/// refusing to clobber a hand-edited file with an unexpected shape.
fn type_error(label: &str, key: &str, expected: &str) -> Error {
    Error::InvalidConfig {
        origin: label.to_string(),
        message: format!("'{key}' must be {expected}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn read(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn creates_manifest_and_hook() {
        let dir = TempDir::new().unwrap();
        let plugin = plugin_dir(dir.path());
        let change = register_hook(&plugin).unwrap();
        assert!(change.created);
        assert!(change.hook_added);
        assert!(change.manifest_added);

        // The manifest is written beside the hooks file.
        let manifest = read(&plugin.join("plugin.json"));
        assert_eq!(manifest["name"], "allowlister");

        let doc = read(&plugin.join("hooks/hooks.json"));
        let group = &doc["hooks"]["PreToolUse"][0];
        assert_eq!(group["matcher"], "(^|__)shell$");
        assert_eq!(group["hooks"][0]["timeout"], 10);
        assert!(group_registers_our_hook(group));
    }

    #[test]
    fn re_registering_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let plugin = plugin_dir(dir.path());
        register_hook(&plugin).unwrap();
        let again = register_hook(&plugin).unwrap();
        assert!(again.was_noop(), "second run must change nothing");
        assert_eq!(
            read(&plugin.join("hooks/hooks.json"))["hooks"]["PreToolUse"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "the hook group must not be duplicated"
        );
    }

    #[test]
    fn preserves_existing_keys_and_appends_beside_a_user_group() {
        let dir = TempDir::new().unwrap();
        let plugin = plugin_dir(dir.path());
        let hooks_path = plugin.join("hooks").join("hooks.json");
        write_text(
            &hooks_path,
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "matcher": "developer__shell", "hooks": [{ "type": "command", "command": "echo mine" }] }
                ],
                "PostToolUse": [
                  { "hooks": [{ "type": "command", "command": "echo after" }] }
                ]
              }
            }"#,
        )
        .unwrap();

        let change = register_hook(&plugin).unwrap();
        assert!(!change.created);
        assert!(change.hook_added);
        // The manifest did not exist yet, so registration also wrote it.
        assert!(change.manifest_added);

        let doc = read(&hooks_path);
        // The user's other event is untouched.
        assert_eq!(
            doc["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "echo after"
        );
        let event = doc["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(event.len(), 2, "our group is appended beside the user's");
        assert_eq!(event[0]["hooks"][0]["command"], "echo mine");
        assert!(group_registers_our_hook(&event[1]));
    }

    #[test]
    fn appends_when_an_existing_group_has_no_hook_array() {
        // A bare group without a `hooks` array must not match our command, so
        // registration still appends ours beside it (and never panics).
        let dir = TempDir::new().unwrap();
        let plugin = plugin_dir(dir.path());
        let hooks_path = plugin.join("hooks").join("hooks.json");
        write_text(
            &hooks_path,
            r#"{"hooks":{"PreToolUse":[{"matcher":"developer__shell"}]}}"#,
        )
        .unwrap();
        let change = register_hook(&plugin).unwrap();
        assert!(change.hook_added);
        let groups = read(&hooks_path)["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(groups, 2, "ours is appended beside the bare group");
    }

    #[test]
    fn re_register_leaves_manifest_untouched() {
        let dir = TempDir::new().unwrap();
        let plugin = plugin_dir(dir.path());
        register_hook(&plugin).unwrap();
        let again = register_hook(&plugin).unwrap();
        assert!(
            !again.manifest_added,
            "an existing manifest is not rewritten"
        );
    }

    #[test]
    fn refuses_to_clobber_malformed_hooks() {
        let dir = TempDir::new().unwrap();
        let plugin = plugin_dir(dir.path());
        write_text(&plugin.join("hooks").join("hooks.json"), "{ not json").unwrap();
        assert!(matches!(
            register_hook(&plugin),
            Err(Error::InvalidConfig { .. })
        ));
    }

    /// Every JSON shape we cannot safely merge into is a hard error, never a
    /// clobber. Parametrized over the keys whose type we depend on.
    #[test]
    fn rejects_unmergeable_shapes() {
        let cases = [
            "[]",                            // top-level not an object
            r#"{"hooks":5}"#,                // hooks not an object
            r#"{"hooks":{"PreToolUse":5}}"#, // event not an array
        ];
        for case in cases {
            let dir = TempDir::new().unwrap();
            let plugin = plugin_dir(dir.path());
            write_text(&plugin.join("hooks").join("hooks.json"), case).unwrap();
            assert!(
                matches!(register_hook(&plugin), Err(Error::InvalidConfig { .. })),
                "should reject {case}"
            );
        }
    }

    #[test]
    fn reading_a_directory_hooks_path_errors() {
        let dir = TempDir::new().unwrap();
        let plugin = plugin_dir(dir.path());
        // Stand a directory where hooks.json should be, so reading it back fails.
        fs::create_dir_all(plugin.join("hooks").join("hooks.json")).unwrap();
        assert!(matches!(register_hook(&plugin), Err(Error::Read { .. })));
    }

    #[test]
    fn writing_under_a_file_plugin_dir_errors() {
        let dir = TempDir::new().unwrap();
        // A regular file stands where the plugin directory would need to be, so
        // creating the manifest's parent fails.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        assert!(matches!(register_hook(&blocker), Err(Error::Write { .. })));
    }

    #[test]
    fn hook_command_is_the_registered_command() {
        assert_eq!(hook_command(), "allowlister hook goose");
    }

    #[test]
    fn global_path_uses_home_and_local_uses_cwd() {
        let env = Env {
            home: Some(PathBuf::from("/home/u")),
            xdg_config_home: Some(PathBuf::from("/xdg")),
        };
        assert_eq!(
            settings_path(true, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/home/u/.agents/plugins/allowlister")
        );
        assert_eq!(
            settings_path(false, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/proj/.agents/plugins/allowlister")
        );
    }

    #[test]
    fn global_path_without_home_errors() {
        let env = Env {
            home: None,
            xdg_config_home: None,
        };
        assert!(matches!(
            settings_path(true, Path::new("/proj"), &env),
            Err(Error::NoConfigHome)
        ));
    }
}

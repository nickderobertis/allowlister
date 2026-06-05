//! Wire allowlister into Crush's `crush.json` as the `PreToolUse` hook.
//!
//! `init` calls this to register the hook automatically instead of asking the
//! user to paste a snippet. The merge is additive and idempotent: it preserves
//! every other key, never duplicates the hook, and refuses to clobber a config
//! file it cannot parse as a JSON object. Unlike the hook read path (which must
//! never crash), this is an explicit setup action, so a malformed target is a
//! hard, typed error rather than a silent skip.
//!
//! Crush nests event groups under a top-level `hooks` object. Unlike Claude
//! Code's nested `{matcher, hooks:[{type, command}]}` shape, each Crush entry is
//! flat — `{matcher, command, timeout}` — and `matcher` is a regex tested against
//! the tool name. There is no permissions block, so the hook is the sole gate.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::errors::{Error, Result};
use crate::io::configfs::Env;

/// The command allowlister registers as the hook. Resolved from `PATH`, matching
/// the snippet the docs tell users to install.
const HOOK_COMMAND: &str = "allowlister hook crush";

/// The lifecycle event allowlister gates on: Crush runs this before executing any
/// tool call, ahead of its permission check.
const HOOK_EVENT: &str = "PreToolUse";

/// The tool-name matcher (a regex). It covers the shell (`bash`), the gateable
/// built-in tools, and MCP tools (Crush's single-underscore `mcp_…`); the adapter
/// routes each to the shell or tool engine, so one block fires once per tool.
const MATCHER: &str =
    "^(bash|view|write|edit|multiedit|fetch|web_fetch|web_search|glob|grep)$|^mcp_";

/// Seconds Crush waits for the hook before giving up (and failing open). Matches
/// Crush's own default, made explicit so the registered entry is self-describing.
const TIMEOUT_SECS: u64 = 30;

/// Where Crush reads its config: the project `crush.json` at `<cwd>` for the local
/// scope, or `<config>/crush/crush.json` for the user (global) scope. Crush's
/// global config is XDG-aware (`$XDG_CONFIG_HOME`, else `~/.config`), like
/// allowlister's own — unlike Claude Code/Cursor, which are always under `HOME`.
pub(crate) fn settings_path(global: bool, cwd: &Path, env: &Env) -> Result<PathBuf> {
    if global {
        config_dir(env)
            .map(|dir| dir.join("crush.json"))
            .ok_or(Error::NoConfigHome)
    } else {
        Ok(cwd.join("crush.json"))
    }
}

/// The user-global Crush config directory: `$XDG_CONFIG_HOME/crush`, else
/// `~/.config/crush`. `None` when neither is set.
fn config_dir(env: &Env) -> Option<PathBuf> {
    if let Some(xdg) = &env.xdg_config_home {
        return Some(xdg.join("crush"));
    }
    env.home
        .as_ref()
        .map(|home| home.join(".config").join("crush"))
}

/// What `register_hook` changed, for the user-facing summary and so tests can
/// assert idempotency (a second run adds nothing).
pub(crate) struct SettingsChange {
    pub path: PathBuf,
    pub created: bool,
    pub hook_added: bool,
}

impl SettingsChange {
    /// True when the file already had the hook — a re-run that touched nothing new.
    pub fn was_noop(&self) -> bool {
        !self.created && !self.hook_added
    }
}

/// Merge the `PreToolUse` hook into the config file at `path`, creating it if
/// absent. Idempotent: an already-registered hook is left in place rather than
/// duplicated.
pub(crate) fn register_hook(path: &Path) -> Result<SettingsChange> {
    let created = !path.exists();
    let mut doc = read_settings(path)?;
    let label = path.display().to_string();
    let obj = doc.as_object_mut().ok_or_else(|| Error::InvalidConfig {
        origin: label.clone(),
        message: "expected a JSON object".to_string(),
    })?;

    let hook_added = ensure_hook(obj, &label)?;

    write_settings(path, &doc)?;
    Ok(SettingsChange {
        path: path.to_path_buf(),
        created,
        hook_added,
    })
}

/// The hook command this binary registers, for messages.
pub(crate) fn hook_command() -> &'static str {
    HOOK_COMMAND
}

/// Serialize the merged config (pretty, trailing newline) and write it, creating
/// the parent directory as needed.
fn write_settings(path: &Path, doc: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| Error::Write {
                path: parent.to_path_buf(),
                source: err,
            })?;
        }
    }
    let mut json = serde_json::to_string_pretty(doc).map_err(|err| Error::InvalidConfig {
        origin: path.display().to_string(),
        message: format!("could not serialize config: {err}"),
    })?;
    json.push('\n');
    fs::write(path, json).map_err(|err| Error::Write {
        path: path.to_path_buf(),
        source: err,
    })
}

/// Read the config file, or an empty object if it does not exist yet. A malformed
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

/// Ensure a `PreToolUse` entry running our command is present. Returns whether one
/// was added. Idempotency is keyed on our command appearing as the `command` of
/// any entry, so a user's own entry is left untouched and we append ours beside it.
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

    if event.iter().any(registers_our_command) {
        return Ok(false);
    }
    event.push(json!({
        "matcher": MATCHER,
        "command": HOOK_COMMAND,
        "timeout": TIMEOUT_SECS,
    }));
    Ok(true)
}

/// True if a `PreToolUse` entry already runs our command.
fn registers_our_command(entry: &Value) -> bool {
    entry.get("command").and_then(Value::as_str) == Some(HOOK_COMMAND)
}

/// A typed error for a config key whose JSON type is not what we can merge into —
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
    fn creates_config_with_the_pre_tool_use_hook() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("crush.json");
        let change = register_hook(&path).unwrap();
        assert!(change.created);
        assert!(change.hook_added);

        let doc = read(&path);
        let entry = &doc["hooks"]["PreToolUse"][0];
        assert_eq!(entry["matcher"], MATCHER);
        assert_eq!(entry["timeout"], 30);
        assert!(registers_our_command(entry));
    }

    #[test]
    fn re_registering_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("crush.json");
        register_hook(&path).unwrap();
        let again = register_hook(&path).unwrap();
        assert!(again.was_noop(), "second run must change nothing");
        assert_eq!(
            read(&path)["hooks"]["PreToolUse"].as_array().unwrap().len(),
            1,
            "the hook must not be duplicated"
        );
    }

    #[test]
    fn preserves_existing_keys_and_appends_beside_a_user_hook() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("crush.json");
        fs::write(
            &path,
            r#"{
              "$schema": "https://charm.land/crush.json",
              "hooks": {
                "PreToolUse": [
                  { "matcher": "^bash$", "command": "echo mine" }
                ]
              }
            }"#,
        )
        .unwrap();

        let change = register_hook(&path).unwrap();
        assert!(!change.created);
        assert!(change.hook_added);

        let doc = read(&path);
        // The user's unrelated key is untouched.
        assert_eq!(doc["$schema"], "https://charm.land/crush.json");
        let event = doc["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(event.len(), 2, "our entry is appended beside the user's");
        assert_eq!(event[0]["command"], "echo mine");
        assert!(registers_our_command(&event[1]));
    }

    #[test]
    fn appends_when_an_existing_entry_has_no_command() {
        // A malformed entry without a `command` must not match ours, so
        // registration still appends ours beside it (and never panics).
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("crush.json");
        fs::write(&path, r#"{"hooks":{"PreToolUse":[{"matcher":"^bash$"}]}}"#).unwrap();
        let change = register_hook(&path).unwrap();
        assert!(change.hook_added);
        let entries = read(&path)["hooks"]["PreToolUse"].as_array().unwrap().len();
        assert_eq!(
            entries, 2,
            "ours is appended beside the entry with no command"
        );
    }

    #[test]
    fn refuses_to_clobber_malformed_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("crush.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            register_hook(&path),
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
            let path = dir.path().join("crush.json");
            fs::write(&path, case).unwrap();
            assert!(
                matches!(register_hook(&path), Err(Error::InvalidConfig { .. })),
                "should reject {case}"
            );
        }
    }

    #[test]
    fn reading_a_directory_config_path_errors() {
        let dir = TempDir::new().unwrap();
        // The config path is a directory, so reading it back fails.
        assert!(matches!(register_hook(dir.path()), Err(Error::Read { .. })));
    }

    #[test]
    fn writing_under_a_file_parent_errors() {
        let dir = TempDir::new().unwrap();
        // A regular file stands where the parent directory would need to be.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        let path = blocker.join("crush.json");
        assert!(matches!(register_hook(&path), Err(Error::Write { .. })));
    }

    #[test]
    fn hook_command_is_the_registered_command() {
        assert_eq!(hook_command(), "allowlister hook crush");
    }

    #[test]
    fn global_path_prefers_xdg_local_uses_cwd() {
        let env = Env {
            home: Some(PathBuf::from("/home/u")),
            xdg_config_home: Some(PathBuf::from("/xdg")),
        };
        assert_eq!(
            settings_path(true, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/xdg/crush/crush.json")
        );
        assert_eq!(
            settings_path(false, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/proj/crush.json")
        );
    }

    #[test]
    fn global_path_falls_back_to_home_config() {
        let env = Env {
            home: Some(PathBuf::from("/home/u")),
            xdg_config_home: None,
        };
        assert_eq!(
            settings_path(true, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/home/u/.config/crush/crush.json")
        );
    }

    #[test]
    fn global_path_without_home_or_xdg_errors() {
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

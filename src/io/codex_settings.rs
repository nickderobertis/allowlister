//! Wire allowlister into Codex's `hooks.json` as the `PreToolUse` hook.
//!
//! `init` calls this to register the hook automatically instead of asking the
//! user to paste a snippet. The merge is additive and idempotent: it preserves
//! every other key, never duplicates the hook, and refuses to clobber a hooks
//! file it cannot parse as a JSON object. Unlike the hook read path (which must
//! never crash), this is an explicit setup action, so a malformed target is a
//! hard, typed error rather than a silent skip.
//!
//! Codex's `hooks.json` nests event groups under a top-level `hooks` object; each
//! `PreToolUse` group carries a tool-name `matcher` and a list of command hooks.
//! There is no permissions block, so the hook is the sole gate — nothing else to
//! deny here.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::errors::{Error, Result};
use crate::io::configfs::Env;

/// The command allowlister registers as the hook. Resolved from `PATH`, matching
/// the snippet the docs tell users to install.
const HOOK_COMMAND: &str = "allowlister hook codex";

/// The lifecycle event allowlister gates on: Codex runs this before executing any
/// tool call, in every approval mode.
const HOOK_EVENT: &str = "PreToolUse";

/// The tool-name matcher (a regex). It covers the shell (`Bash`), the gateable
/// non-shell write tool (`apply_patch`), and MCP tools (`mcp__…`); the adapter
/// routes each to the shell or tool engine, so one block fires once per tool.
const MATCHER: &str = "^(Bash|apply_patch)$|^mcp__";

/// Where Codex reads hooks: `~/.codex/hooks.json` for the user (global) scope, or
/// `<cwd>/.codex/hooks.json` for a project (local) scope.
pub(crate) fn settings_path(global: bool, cwd: &Path, env: &Env) -> Result<PathBuf> {
    if global {
        env.home
            .as_ref()
            .map(|home| home.join(".codex").join("hooks.json"))
            .ok_or(Error::NoConfigHome)
    } else {
        Ok(cwd.join(".codex").join("hooks.json"))
    }
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

/// Merge the `PreToolUse` hook into the hooks file at `path`, creating it if
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

/// Serialize the merged hooks (pretty, trailing newline) and write them, creating
/// `.codex/` as needed.
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
        message: format!("could not serialize hooks: {err}"),
    })?;
    json.push('\n');
    fs::write(path, json).map_err(|err| Error::Write {
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
        "hooks": [{ "type": "command", "command": HOOK_COMMAND }],
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
    fn creates_hooks_with_the_pre_tool_use_group() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".codex/hooks.json");
        let change = register_hook(&path).unwrap();
        assert!(change.created);
        assert!(change.hook_added);

        let doc = read(&path);
        let group = &doc["hooks"]["PreToolUse"][0];
        assert_eq!(group["matcher"], MATCHER);
        assert!(group_registers_our_hook(group));
    }

    #[test]
    fn re_registering_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hooks.json");
        register_hook(&path).unwrap();
        let again = register_hook(&path).unwrap();
        assert!(again.was_noop(), "second run must change nothing");
        assert_eq!(
            read(&path)["hooks"]["PreToolUse"].as_array().unwrap().len(),
            1,
            "the hook group must not be duplicated"
        );
    }

    #[test]
    fn preserves_existing_keys_and_appends_beside_a_user_group() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hooks.json");
        fs::write(
            &path,
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "matcher": "^Bash$", "hooks": [{ "type": "command", "command": "echo mine" }] }
                ],
                "PostToolUse": [
                  { "hooks": [{ "type": "command", "command": "echo after" }] }
                ]
              }
            }"#,
        )
        .unwrap();

        let change = register_hook(&path).unwrap();
        assert!(!change.created);
        assert!(change.hook_added);

        let doc = read(&path);
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
        let path = dir.path().join("hooks.json");
        fs::write(&path, r#"{"hooks":{"PreToolUse":[{"matcher":"^Bash$"}]}}"#).unwrap();
        let change = register_hook(&path).unwrap();
        assert!(change.hook_added);
        let groups = read(&path)["hooks"]["PreToolUse"].as_array().unwrap().len();
        assert_eq!(groups, 2, "ours is appended beside the bare group");
    }

    #[test]
    fn refuses_to_clobber_malformed_hooks() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hooks.json");
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
            let path = dir.path().join("hooks.json");
            fs::write(&path, case).unwrap();
            assert!(
                matches!(register_hook(&path), Err(Error::InvalidConfig { .. })),
                "should reject {case}"
            );
        }
    }

    #[test]
    fn reading_a_directory_hooks_path_errors() {
        let dir = TempDir::new().unwrap();
        // The hooks path is a directory, so reading it back fails.
        assert!(matches!(register_hook(dir.path()), Err(Error::Read { .. })));
    }

    #[test]
    fn writing_under_a_file_parent_errors() {
        let dir = TempDir::new().unwrap();
        // A regular file stands where `.codex/` would need to be.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        let path = blocker.join("hooks.json");
        assert!(matches!(register_hook(&path), Err(Error::Write { .. })));
    }

    #[test]
    fn hook_command_is_the_registered_command() {
        assert_eq!(hook_command(), "allowlister hook codex");
    }

    #[test]
    fn global_path_uses_home_and_local_uses_cwd() {
        let env = Env {
            home: Some(PathBuf::from("/home/u")),
            xdg_config_home: Some(PathBuf::from("/xdg")),
        };
        assert_eq!(
            settings_path(true, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/home/u/.codex/hooks.json")
        );
        assert_eq!(
            settings_path(false, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/proj/.codex/hooks.json")
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

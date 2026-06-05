//! Wire allowlister into GitHub Copilot CLI's hooks as the `preToolUse` hook.
//!
//! `init` calls this to register the hook automatically instead of asking the
//! user to paste a snippet. The merge is additive and idempotent: it preserves
//! every other key, never duplicates the hook, and refuses to clobber a hooks
//! file it cannot parse as a JSON object. Unlike the hook read path (which must
//! never crash), this is an explicit setup action, so a malformed target is a
//! hard, typed error rather than a silent skip.
//!
//! Copilot loads hook configs from a *directory* of independent JSON files —
//! `<repo>/.github/hooks/*.json` (project) or `~/.copilot/hooks/*.json` (user) —
//! so allowlister owns a dedicated `allowlister.json` rather than merging into a
//! shared settings file the way the Claude Code and Cursor adapters do. There is
//! no permissions block to deny into; the hook is the sole gate.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::errors::{Error, Result};
use crate::io::configfs::Env;

/// The command allowlister registers as the hook. Resolved from `PATH`, matching
/// the snippet the docs tell users to install. Copilot wants a per-OS command, so
/// it is written under both `bash` and `powershell`; invoking the binary is
/// identical on every platform.
const HOOK_COMMAND: &str = "allowlister hook copilot";

/// The hook-config schema version allowlister writes when the file has none. A
/// user-pinned version is preserved rather than overwritten.
const HOOKS_VERSION: u64 = 1;

/// The lifecycle event allowlister gates on: Copilot runs this before executing a
/// tool call, and (unlike `postToolUse`) a `deny` here blocks it.
const HOOK_EVENT: &str = "preToolUse";

/// The file allowlister owns within Copilot's hooks directory.
const HOOK_FILE: &str = "allowlister.json";

/// Where Copilot reads hooks: `~/.copilot/hooks/allowlister.json` for the user
/// (global) scope, or `<cwd>/.github/hooks/allowlister.json` for a project
/// (local) scope. Like the other adapters this targets the conventional home
/// (`~/.copilot`); Copilot also honors `$COPILOT_HOME`, but allowlister writes to
/// the default location, consistent with how it targets `~/.claude` and
/// `~/.cursor`.
pub(crate) fn settings_path(global: bool, cwd: &Path, env: &Env) -> Result<PathBuf> {
    if global {
        env.home
            .as_ref()
            .map(|home| home.join(".copilot").join("hooks").join(HOOK_FILE))
            .ok_or(Error::NoConfigHome)
    } else {
        Ok(cwd.join(".github").join("hooks").join(HOOK_FILE))
    }
}

/// What `register_hook` changed, for the user-facing summary and so tests can
/// assert idempotency (a second run adds nothing).
pub(crate) struct SettingsChange {
    pub path: PathBuf,
    pub created: bool,
    pub hook_added: bool,
    pub version_set: bool,
}

impl SettingsChange {
    /// True when the file already had the hook and a version — a re-run that
    /// touched nothing new.
    pub fn was_noop(&self) -> bool {
        !self.created && !self.hook_added && !self.version_set
    }
}

/// Merge the `preToolUse` hook into the hooks file at `path`, creating it if
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

    let version_set = ensure_version(obj);
    let hook_added = ensure_hook(obj, &label)?;

    write_settings(path, &doc)?;
    Ok(SettingsChange {
        path: path.to_path_buf(),
        created,
        hook_added,
        version_set,
    })
}

/// The hook command this binary registers, for messages.
pub(crate) fn hook_command() -> &'static str {
    HOOK_COMMAND
}

/// Serialize the merged hooks (pretty, trailing newline) and write them, creating
/// `.github/hooks/` (or `~/.copilot/hooks/`) as needed.
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

/// Ensure a top-level `version` is present, adding the default only when absent.
/// A version the user already pinned is left untouched. Returns whether one was
/// written.
fn ensure_version(obj: &mut serde_json::Map<String, Value>) -> bool {
    if obj.contains_key("version") {
        return false;
    }
    obj.insert("version".to_string(), json!(HOOKS_VERSION));
    true
}

/// Ensure a `preToolUse` hook running our command is present. Returns whether it
/// was added. Idempotency is keyed on our command appearing as the `bash`,
/// `powershell`, or `command` of any element, so a user's own hook is left
/// untouched and we append our own element beside it.
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
        .ok_or_else(|| type_error(label, "hooks.preToolUse", "an array"))?;

    if event.iter().any(registers_our_hook) {
        return Ok(false);
    }
    event.push(json!({
        "type": "command",
        "bash": HOOK_COMMAND,
        "powershell": HOOK_COMMAND,
    }));
    Ok(true)
}

/// True if a hook entry already runs our command under any of the command keys
/// Copilot accepts.
fn registers_our_hook(entry: &Value) -> bool {
    ["bash", "powershell", "command"]
        .iter()
        .any(|key| entry.get(*key).and_then(Value::as_str) == Some(HOOK_COMMAND))
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
    fn creates_hooks_with_hook_and_version() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".github/hooks/allowlister.json");
        let change = register_hook(&path).unwrap();
        assert!(change.created);
        assert!(change.hook_added);
        assert!(change.version_set);

        let doc = read(&path);
        assert_eq!(doc["version"], HOOKS_VERSION);
        let hook = &doc["hooks"]["preToolUse"][0];
        assert!(registers_our_hook(hook));
        assert_eq!(hook["type"], "command");
        assert_eq!(hook["bash"], HOOK_COMMAND);
        assert_eq!(hook["powershell"], HOOK_COMMAND);
    }

    #[test]
    fn re_registering_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlister.json");
        register_hook(&path).unwrap();
        let again = register_hook(&path).unwrap();
        assert!(again.was_noop(), "second run must change nothing");
        assert_eq!(
            read(&path)["hooks"]["preToolUse"].as_array().unwrap().len(),
            1,
            "the hook must not be duplicated"
        );
    }

    #[test]
    fn preserves_existing_keys_and_appends_beside_a_user_hook() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlister.json");
        fs::write(
            &path,
            r#"{
              "version": 1,
              "hooks": {
                "preToolUse": [
                  { "type": "command", "bash": "echo mine" }
                ],
                "postToolUse": [
                  { "type": "command", "bash": "echo done" }
                ]
              }
            }"#,
        )
        .unwrap();

        let change = register_hook(&path).unwrap();
        assert!(!change.created);
        assert!(change.hook_added);
        assert!(!change.version_set, "an existing version is left untouched");

        let doc = read(&path);
        // The user's other event is untouched.
        assert_eq!(doc["hooks"]["postToolUse"][0]["bash"], "echo done");
        let event = doc["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(event.len(), 2, "our hook is appended beside the user's");
        assert_eq!(event[0]["bash"], "echo mine");
        assert!(registers_our_hook(&event[1]));
    }

    #[test]
    fn re_registers_when_only_a_user_hook_exists() {
        // A file the user owns with a different preToolUse hook gains ours beside
        // it and is not treated as already-registered.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlister.json");
        fs::write(
            &path,
            r#"{"version":1,"hooks":{"preToolUse":[{"type":"command","command":"echo other"}]}}"#,
        )
        .unwrap();
        let change = register_hook(&path).unwrap();
        assert!(change.hook_added);
        assert_eq!(
            read(&path)["hooks"]["preToolUse"].as_array().unwrap().len(),
            2
        );
    }

    #[test]
    fn preserves_a_user_pinned_version() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlister.json");
        fs::write(&path, r#"{"version": 2}"#).unwrap();
        let change = register_hook(&path).unwrap();
        assert!(!change.version_set);
        assert_eq!(read(&path)["version"], 2, "a pinned version is preserved");
    }

    #[test]
    fn refuses_to_clobber_malformed_hooks() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allowlister.json");
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
            r#"{"hooks":{"preToolUse":5}}"#, // event not an array
        ];
        for case in cases {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("allowlister.json");
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
        // A regular file stands where the hooks directory would need to be.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        let path = blocker.join("allowlister.json");
        assert!(matches!(register_hook(&path), Err(Error::Write { .. })));
    }

    #[test]
    fn hook_command_is_the_registered_command() {
        assert_eq!(hook_command(), "allowlister hook copilot");
    }

    #[test]
    fn global_path_uses_home_copilot_and_local_uses_github_hooks() {
        let env = Env {
            home: Some(PathBuf::from("/home/u")),
            xdg_config_home: Some(PathBuf::from("/xdg")),
        };
        assert_eq!(
            settings_path(true, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/home/u/.copilot/hooks/allowlister.json")
        );
        assert_eq!(
            settings_path(false, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/proj/.github/hooks/allowlister.json")
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

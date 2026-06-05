//! Wire allowlister into Claude Code's `settings.json` as the `Bash` PreToolUse
//! hook.
//!
//! `init` calls this to register the hook automatically instead of asking the
//! user to paste a snippet. The merge is additive and idempotent: it preserves
//! every other key, never duplicates the hook, and refuses to clobber a settings
//! file it cannot parse as a JSON object. Unlike the hook read path (which must
//! never crash), this is an explicit setup action, so a malformed target is a
//! hard, typed error rather than a silent skip.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::errors::{Error, Result};
use crate::io::configfs::Env;

/// The command allowlister registers as the hook. Resolved from `PATH`, matching
/// the snippet the docs tell users to install.
const HOOK_COMMAND: &str = "allowlister hook claude-code";

/// Hook timeout in seconds. Generous for a one-shot parse-and-match; the adapter
/// returns in well under a second.
const HOOK_TIMEOUT: u64 = 10;

/// Matcher for the non-shell tools the tool-rule engine gates, kept as a separate
/// `PreToolUse` block from `Bash` so the shell path stays byte-identical and Bash
/// is never evaluated twice. MCP tools arrive as `mcp__<server>__<tool>`.
const TOOL_MATCHER: &str = "Read|Edit|Write|Glob|Grep|WebFetch|WebSearch|NotebookEdit|mcp__.*";

/// The `PreToolUse` matchers allowlister registers, each running the same hook
/// command: the shell tool and the non-shell tools.
const MATCHERS: [&str; 2] = ["Bash", TOOL_MATCHER];

/// Nuclear-pattern denies added to `permissions.deny` as defense in depth. The
/// hook is the source of allow truth; these are a backstop the harness enforces
/// even before the hook runs. `permissions.allow`/`ask` are never touched — a
/// broad allow there would let the agent skip its own prompt and short-circuit
/// the hook entirely.
const NUCLEAR_DENIES: [&str; 3] = ["Bash(rm -rf /)", "Bash(rm -rf ~)", "Bash(rm -rf /*)"];

/// Where Claude Code reads settings: `~/.claude/settings.json` for the user
/// (global) scope, or `<cwd>/.claude/settings.json` for a project (local) scope.
/// Unlike the allowlister config, this is always under `HOME` (Claude Code does
/// not consult `XDG_CONFIG_HOME`).
pub(crate) fn settings_path(global: bool, cwd: &Path, env: &Env) -> Result<PathBuf> {
    if global {
        env.home
            .as_ref()
            .map(|home| home.join(".claude").join("settings.json"))
            .ok_or(Error::NoConfigHome)
    } else {
        Ok(cwd.join(".claude").join("settings.json"))
    }
}

/// What `register_hook` changed, for the user-facing summary and so tests can
/// assert idempotency (a second run adds nothing).
pub(crate) struct SettingsChange {
    pub path: PathBuf,
    pub created: bool,
    pub hook_added: bool,
    pub denies_added: usize,
}

impl SettingsChange {
    /// True when the file already had the hook and every nuclear deny — a
    /// re-run that touched nothing new.
    pub fn was_noop(&self) -> bool {
        !self.created && !self.hook_added && self.denies_added == 0
    }
}

/// Merge the `Bash` PreToolUse hook and the nuclear denies into the settings
/// file at `path`, creating it if absent. Idempotent: an already-registered hook
/// is left in place rather than duplicated.
pub(crate) fn register_hook(path: &Path) -> Result<SettingsChange> {
    let created = !path.exists();
    let mut doc = read_settings(path)?;
    let label = path.display().to_string();
    let obj = doc.as_object_mut().ok_or_else(|| Error::InvalidConfig {
        origin: label.clone(),
        message: "expected a JSON object".to_string(),
    })?;

    let hook_added = ensure_hook(obj, &label)?;
    let denies_added = ensure_denies(obj, &label)?;

    write_settings(path, &doc)?;
    Ok(SettingsChange {
        path: path.to_path_buf(),
        created,
        hook_added,
        denies_added,
    })
}

/// The hook command this binary registers, for messages.
pub(crate) fn hook_command() -> &'static str {
    HOOK_COMMAND
}

/// Serialize the merged settings (pretty, trailing newline) and write them,
/// creating `.claude/` as needed.
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
        message: format!("could not serialize settings: {err}"),
    })?;
    json.push('\n');
    fs::write(path, json).map_err(|err| Error::Write {
        path: path.to_path_buf(),
        source: err,
    })
}

/// Read the settings file, or an empty object if it does not exist yet. A
/// malformed existing file is an error: never clobber what we cannot parse.
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

/// Ensure a `PreToolUse` block running our command is present for every matcher
/// in [`MATCHERS`] (the shell tool and the non-shell tools). Returns whether any
/// was added. Idempotency is keyed per matcher: a block the user owns, or one we
/// already wrote, is left untouched and only missing matchers are appended.
fn ensure_hook(obj: &mut serde_json::Map<String, Value>, label: &str) -> Result<bool> {
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| type_error(label, "hooks", "an object"))?;
    let pre = hooks
        .entry("PreToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| type_error(label, "hooks.PreToolUse", "an array"))?;

    let mut added = false;
    for matcher in MATCHERS {
        if pre
            .iter()
            .any(|entry| registers_our_matcher(entry, matcher))
        {
            continue;
        }
        pre.push(json!({
            "matcher": matcher,
            "hooks": [{
                "type": "command",
                "command": HOOK_COMMAND,
                "timeout": HOOK_TIMEOUT,
            }],
        }));
        added = true;
    }
    Ok(added)
}

/// True if a PreToolUse entry runs our hook command under a specific matcher.
fn registers_our_matcher(entry: &Value, matcher: &str) -> bool {
    entry.get("matcher").and_then(Value::as_str) == Some(matcher) && registers_our_hook(entry)
}

/// True if a PreToolUse entry already runs our hook command (under any matcher).
fn registers_our_hook(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks
                .iter()
                .any(|hook| hook.get("command").and_then(Value::as_str) == Some(HOOK_COMMAND))
        })
}

/// Ensure every nuclear-pattern deny is present in `permissions.deny`, adding
/// only the missing ones. Returns how many were added. Leaves `allow`/`ask`
/// alone.
fn ensure_denies(obj: &mut serde_json::Map<String, Value>, label: &str) -> Result<usize> {
    let permissions = obj
        .entry("permissions")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| type_error(label, "permissions", "an object"))?;
    let deny = permissions
        .entry("deny")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| type_error(label, "permissions.deny", "an array"))?;

    let mut added = 0;
    for pattern in NUCLEAR_DENIES {
        let present = deny.iter().any(|v| v.as_str() == Some(pattern));
        if !present {
            deny.push(Value::String(pattern.to_string()));
            added += 1;
        }
    }
    Ok(added)
}

/// A typed error for a settings key whose JSON type is not what we can merge
/// into — refusing to clobber a hand-edited file with an unexpected shape.
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
    fn creates_settings_with_hook_and_denies() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".claude/settings.json");
        let change = register_hook(&path).unwrap();
        assert!(change.created);
        assert!(change.hook_added);
        assert_eq!(change.denies_added, NUCLEAR_DENIES.len());

        let doc = read(&path);
        let pre = doc["hooks"]["PreToolUse"].as_array().unwrap();
        // One block per matcher: the shell tool and the non-shell tools.
        assert_eq!(pre.len(), MATCHERS.len());
        assert!(registers_our_matcher(&pre[0], "Bash"));
        assert!(registers_our_matcher(&pre[1], TOOL_MATCHER));
        let deny = doc["permissions"]["deny"].as_array().unwrap();
        assert_eq!(deny.len(), NUCLEAR_DENIES.len());
    }

    #[test]
    fn re_registering_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        register_hook(&path).unwrap();
        let again = register_hook(&path).unwrap();
        assert!(again.was_noop(), "second run must change nothing");
        assert_eq!(
            read(&path)["hooks"]["PreToolUse"].as_array().unwrap().len(),
            MATCHERS.len(),
            "the hooks must not be duplicated"
        );
    }

    #[test]
    fn preserves_existing_keys_and_appends_beside_a_user_hook() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{
              "$schema": "https://example/schema.json",
              "model": "opus",
              "hooks": {
                "PreToolUse": [
                  { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo mine" }] }
                ]
              },
              "permissions": { "allow": ["Bash(ls *)"], "deny": ["Bash(rm -rf /)"] }
            }"#,
        )
        .unwrap();

        let change = register_hook(&path).unwrap();
        assert!(!change.created);
        assert!(change.hook_added);
        // Only the two missing nuclear denies are added; the present one is kept.
        assert_eq!(change.denies_added, NUCLEAR_DENIES.len() - 1);

        let doc = read(&path);
        assert_eq!(doc["$schema"], "https://example/schema.json");
        assert_eq!(doc["model"], "opus");
        // The user's allow list is untouched.
        assert_eq!(doc["permissions"]["allow"][0], "Bash(ls *)");
        let pre = doc["hooks"]["PreToolUse"].as_array().unwrap();
        // The user's own Bash block (running "echo mine") is preserved, and both
        // of our matcher blocks are appended beside it.
        assert_eq!(
            pre.len(),
            1 + MATCHERS.len(),
            "our hooks append beside the user's"
        );
        assert_eq!(pre[0]["hooks"][0]["command"], "echo mine");
        assert!(registers_our_matcher(&pre[1], "Bash"));
        assert!(registers_our_matcher(&pre[2], TOOL_MATCHER));
    }

    #[test]
    fn refuses_to_clobber_malformed_settings() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            register_hook(&path),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn rejects_a_non_object_hooks_key() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"hooks": 5}"#).unwrap();
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
            r#"{"hooks":{"PreToolUse":5}}"#, // PreToolUse not an array
            r#"{"permissions":5}"#,          // permissions not an object
            r#"{"permissions":{"deny":5}}"#, // deny not an array
        ];
        for case in cases {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("settings.json");
            fs::write(&path, case).unwrap();
            assert!(
                matches!(register_hook(&path), Err(Error::InvalidConfig { .. })),
                "should reject {case}"
            );
        }
    }

    #[test]
    fn reading_a_directory_settings_path_errors() {
        let dir = TempDir::new().unwrap();
        // The settings path is a directory, so reading it back fails.
        assert!(matches!(register_hook(dir.path()), Err(Error::Read { .. })));
    }

    #[test]
    fn writing_under_a_file_parent_errors() {
        let dir = TempDir::new().unwrap();
        // A regular file stands where `.claude/` would need to be.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        let path = blocker.join("settings.json");
        assert!(matches!(register_hook(&path), Err(Error::Write { .. })));
    }

    #[test]
    fn hook_command_is_the_registered_command() {
        assert_eq!(hook_command(), "allowlister hook claude-code");
    }

    #[test]
    fn global_path_uses_home_and_local_uses_cwd() {
        let env = Env {
            home: Some(PathBuf::from("/home/u")),
            xdg_config_home: Some(PathBuf::from("/xdg")),
        };
        assert_eq!(
            settings_path(true, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/home/u/.claude/settings.json")
        );
        assert_eq!(
            settings_path(false, Path::new("/proj"), &env).unwrap(),
            PathBuf::from("/proj/.claude/settings.json")
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

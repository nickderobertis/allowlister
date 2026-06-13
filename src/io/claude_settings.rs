//! Claude Code's nuclear-pattern deny backstop.
//!
//! The hook itself is installed by the shared cross-harness installer (see
//! [`crate::io::hooks`]); this adds one Claude-specific extra the installer does
//! not: a few `permissions.deny` entries the harness enforces even before the
//! hook runs. It is a permissions merge, not part of the hook, and applies only
//! to Claude Code, so it lives on its own here.
//!
//! Like the hook install, the merge is additive and idempotent: it preserves
//! every other key, adds only the missing denies, never touches
//! `permissions.allow`/`ask`, and refuses to clobber a file it cannot parse as a
//! JSON object.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use crate::errors::{Error, Result};

/// Nuclear-pattern denies added to `permissions.deny` as defense in depth. The
/// hook is the source of allow truth; these are a backstop the harness enforces
/// even before the hook runs. `permissions.allow`/`ask` are never touched — a
/// broad allow there would let the agent skip its own prompt and short-circuit
/// the hook entirely.
const NUCLEAR_DENIES: [&str; 3] = ["Bash(rm -rf /)", "Bash(rm -rf ~)", "Bash(rm -rf /*)"];

/// Merge the nuclear-pattern denies into `permissions.deny` in the settings file
/// at `path` (the file the hook install just created or updated), adding only the
/// missing ones. Returns how many were added — zero on a clean re-run. Leaves
/// `allow`/`ask` alone and refuses to clobber an unparseable file.
pub(crate) fn ensure_nuclear_denies(path: &Path) -> Result<usize> {
    let mut doc = read_settings(path)?;
    let label = path.display().to_string();
    let obj = doc.as_object_mut().ok_or_else(|| Error::InvalidConfig {
        origin: label.clone(),
        message: "expected a JSON object".to_string(),
    })?;

    let added = ensure_denies(obj, &label)?;
    if added > 0 {
        write_settings(path, &doc)?;
    }
    Ok(added)
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

/// Serialize the merged settings (pretty, trailing newline) and write them,
/// creating the parent directory as needed.
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
    fn adds_all_denies_to_a_fresh_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        let added = ensure_nuclear_denies(&path).unwrap();
        assert_eq!(added, NUCLEAR_DENIES.len());
        let deny = read(&path)["permissions"]["deny"].as_array().unwrap().len();
        assert_eq!(deny, NUCLEAR_DENIES.len());
    }

    #[test]
    fn re_running_adds_nothing_and_leaves_the_file_untouched() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        ensure_nuclear_denies(&path).unwrap();
        let before = fs::read_to_string(&path).unwrap();
        let added = ensure_nuclear_denies(&path).unwrap();
        assert_eq!(added, 0, "second run adds nothing");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            before,
            "an all-present file is not rewritten"
        );
    }

    #[test]
    fn adds_only_the_missing_denies_and_keeps_allow_and_other_keys() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{
              "model": "opus",
              "permissions": { "allow": ["Bash(ls *)"], "deny": ["Bash(rm -rf /)"] }
            }"#,
        )
        .unwrap();

        let added = ensure_nuclear_denies(&path).unwrap();
        // Only the two absent nuclear denies are added; the present one is kept.
        assert_eq!(added, NUCLEAR_DENIES.len() - 1);

        let doc = read(&path);
        assert_eq!(doc["model"], "opus");
        // The user's allow list is untouched.
        assert_eq!(doc["permissions"]["allow"][0], "Bash(ls *)");
        let deny = doc["permissions"]["deny"].as_array().unwrap();
        assert_eq!(deny.len(), NUCLEAR_DENIES.len());
    }

    #[test]
    fn refuses_to_clobber_malformed_settings() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            ensure_nuclear_denies(&path),
            Err(Error::InvalidConfig { .. })
        ));
    }

    /// Every JSON shape we cannot safely merge into is a hard error, never a
    /// clobber.
    #[test]
    fn rejects_unmergeable_shapes() {
        let cases = [
            "[]",                            // top-level not an object
            r#"{"permissions":5}"#,          // permissions not an object
            r#"{"permissions":{"deny":5}}"#, // deny not an array
        ];
        for case in cases {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("settings.json");
            fs::write(&path, case).unwrap();
            assert!(
                matches!(
                    ensure_nuclear_denies(&path),
                    Err(Error::InvalidConfig { .. })
                ),
                "should reject {case}"
            );
        }
    }

    #[test]
    fn reading_a_directory_path_errors() {
        let dir = TempDir::new().unwrap();
        // The path is a directory, so reading it back fails.
        assert!(matches!(
            ensure_nuclear_denies(dir.path()),
            Err(Error::Read { .. })
        ));
    }

    #[test]
    fn writing_under_a_file_parent_errors() {
        let dir = TempDir::new().unwrap();
        // A regular file stands where the settings parent dir would need to be.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        let path = blocker.join("settings.json");
        assert!(matches!(
            ensure_nuclear_denies(&path),
            Err(Error::Write { .. })
        ));
    }
}

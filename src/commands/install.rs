//! `allowlister install` — merge an allowlist file (or a built-in profile) into
//! a target config, creating it if absent. The merge is by rule name, so
//! re-running is idempotent: a rule already present is left in place rather than
//! duplicated. This is the "get started from a known-good ruleset" path; `init`
//! writes a minimal starter, `install` layers a curated profile onto whatever
//! you already have.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::config;
use crate::errors::{Error, Result};
use crate::io::configfs::{self, Env};

/// The built-in profiles, embedded from `examples/recommended/` so they ship
/// inside the binary and stay byte-for-byte in sync with the files the
/// recommended-profile tests pin.
const READ_ONLY: &str = include_str!("../../examples/recommended/read-only.json");
const REPO_WRITE: &str = include_str!("../../examples/recommended/repo-write.json");

/// Merge `source` into the chosen target config. `--global` (the default)
/// targets the user config, `--local` a project `.allowlister.json`, and
/// `output` an explicit path. `source` is a built-in profile name or a file
/// path.
pub fn run(source: &str, _global: bool, local: bool, output: Option<&Path>) -> Result<i32> {
    let source = resolve_source(source)?;

    // Vouch for what we are about to install: every rule must compile, so a
    // broken profile fails loudly here instead of landing half-applied. The
    // built-in profiles are already gated by `tests/recommended.rs`, so
    // re-compiling their 54+ rules on every install is pure overhead — only
    // untrusted file sources need the check.
    if !source.trusted {
        let validated = config::compile_str(&source.text, &source.label);
        if !validated.warnings.is_empty() {
            return Err(Error::InvalidConfig {
                origin: source.label,
                message: format!("rules do not compile:\n{}", validated.warnings.join("\n")),
            });
        }
    }

    let incoming = rules_of(parse_config(&source.text, &source.label)?, &source.label)?;
    if incoming.is_empty() {
        return Err(Error::InvalidConfig {
            origin: source.label,
            message: "contains no rules to install".to_string(),
        });
    }

    let target = target_path(local, output)?;
    let created = !target.exists();
    let mut target_doc = read_target(&target)?;
    let merge = merge_rules(&mut target_doc, &target, incoming)?;

    write_config(&target, &target_doc)?;

    let verb = if created { "Created" } else { "Updated" };
    println!("{verb} {} from {}.", target.display(), source.label);
    println!(
        "  {} rule(s) added, {} already present ({} total).",
        merge.added, merge.skipped, merge.total
    );
    // A brand-new config still needs the hook wired up to do anything; hand the
    // user the same snippet `init` would, since `init` refuses once a config
    // exists.
    if created {
        println!();
        super::init::print_hook_setup();
    }

    Ok(0)
}

/// A resolved source: its config text, a label for messages, and whether it is
/// trusted (a built-in profile validated at build time) or an untrusted file
/// that must be compile-checked before install.
struct Source {
    label: String,
    text: String,
    trusted: bool,
}

/// Resolve `source` to its config text. An existing file always wins, so a
/// built-in name only applies when no such file exists.
fn resolve_source(source: &str) -> Result<Source> {
    let path = Path::new(source);
    if path.is_file() {
        let text = fs::read_to_string(path).map_err(|err| Error::Read {
            path: path.to_path_buf(),
            source: err,
        })?;
        return Ok(Source {
            label: source.to_string(),
            text,
            trusted: false,
        });
    }
    let (label, text) = match source {
        "read-only" => ("built-in profile 'read-only'", READ_ONLY),
        "repo-write" => ("built-in profile 'repo-write'", REPO_WRITE),
        _ => return Err(Error::UnknownSource(source.to_string())),
    };
    Ok(Source {
        label: label.to_string(),
        text: text.to_string(),
        trusted: true,
    })
}

/// Where the merge result is written.
fn target_path(local: bool, output: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = output {
        return Ok(path.to_path_buf());
    }
    if local {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        return Ok(configfs::local_config_path(&cwd));
    }
    configfs::default_user_config_path(&Env::from_process()).ok_or(Error::NoConfigHome)
}

/// Parse config JSON, mapping a syntax error to a typed boundary error.
fn parse_config(text: &str, label: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|err| Error::InvalidConfig {
        origin: label.to_string(),
        message: format!("invalid JSON: {err}"),
    })
}

/// The `rules` array of a config document, taken by value so the array moves out
/// rather than being cloned. A document without a `rules` key has no rules.
fn rules_of(doc: Value, label: &str) -> Result<Vec<Value>> {
    let mut obj = match doc {
        Value::Object(obj) => obj,
        _ => {
            return Err(Error::InvalidConfig {
                origin: label.to_string(),
                message: "expected a JSON object".to_string(),
            })
        }
    };
    match obj.remove("rules") {
        None => Ok(Vec::new()),
        Some(Value::Array(rules)) => Ok(rules),
        Some(_) => Err(Error::InvalidConfig {
            origin: label.to_string(),
            message: "'rules' must be an array".to_string(),
        }),
    }
}

/// Read the target config, or an empty object if it does not exist yet. A
/// malformed existing target is an error: never clobber a file we cannot parse.
fn read_target(target: &Path) -> Result<Value> {
    if !target.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = fs::read_to_string(target).map_err(|err| Error::Read {
        path: target.to_path_buf(),
        source: err,
    })?;
    parse_config(&text, &target.display().to_string())
}

/// Counts from a merge, for the user-facing summary.
struct Merge {
    added: usize,
    skipped: usize,
    total: usize,
}

/// Append every incoming rule whose `name` is not already present in `target`.
/// Rules with no `name` cannot be deduplicated, so they are always appended.
fn merge_rules(target: &mut Value, target_path: &Path, incoming: Vec<Value>) -> Result<Merge> {
    let label = target_path.display().to_string();
    let obj = target.as_object_mut().ok_or_else(|| Error::InvalidConfig {
        origin: label.clone(),
        message: "expected a JSON object".to_string(),
    })?;
    let rules = obj
        .entry("rules")
        .or_insert_with(|| Value::Array(Vec::new()));
    let rules = rules.as_array_mut().ok_or_else(|| Error::InvalidConfig {
        origin: label,
        message: "'rules' must be an array".to_string(),
    })?;

    let mut seen: HashSet<String> = rules
        .iter()
        .filter_map(|rule| rule.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();

    let mut added = 0;
    let mut skipped = 0;
    for rule in incoming {
        // Compute the name as an owned value first so the rule is free to move.
        let name = rule.get("name").and_then(Value::as_str).map(str::to_string);
        match name {
            Some(name) if seen.contains(&name) => skipped += 1,
            Some(name) => {
                seen.insert(name);
                rules.push(rule);
                added += 1;
            }
            None => {
                rules.push(rule);
                added += 1;
            }
        }
    }

    Ok(Merge {
        added,
        skipped,
        total: rules.len(),
    })
}

/// Serialize the merged document (pretty, trailing newline) and write it,
/// creating parent directories as needed.
fn write_config(target: &Path, doc: &Value) -> Result<()> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| Error::Write {
                path: parent.to_path_buf(),
                source: err,
            })?;
        }
    }
    let mut json = serde_json::to_string_pretty(doc).map_err(|err| Error::InvalidConfig {
        origin: target.display().to_string(),
        message: format!("could not serialize merged config: {err}"),
    })?;
    json.push('\n');
    fs::write(target, json).map_err(|err| Error::Write {
        path: target.to_path_buf(),
        source: err,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn read(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn rule_names(doc: &Value) -> Vec<String> {
        doc["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|rule| rule.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn installs_builtin_into_a_new_file() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        assert_eq!(
            run("read-only", true, false, Some(target.as_path())).unwrap(),
            0
        );

        let doc = read(&target);
        assert!(doc["rules"].as_array().unwrap().len() > 30);
        // What we wrote is a loadable, warning-free config.
        let loaded = config::load_from_paths(&[target]);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    }

    #[test]
    fn re_installing_does_not_duplicate_rules() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        run("read-only", true, false, Some(target.as_path())).unwrap();
        let first = rule_names(&read(&target));
        run("read-only", true, false, Some(target.as_path())).unwrap();
        let second = rule_names(&read(&target));
        assert_eq!(first, second, "a second install must be a no-op");
    }

    #[test]
    fn repo_write_layers_onto_read_only_without_duplicates() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        run("read-only", true, false, Some(target.as_path())).unwrap();
        run("repo-write", true, false, Some(target.as_path())).unwrap();

        let names = rule_names(&read(&target));
        let unique: HashSet<&String> = names.iter().collect();
        assert_eq!(names.len(), unique.len(), "rule names must stay unique");
    }

    #[test]
    fn merge_preserves_existing_rules_and_top_level_keys() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        fs::write(
            &target,
            r#"{"name":"mine","rules":[{"name":"keep","match":"ls*","action":"allow"}]}"#,
        )
        .unwrap();

        run("read-only", true, false, Some(target.as_path())).unwrap();
        let doc = read(&target);
        assert_eq!(doc["name"], "mine");
        let names = rule_names(&doc);
        assert!(names.contains(&"keep".to_string()));
        assert!(names.len() > 1);
    }

    #[test]
    fn installs_from_a_file_path() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.json");
        fs::write(
            &src,
            r#"{"rules":[{"name":"x","match":"x*","action":"allow"}]}"#,
        )
        .unwrap();
        let target = dir.path().join("config.json");

        run(src.to_str().unwrap(), true, false, Some(target.as_path())).unwrap();
        assert_eq!(rule_names(&read(&target)), vec!["x".to_string()]);
    }

    #[test]
    fn unknown_source_errors_and_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        let err = run("no-such-profile", true, false, Some(target.as_path())).unwrap_err();
        assert!(matches!(err, Error::UnknownSource(_)));
        assert!(!target.exists());
    }

    #[test]
    fn empty_source_errors() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("empty.json");
        fs::write(&src, r#"{"rules":[]}"#).unwrap();
        let target = dir.path().join("config.json");
        let err = run(src.to_str().unwrap(), true, false, Some(target.as_path())).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
        assert!(!target.exists());
    }

    #[test]
    fn source_with_a_bad_rule_errors_before_writing() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("bad.json");
        // A rule that sets neither `match` nor `argv` does not compile.
        fs::write(&src, r#"{"rules":[{"name":"bad","action":"allow"}]}"#).unwrap();
        let target = dir.path().join("config.json");
        let err = run(src.to_str().unwrap(), true, false, Some(target.as_path())).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
        assert!(!target.exists());
    }

    #[test]
    fn refuses_to_merge_into_a_malformed_target() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        fs::write(&target, "{ not json").unwrap();
        let err = run("read-only", true, false, Some(target.as_path())).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
    }

    #[test]
    fn source_without_a_rules_key_is_empty() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("nokey.json");
        fs::write(&src, "{}").unwrap();
        let target = dir.path().join("config.json");
        let err = run(src.to_str().unwrap(), true, false, Some(target.as_path())).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
    }

    #[test]
    fn rules_without_a_name_are_always_appended() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("nameless.json");
        fs::write(&src, r#"{"rules":[{"match":"ls*","action":"allow"}]}"#).unwrap();
        let target = dir.path().join("config.json");
        run(src.to_str().unwrap(), true, false, Some(target.as_path())).unwrap();
        assert_eq!(read(&target)["rules"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn write_into_a_path_under_a_file_errors() {
        let dir = TempDir::new().unwrap();
        // A regular file stands where a directory would need to be, so creating
        // the target's parent fails — exercising the write error path.
        let blocker = dir.path().join("not-a-dir");
        fs::write(&blocker, "x").unwrap();
        let target = blocker.join("config.json");
        let err = run("read-only", true, false, Some(target.as_path())).unwrap_err();
        assert!(matches!(err, Error::Write { .. }));
    }
}

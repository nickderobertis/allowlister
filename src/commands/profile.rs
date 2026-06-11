//! Shared allowlist-source plumbing for `init` and `install`.
//!
//! A *source* is either a built-in profile name (`starter`, `read-only`,
//! `repo-write`) or a path to an allowlist JSON file. `init` writes a source as
//! a fresh config; `install` layers a source onto an existing one. Both verbs
//! resolve and validate sources the same way here, so they agree on what a
//! profile is — the only difference is create-fresh versus merge-in.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::config;
use crate::errors::{Error, Result};

/// The built-in profiles, embedded from `examples/recommended/` so they ship
/// inside the binary and stay byte-for-byte in sync with the files the
/// recommended-profile tests pin.
const READ_ONLY: &str = include_str!("../../examples/recommended/read-only.jsonc");
const REPO_WRITE: &str = include_str!("../../examples/recommended/repo-write.jsonc");

/// The minimal starter ruleset `init` writes by default: read-only inspection
/// commands, common pipe filters scoped to their role, and a couple of nuclear
/// denies. Embedded as a built-in so `init` and `install` share one source of
/// truth for what "starter" means.
pub(crate) const STARTER: &str = r#"{
  "rules": [
    { "name": "ls",           "match": "ls*",                                    "action": "allow" },
    { "name": "pwd",          "match": "pwd",                                    "action": "allow" },
    { "name": "echo",         "match": "echo *",                                 "action": "allow" },
    { "name": "cat",          "match": "cat *",                                  "action": "allow" },

    { "name": "git read-only",
      "match": "git @(status|diff|log|show|branch|remote|rev-parse|describe)*",
      "action": "allow" },

    { "name": "pipe filters",
      "match": "@(head|tail|wc|grep|awk|sort|uniq|cut|sed|tr|jq|less|more) *",
      "action": "allow",
      "roles": ["pipe_filter", "substitution"] },

    { "name": "rm -rf — never",   "match": "rm -rf*",  "action": "deny" },

    { "name": "shell as pipe target — never",
      "argv": ["@(sh|bash|zsh|fish|dash|ksh)", "**"],
      "action": "deny",
      "roles": ["pipe_filter"] }
  ]
}
"#;

/// A resolved source: its config text, a label for messages, and whether it is
/// trusted (a built-in validated at build time) or an untrusted file that must
/// be compile-checked before it is written anywhere.
pub(crate) struct Source {
    pub label: String,
    pub text: String,
    pub trusted: bool,
}

/// Resolve `source` to its config text. An existing file always wins, so a
/// built-in name only applies when no such file exists.
pub(crate) fn resolve_source(source: &str) -> Result<Source> {
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
        "starter" => ("built-in profile 'starter'", STARTER),
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

/// Vouch for a source before anything is written: every rule must compile, so a
/// broken profile fails loudly here instead of landing half-applied. Built-in
/// profiles are already gated by `tests/recommended.rs`, so re-compiling their
/// rules on every run is pure overhead — only untrusted file sources are checked.
pub(crate) fn validate(source: &Source) -> Result<()> {
    if source.trusted {
        return Ok(());
    }
    let validated = config::compile_str(&source.text, &source.label);
    if !validated.warnings.is_empty() {
        return Err(Error::InvalidConfig {
            origin: source.label.clone(),
            message: format!("rules do not compile:\n{}", validated.warnings.join("\n")),
        });
    }
    Ok(())
}

/// The `rules` array of a source, by value, with a guarantee it is non-empty —
/// a source with nothing to install is a user error, not a silent no-op.
pub(crate) fn incoming_rules(source: &Source) -> Result<Vec<Value>> {
    let rules = rules_of(parse_config(&source.text, &source.label)?, &source.label)?;
    if rules.is_empty() {
        return Err(Error::InvalidConfig {
            origin: source.label.clone(),
            message: "contains no rules to install".to_string(),
        });
    }
    Ok(rules)
}

/// Parse config JSON, mapping a syntax error to a typed boundary error. Comments
/// are stripped first so a profile or target file may carry explanatory notes,
/// matching what the rule loader accepts.
fn parse_config(text: &str, label: &str) -> Result<Value> {
    serde_json::from_str(&config::strip_jsonc_comments(text)).map_err(|err| Error::InvalidConfig {
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

/// Counts from a merge, for the user-facing summary.
pub(crate) struct Merge {
    pub added: usize,
    pub skipped: usize,
    pub total: usize,
}

/// Merge `incoming` into the target config *text*, returning the updated text
/// and the counts. Rules whose `name` is already present are skipped; the rest
/// are spliced into the original text, so the target's comments and formatting
/// survive byte-for-byte. Rules with no `name` cannot be deduplicated, so they
/// are always appended. A malformed target is an error: never clobber a file we
/// cannot parse.
pub(crate) fn merge_rules_text(
    target_text: &str,
    label: &str,
    incoming: Vec<Value>,
) -> Result<(String, Merge)> {
    let doc = parse_config(target_text, label)?;
    let obj = doc.as_object().ok_or_else(|| Error::InvalidConfig {
        origin: label.to_string(),
        message: "expected a JSON object".to_string(),
    })?;
    let existing: &[Value] = match obj.get("rules") {
        None => &[],
        Some(Value::Array(rules)) => rules,
        Some(_) => {
            return Err(Error::InvalidConfig {
                origin: label.to_string(),
                message: "'rules' must be an array".to_string(),
            })
        }
    };

    let mut seen: HashSet<String> = existing
        .iter()
        .filter_map(|rule| rule.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();

    let mut to_add = Vec::new();
    let mut skipped = 0;
    for rule in incoming {
        // Compute the name as an owned value first so the rule is free to move.
        let name = rule.get("name").and_then(Value::as_str).map(str::to_string);
        match name {
            Some(name) if seen.contains(&name) => skipped += 1,
            Some(name) => {
                seen.insert(name);
                to_add.push(rule);
            }
            None => to_add.push(rule),
        }
    }

    let merge = Merge {
        added: to_add.len(),
        skipped,
        total: existing.len() + to_add.len(),
    };
    if to_add.is_empty() {
        return Ok((target_text.to_string(), merge));
    }
    let text = crate::jsonc::append_rules(target_text, &to_add).map_err(|message| {
        Error::InvalidConfig {
            origin: label.to_string(),
            message,
        }
    })?;
    Ok((text, merge))
}

/// Write `contents` to `target`, creating parent directories as needed. Shared
/// so writing source text verbatim (`init`) and writing a merged document
/// (`install`) report the same typed write errors.
pub(crate) fn write_file(target: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| Error::Write {
                path: parent.to_path_buf(),
                source: err,
            })?;
        }
    }
    fs::write(target, contents).map_err(|err| Error::Write {
        path: target.to_path_buf(),
        source: err,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_is_a_valid_warning_free_config() {
        let source = resolve_source("starter").unwrap();
        assert!(source.trusted);
        validate(&source).unwrap();
        assert!(!incoming_rules(&source).unwrap().is_empty());
    }

    #[test]
    fn builtin_profiles_resolve_and_validate() {
        for name in ["read-only", "repo-write"] {
            let source = resolve_source(name).unwrap();
            assert!(source.trusted, "{name} must be trusted");
            validate(&source).unwrap();
            assert!(incoming_rules(&source).unwrap().len() > 1);
        }
    }

    #[test]
    fn unknown_builtin_name_errors() {
        assert!(matches!(
            resolve_source("no-such-profile"),
            Err(Error::UnknownSource(_))
        ));
    }

    #[test]
    fn untrusted_file_with_a_bad_rule_fails_validation() {
        let source = Source {
            label: "bad".to_string(),
            // A rule that sets neither `match` nor `argv` does not compile.
            text: r#"{"rules":[{"name":"bad","action":"allow"}]}"#.to_string(),
            trusted: false,
        };
        assert!(matches!(
            validate(&source),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn a_source_without_rules_is_rejected() {
        let source = Source {
            label: "empty".to_string(),
            text: r#"{"rules":[]}"#.to_string(),
            trusted: true,
        };
        assert!(matches!(
            incoming_rules(&source),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn a_non_object_source_is_rejected() {
        let source = Source {
            label: "arr".to_string(),
            text: "[]".to_string(),
            trusted: true,
        };
        assert!(matches!(
            incoming_rules(&source),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn a_source_with_non_array_rules_is_rejected() {
        let source = Source {
            label: "weird".to_string(),
            text: r#"{"rules": 5}"#.to_string(),
            trusted: true,
        };
        assert!(matches!(
            incoming_rules(&source),
            Err(Error::InvalidConfig { .. })
        ));
    }

    #[test]
    fn writing_to_a_directory_path_is_a_write_error() {
        let dir = tempfile::TempDir::new().unwrap();
        // The destination is an existing directory, so the write itself fails
        // even though its parent already exists.
        assert!(matches!(
            write_file(dir.path(), "x"),
            Err(Error::Write { .. })
        ));
    }

    #[test]
    fn resolve_source_surfaces_a_read_failure() {
        // A path that is a file but cannot be read maps to a typed Read error.
        // Skipped when running as root (which bypasses the permission bits).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("locked.json");
            fs::write(&path, "{}").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            // If we can still read it (e.g. CI running as root), skip the assert.
            if fs::read_to_string(&path).is_err() {
                assert!(matches!(
                    resolve_source(path.to_str().unwrap()),
                    Err(Error::Read { .. })
                ));
            }
        }
    }
}

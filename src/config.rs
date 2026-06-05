//! Configuration loading: the JSON rule schema, strict validation, and the
//! user + project merge.
//!
//! The merged rule list is a concatenation (user rules first, then project
//! rules outermost-first). Because the verdict is set-theoretic — any deny
//! denies, any allow allows, otherwise defer — merge order only affects which
//! rule's name is cited in a reason, never the verdict itself.
//!
//! A malformed config file (or a single malformed rule) is skipped with a
//! recorded warning; loading never fails the caller. The hook must never crash.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::domain::{Action, Grant, MatchKind, RedirPolicy, Role, Rule};
use crate::io::configfs::{self, Env};

/// The compiled, merged configuration.
#[derive(Debug, Default)]
pub struct LoadedConfig {
    pub rules: Vec<Rule>,
    /// Config files that were loaded (or skipped, annotated with the reason).
    pub sources: Vec<String>,
    /// Non-fatal problems encountered while loading.
    pub warnings: Vec<String>,
}

/// Load and merge user + project config relative to `cwd`.
pub fn load(cwd: &Path) -> LoadedConfig {
    load_from_paths(&configfs::discover(cwd, &Env::from_process()))
}

/// Load and merge an explicit, ordered list of config files. Used by tests and
/// by `load` after discovery.
pub fn load_from_paths(paths: &[PathBuf]) -> LoadedConfig {
    let mut config = LoadedConfig::default();
    for path in paths {
        let display = path.display().to_string();
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(err) => {
                config.sources.push(format!("{display} (skipped: {err})"));
                continue;
            }
        };
        append_config(&mut config, &contents, &display);
    }
    config
}

/// Compile a config straight from its JSON text, with no filesystem read.
/// `source_label` names the origin in any messages. Like [`load_from_paths`], a
/// parse error or a malformed rule becomes a warning rather than a hard
/// failure; a caller that needs strictness inspects `warnings` itself.
pub(crate) fn compile_str(contents: &str, source_label: &str) -> LoadedConfig {
    let mut config = LoadedConfig::default();
    append_config(&mut config, contents, source_label);
    config
}

/// Parse `contents` and append its compiled rules — and any problems — to
/// `config`, recording `display` as a source.
fn append_config(config: &mut LoadedConfig, contents: &str, display: &str) {
    let raw: RawConfig = match serde_json::from_str(contents) {
        Ok(raw) => raw,
        Err(err) => {
            config
                .sources
                .push(format!("{display} (skipped: invalid JSON: {err})"));
            config
                .warnings
                .push(format!("invalid JSON in {display}: {err}"));
            return;
        }
    };
    for (index, raw_rule) in raw.rules.into_iter().enumerate() {
        match raw_rule.compile(display) {
            Ok(rule) => config.rules.push(rule),
            Err(err) => config.warnings.push(format!(
                "{display}: skipping rule #{index} ('{name}'): {err}",
                name = raw_rule.display_name()
            )),
        }
    }
    config.sources.push(display.to_string());
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
struct RawRule {
    #[serde(default)]
    name: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default, rename = "match")]
    match_pattern: Option<String>,
    #[serde(default)]
    argv: Option<Vec<String>>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    roles: Option<Vec<String>>,
    #[serde(default)]
    redirections: Option<RawRedir>,
    #[serde(default)]
    grants: Option<String>,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct RawRedir {
    #[serde(default)]
    deny: bool,
    #[serde(default)]
    write_glob: Option<Vec<String>>,
    #[serde(default)]
    read_glob: Option<Vec<String>>,
}

impl RawRule {
    fn compile(&self, source: &str) -> Result<Rule, String> {
        let action = match self.action.as_deref() {
            None | Some("allow") => Action::Allow,
            Some("deny") => Action::Deny,
            Some(other) => return Err(format!("unknown action '{other}'")),
        };
        let kind = MatchKind::parse(self.kind.as_deref())?;
        let roles = self.compile_roles()?;
        let redirections = self.compile_redirections()?;
        let grant = self.compile_grant()?;
        let name = self.display_name();

        // A redirection-only rule only ever widens redirection targets for an
        // already-authorized command, so a deny grant is meaningless and an
        // absent redirection block makes it a silent no-op; reject both.
        if grant == Grant::Redirections {
            if action != Action::Allow {
                return Err(
                    "a redirection-only rule (grants 'redirections') must use action 'allow'"
                        .to_string(),
                );
            }
            if self.redirections.is_none() {
                return Err(
                    "a redirection-only rule (grants 'redirections') must define a 'redirections' block"
                        .to_string(),
                );
            }
        }

        match (&self.match_pattern, &self.argv) {
            (Some(_), Some(_)) => {
                Err("rule must set exactly one of 'match' or 'argv', not both".to_string())
            }
            (None, None) => Err("rule must set one of 'match' or 'argv'".to_string()),
            (Some(pattern), None) => Rule::from_match(
                name,
                action,
                pattern,
                kind,
                roles,
                redirections,
                self.description.clone(),
                source.to_string(),
            )
            .map(|rule| rule.with_grant(grant)),
            (None, Some(argv)) => {
                if argv.is_empty() {
                    return Err("'argv' must not be empty".to_string());
                }
                Rule::from_argv(
                    name,
                    action,
                    argv,
                    kind,
                    roles,
                    redirections,
                    self.description.clone(),
                    source.to_string(),
                )
                .map(|rule| rule.with_grant(grant))
            }
        }
    }

    fn compile_grant(&self) -> Result<Grant, String> {
        match self.grants.as_deref() {
            None | Some("command") => Ok(Grant::Command),
            Some("redirections") => Ok(Grant::Redirections),
            Some(other) => Err(format!("unknown grant '{other}'")),
        }
    }

    fn compile_roles(&self) -> Result<Option<Vec<Role>>, String> {
        match &self.roles {
            None => Ok(None),
            Some(names) => {
                let mut roles = Vec::with_capacity(names.len());
                for name in names {
                    let role = Role::parse(name).ok_or_else(|| format!("unknown role '{name}'"))?;
                    roles.push(role);
                }
                Ok(Some(roles))
            }
        }
    }

    fn compile_redirections(&self) -> Result<RedirPolicy, String> {
        match &self.redirections {
            None => Ok(RedirPolicy::default()),
            Some(raw) => RedirPolicy::from_globs(
                raw.deny,
                raw.write_glob.as_deref(),
                raw.read_glob.as_deref(),
            ),
        }
    }

    fn display_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        if let Some(pattern) = &self.match_pattern {
            return pattern.clone();
        }
        if let Some(argv) = &self.argv {
            return argv.join(" ");
        }
        "(unnamed)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_config(dir: &TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn loads_and_compiles_rules() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"ls","match":"ls*","action":"allow"}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert_eq!(config.rules.len(), 1);
        assert!(config.warnings.is_empty());
    }

    #[test]
    fn invalid_json_is_skipped_with_warning() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "bad.json", "{not json");
        let config = load_from_paths(&[path]);
        assert!(config.rules.is_empty());
        assert_eq!(config.warnings.len(), 1);
    }

    #[test]
    fn malformed_rule_is_skipped_not_fatal() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[
                {"name":"ok","match":"ls*","action":"allow"},
                {"name":"both","match":"x","argv":["x"],"action":"allow"}
            ]}"#,
        );
        let config = load_from_paths(&[path]);
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.warnings.len(), 1);
    }

    #[test]
    fn unknown_role_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"r","match":"x","action":"allow","roles":["nope"]}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert!(config.rules.is_empty());
        assert_eq!(config.warnings.len(), 1);
    }

    #[test]
    fn missing_file_is_recorded_not_fatal() {
        let config = load_from_paths(&[PathBuf::from("/nonexistent/allowlister.json")]);
        assert!(config.rules.is_empty());
        assert_eq!(config.sources.len(), 1);
    }

    #[test]
    fn redirection_only_grant_compiles() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"scratch","argv":["**"],"action":"allow","grants":"redirections","redirections":{"write_glob":["/tmp/**"]}}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert_eq!(config.rules.len(), 1);
        assert!(config.warnings.is_empty());
        assert!(config.rules[0].is_redirection_only());
    }

    #[test]
    fn unknown_grant_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"r","match":"x","action":"allow","grants":"bogus"}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert!(config.rules.is_empty());
        assert_eq!(config.warnings.len(), 1);
    }

    #[test]
    fn redirection_only_grant_requires_allow_action() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"r","match":"x","action":"deny","grants":"redirections","redirections":{"write_glob":["/tmp/**"]}}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert!(config.rules.is_empty());
        assert_eq!(config.warnings.len(), 1);
    }

    #[test]
    fn redirection_only_grant_requires_redirections_block() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"r","match":"x","action":"allow","grants":"redirections"}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert!(config.rules.is_empty());
        assert_eq!(config.warnings.len(), 1);
    }
}

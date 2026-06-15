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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::domain::{Action, Grant, MatchKind, ParamKey, RedirPolicy, Role, Rule, ToolRule};
use crate::io::configfs::{self, Env};

/// Canonical, publicly hosted location of the config JSON Schema. `init` and
/// `install` write it as a `"$schema"` key so editors validate and autocomplete
/// the file; the loader itself accepts and ignores the key.
pub const SCHEMA_URL: &str = "https://nickderobertis.github.io/allowlister/allowlister.schema.json";

/// The compiled, merged configuration.
#[derive(Debug, Default)]
pub struct LoadedConfig {
    /// Shell-command rules, evaluated by the structural bash engine.
    pub rules: Vec<Rule>,
    /// Non-shell tool-call rules, evaluated by the tool engine.
    pub tool_rules: Vec<ToolRule>,
    /// Usage-history recording settings (opt-in; see [`HistorySettings`]).
    pub history: HistorySettings,
    /// Config files that were loaded (or skipped, annotated with the reason).
    pub sources: Vec<String>,
    /// Non-fatal problems encountered while loading.
    pub warnings: Vec<String>,
}

/// Whether allowlister records a local history of evaluated commands. Off by
/// default; `init` offers to turn it on, and a later-loaded config file (project
/// over user) wins. The hot path also honors the `ALLOWLISTER_HISTORY` env var
/// as an override.
#[derive(Debug, Clone, Default)]
pub struct HistorySettings {
    /// Record each evaluation (verdict plus parsed subcommands) to disk.
    pub enabled: bool,
}

/// Blank out `//` line and `/* */` block comments so a config file may carry
/// explanatory notes that strict JSON forbids. Comment bytes are replaced with
/// spaces in place (newlines preserved), so the result is the same length as the
/// input and every byte keeps its offset — a later parse error still reports the
/// true line and column. String contents are untouched, so a `//` inside a value
/// (e.g. a `https://` URL glob) survives. Trailing commas are *not* accepted;
/// only comments are stripped.
///
/// Public so a consumer reading an allowlister config (which may carry comments)
/// can pre-process it the same way the loader does before handing it to a strict
/// JSON parser.
pub fn strip_jsonc_comments(input: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Normal,
        Str,
        Line,
        Block,
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut state = State::Normal;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match state {
            State::Normal => match (b, bytes.get(i + 1)) {
                (b'"', _) => {
                    out.push(b);
                    state = State::Str;
                }
                (b'/', Some(b'/')) => {
                    out.extend_from_slice(b"  ");
                    i += 2;
                    state = State::Line;
                    continue;
                }
                (b'/', Some(b'*')) => {
                    out.extend_from_slice(b"  ");
                    i += 2;
                    state = State::Block;
                    continue;
                }
                _ => out.push(b),
            },
            State::Str => {
                out.push(b);
                if b == b'\\' {
                    // Copy the escaped byte verbatim so an escaped quote does not
                    // end the string early.
                    if let Some(&next) = bytes.get(i + 1) {
                        out.push(next);
                        i += 2;
                        continue;
                    }
                } else if b == b'"' {
                    state = State::Normal;
                }
            }
            // Blank the comment body to spaces but keep newlines, so line and
            // column numbers downstream match the original file.
            State::Line => {
                if b == b'\n' {
                    out.push(b);
                    state = State::Normal;
                } else {
                    out.push(b' ');
                }
            }
            State::Block => {
                if b == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    out.extend_from_slice(b"  ");
                    i += 2;
                    state = State::Normal;
                    continue;
                }
                out.push(if b == b'\n' { b'\n' } else { b' ' });
            }
        }
        i += 1;
    }
    // Every substitution is ASCII-for-ASCII and multibyte bytes are copied or
    // uniformly blanked, so the buffer is still valid UTF-8.
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
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
    let contents = strip_jsonc_comments(contents);
    let raw: RawConfig = match serde_json::from_str(&contents) {
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
    // A later config file overrides the history toggle of an earlier one (project
    // over user), matching the merge order; an absent key leaves it untouched.
    if let Some(history) = &raw.history {
        if let Some(enabled) = history.enabled {
            config.history.enabled = enabled;
        }
    }
    for (index, raw_rule) in raw.rules.into_iter().enumerate() {
        match raw_rule.compile(display) {
            Ok(Compiled::Bash(rule)) => config.rules.push(rule),
            Ok(Compiled::Tool(rule)) => config.tool_rules.push(rule),
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
    #[serde(default)]
    history: Option<RawHistory>,
}

#[derive(Debug, Deserialize)]
struct RawHistory {
    #[serde(default)]
    enabled: Option<bool>,
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
    // Tool-rule fields. The presence of `tool` selects the non-shell engine; it
    // is then mutually exclusive with the bash-only fields above.
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    params: Option<BTreeMap<String, OneOrMany>>,
    #[serde(default)]
    jsonpath: Option<BTreeMap<String, OneOrMany>>,
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

/// A glob value that accepts either a single string or an array of strings, so
/// `"path": "/repo/**"` and `"path": ["/repo/**", "./**"]` both parse.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn to_vec(&self) -> Vec<String> {
        match self {
            OneOrMany::One(value) => vec![value.clone()],
            OneOrMany::Many(values) => values.clone(),
        }
    }
}

/// A compiled rule, either a shell rule or a tool rule.
enum Compiled {
    Bash(Rule),
    Tool(ToolRule),
}

impl RawRule {
    fn compile(&self, source: &str) -> Result<Compiled, String> {
        let action = Action::parse(self.action.as_deref())?;
        let kind = MatchKind::parse(self.kind.as_deref())?;
        let name = self.display_name();

        // The presence of `tool` selects the non-shell engine; everything else is
        // the existing bash path, byte-for-byte unchanged.
        if let Some(tool) = self.tool.as_deref() {
            return self
                .compile_tool(name, action, tool, kind, source)
                .map(Compiled::Tool);
        }

        // Tool-only fields on a bash rule are a mistake, not a silent no-op.
        if self.params.is_some() || self.jsonpath.is_some() {
            return Err("'params' and 'jsonpath' are only valid on a 'tool' rule".to_string());
        }

        let roles = self.compile_roles()?;
        let redirections = self.compile_redirections()?;
        let grant = self.compile_grant()?;

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

        let rule = match (&self.match_pattern, &self.argv) {
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
        }?;
        Ok(Compiled::Bash(rule))
    }

    /// Compile the non-shell tool rule. Bash-only fields are rejected so a
    /// `tool` rule and a shell rule never overlap.
    fn compile_tool(
        &self,
        name: String,
        action: Action,
        tool: &str,
        kind: MatchKind,
        source: &str,
    ) -> Result<ToolRule, String> {
        if self.match_pattern.is_some() || self.argv.is_some() {
            return Err("a 'tool' rule must not set 'match' or 'argv'".to_string());
        }
        if self.roles.is_some() {
            return Err("a 'tool' rule must not set 'roles' (a tool call has no role)".to_string());
        }
        if self.redirections.is_some() {
            return Err("a 'tool' rule must not set 'redirections'".to_string());
        }
        if self.grants.is_some() {
            return Err("a 'tool' rule must not set 'grants'".to_string());
        }
        let params = self.compile_params()?;
        let jsonpath = self.compile_jsonpath();
        ToolRule::compile(
            name,
            action,
            tool,
            kind,
            &params,
            &jsonpath,
            self.description.clone(),
            source.to_string(),
        )
    }

    fn compile_params(&self) -> Result<Vec<(ParamKey, Vec<String>)>, String> {
        let mut out = Vec::new();
        if let Some(map) = &self.params {
            for (key, globs) in map {
                let param = ParamKey::parse(key)
                    .ok_or_else(|| format!("unknown canonical param '{key}'"))?;
                out.push((param, globs.to_vec()));
            }
        }
        Ok(out)
    }

    fn compile_jsonpath(&self) -> Vec<(String, Vec<String>)> {
        self.jsonpath
            .iter()
            .flatten()
            .map(|(path, globs)| (path.clone(), globs.to_vec()))
            .collect()
    }

    fn compile_grant(&self) -> Result<Grant, String> {
        Grant::parse(self.grants.as_deref())
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
        if let Some(tool) = &self.tool {
            return tool.clone();
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
    fn schema_key_is_accepted_and_ignored() {
        // A `"$schema"` pointing at the published JSON Schema lets editors
        // validate the file; the loader treats it as a no-op, not a warning.
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"$schema":"https://nickderobertis.github.io/allowlister/allowlister.schema.json","rules":[{"name":"ls","match":"ls*","action":"allow"}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert_eq!(config.rules.len(), 1);
        assert!(config.warnings.is_empty());
    }

    #[test]
    fn strips_line_and_block_comments_preserving_offsets() {
        let src = "{\n  // a note\n  \"rules\": [] /* trailing */\n}";
        let out = strip_jsonc_comments(src);
        // Same length as the input: comments became spaces, newlines kept.
        assert_eq!(out.len(), src.len());
        assert!(!out.contains("a note"));
        assert!(!out.contains("trailing"));
        serde_json::from_str::<serde_json::Value>(&out).unwrap();
    }

    #[test]
    fn does_not_strip_comment_markers_inside_strings() {
        // A `//` inside a string value (a URL glob) must survive untouched.
        let src = r#"{"rules":[{"tool":"web_fetch","action":"allow","params":{"url":"https://github.com/**"}}]}"#;
        let out = strip_jsonc_comments(src);
        assert_eq!(out, src);
        // An escaped quote must not end the string early and expose a later `//`.
        let tricky = r#"{"name":"say \"hi\" // not a comment"}"#;
        assert_eq!(strip_jsonc_comments(tricky), tricky);
    }

    #[test]
    fn comments_in_a_config_file_load_cleanly() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            "{\n  // allow listing\n  \"rules\": [\n    { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" } /* ok */\n  ]\n}",
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
    fn ask_action_compiles() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"confirm","match":"npm publish*","action":"ask"}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert_eq!(config.rules.len(), 1);
        assert!(config.warnings.is_empty());
        assert_eq!(config.rules[0].action, crate::domain::Action::Ask);
    }

    #[test]
    fn unknown_action_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"r","match":"x","action":"maybe"}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert!(config.rules.is_empty());
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

    #[test]
    fn tool_rule_compiles_into_tool_rules() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"reads","tool":"read","action":"allow","params":{"path":["/repo/**"]}}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert!(config.rules.is_empty());
        assert_eq!(config.tool_rules.len(), 1);
        assert!(config.warnings.is_empty());
    }

    #[test]
    fn bash_rules_still_compile_unchanged_alongside_tool_rules() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[
                {"name":"git","match":"git status","action":"allow"},
                {"name":"reads","tool":"read","action":"allow","params":{"path":["/repo/**"]}}
            ]}"#,
        );
        let config = load_from_paths(&[path]);
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.tool_rules.len(), 1);
        assert!(config.warnings.is_empty());
    }

    #[test]
    fn tool_and_bash_keys_are_mutually_exclusive() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"bad","tool":"read","argv":["x"],"action":"allow"}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert!(config.rules.is_empty());
        assert!(config.tool_rules.is_empty());
        assert_eq!(config.warnings.len(), 1);
    }

    #[test]
    fn params_without_tool_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"name":"bad","match":"ls*","action":"allow","params":{"path":["/x"]}}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert!(config.rules.is_empty());
        assert_eq!(config.warnings.len(), 1);
    }

    #[test]
    fn unknown_canonical_param_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"tool":"read","action":"allow","params":{"nope":["x"]}}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert!(config.tool_rules.is_empty());
        assert_eq!(config.warnings.len(), 1);
    }

    #[test]
    fn param_glob_accepts_string_or_array() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[
                {"tool":"read","action":"allow","params":{"path":"/repo/**"}},
                {"tool":"web_fetch","action":"allow","params":{"url":["https://github.com/**","https://*.github.com/**"]}}
            ]}"#,
        );
        let config = load_from_paths(&[path]);
        assert_eq!(config.tool_rules.len(), 2);
        assert!(config.warnings.is_empty());
    }

    #[test]
    fn history_toggle_defaults_off_and_parses_and_overrides() {
        let dir = TempDir::new().unwrap();
        // Absent key: default off.
        let plain = write_config(&dir, "a.json", r#"{"rules":[]}"#);
        assert!(!load_from_paths(&[plain]).history.enabled);
        // Present and true: on.
        let on = write_config(&dir, "b.json", r#"{"history":{"enabled":true},"rules":[]}"#);
        assert!(load_from_paths(std::slice::from_ref(&on)).history.enabled);
        // A later file (project over user) wins: user on, project off → off.
        let off = write_config(&dir, "c.json", r#"{"history":{"enabled":false}}"#);
        assert!(!load_from_paths(&[on, off]).history.enabled);
    }

    #[test]
    fn mcp_raw_name_and_jsonpath_compile() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "c.json",
            r#"{"rules":[{"tool":"mcp__github__*","action":"deny","jsonpath":{"owner":["evilcorp"]}}]}"#,
        );
        let config = load_from_paths(&[path]);
        assert_eq!(config.tool_rules.len(), 1);
        assert!(config.warnings.is_empty());
    }
}

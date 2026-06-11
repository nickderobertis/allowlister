//! `allowlister config` — manage an allowlist beyond first-time setup: add a
//! single rule, remove one by name, or show the effective merged configuration
//! and where each rule comes from.
//!
//! `add` and `remove` are the surgical, single-rule siblings of `install` (which
//! layers a whole ruleset). They target the same configs — user-global by
//! default, a project `.allowlister.jsonc` with `--local`, or an explicit
//! `--output` path — and edit in place so comments and formatting survive. `add`
//! de-duplicates by name like `install`, so re-adding is a no-op.
//!
//! `show` is read-only: it discovers the same user + project configs the engine
//! merges (or one scope, with `--global`/`--local`), lists every configured rule
//! annotated with its source file, and surfaces any load warnings.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::commands::profile;
use crate::config;
use crate::errors::{Error, Result};
use crate::io::configfs::{self, Env};

use super::resolve_cwd;

/// Parsed `config add` inputs. The CLI guarantees exactly one of
/// `match_pattern`/`argv`/`tool` is set.
pub struct AddArgs {
    pub name: Option<String>,
    pub action: String,
    pub match_pattern: Option<String>,
    pub argv: Vec<String>,
    pub tool: Option<String>,
    pub kind: Option<String>,
    pub roles: Vec<String>,
    pub params: Vec<String>,
    pub jsonpath: Vec<String>,
    pub description: Option<String>,
    pub local: bool,
    pub output: Option<PathBuf>,
}

/// Parsed `config show` inputs.
pub struct ShowArgs {
    pub cwd: Option<PathBuf>,
    pub global: bool,
    pub local: bool,
    pub json: bool,
}

/// Add a single rule to the chosen config, creating the file if absent.
pub fn add(args: AddArgs) -> Result<i32> {
    let rule = build_rule(&args)?;
    // Vouch for the rule before touching any file: compile it in isolation so a
    // bad action/role/kind/glob fails loudly here instead of landing in the
    // config and being silently skipped at load time.
    let probe = Value::Object(
        [("rules".to_string(), Value::Array(vec![rule.clone()]))]
            .into_iter()
            .collect(),
    );
    let compiled = config::compile_str(&probe.to_string(), "the new rule");
    if !compiled.warnings.is_empty() {
        return Err(Error::InvalidConfig {
            origin: "the new rule".to_string(),
            message: compiled.warnings.join("\n"),
        });
    }

    let target = target_path(args.local, args.output.as_deref())?;
    let created = !target.exists();
    let merge = if created {
        let text = serde_json::to_string_pretty(&Value::Object(
            [("rules".to_string(), Value::Array(vec![rule]))]
                .into_iter()
                .collect(),
        ))
        .unwrap_or_default()
            + "\n";
        profile::write_file(&target, &text)?;
        profile::Merge {
            added: 1,
            skipped: 0,
            total: 1,
        }
    } else {
        let text = fs::read_to_string(&target).map_err(|err| Error::Read {
            path: target.clone(),
            source: err,
        })?;
        let label = target.display().to_string();
        let (updated, merge) = profile::merge_rules_text(&text, &label, vec![rule])?;
        if updated != text {
            profile::write_file(&target, &updated)?;
        }
        merge
    };

    let verb = if created { "Created" } else { "Updated" };
    println!("{verb} {}.", target.display());
    println!(
        "  {} rule(s) added, {} already present ({} total).",
        merge.added, merge.skipped, merge.total
    );
    Ok(0)
}

/// Remove the rule named `name` from the chosen config.
pub fn remove(name: &str, local: bool, output: Option<&Path>) -> Result<i32> {
    let target = target_path(local, output)?;
    let text = match fs::read_to_string(&target) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            println!("No config at {}; nothing to remove.", target.display());
            return Ok(0);
        }
        Err(err) => {
            return Err(Error::Read {
                path: target,
                source: err,
            })
        }
    };

    let label = target.display().to_string();
    let (updated, removed) =
        crate::jsonc::remove_rule(&text, name).map_err(|message| Error::InvalidConfig {
            origin: label,
            message,
        })?;
    if removed {
        profile::write_file(&target, &updated)?;
        println!("Removed rule '{name}' from {}.", target.display());
    } else {
        println!(
            "No rule named '{name}' in {}; nothing changed.",
            target.display()
        );
    }
    Ok(0)
}

/// Show the effective configuration: every configured rule, annotated with the
/// file it came from, plus the active history toggle and any load warnings.
pub fn show(args: ShowArgs) -> Result<i32> {
    let env = Env::from_process();
    let cwd = resolve_cwd(args.cwd.as_deref());
    let (scope, paths) = if args.global {
        (
            "user-global",
            configfs::user_config_path(&env).into_iter().collect(),
        )
    } else if args.local {
        ("project-local", configfs::project_config_paths(&cwd))
    } else {
        ("combined (user + project)", configfs::discover(&cwd, &env))
    };

    // The loader is the source of truth for warnings and the history toggle; the
    // raw per-file read below is only for displaying each rule with its source.
    let loaded = config::load_from_paths(&paths);
    let rules = read_rules(&paths);

    if args.json {
        print_json(scope, &loaded, &rules);
    } else {
        print_human(scope, &loaded, &rules);
    }
    Ok(0)
}

/// One configured rule paired with the file it was read from.
struct SourcedRule {
    source: String,
    rule: Value,
}

/// Read the `rules` array out of each config file, in discovery (merge) order,
/// pairing every rule with its source path. A file that cannot be read or parsed
/// contributes no rules here; the loader records the matching warning.
fn read_rules(paths: &[PathBuf]) -> Vec<SourcedRule> {
    let mut out = Vec::new();
    for path in paths {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let stripped = config::strip_jsonc_comments(&text);
        let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(&stripped) else {
            continue;
        };
        if let Some(Value::Array(rules)) = obj.remove("rules") {
            let source = path.display().to_string();
            for rule in rules {
                out.push(SourcedRule {
                    source: source.clone(),
                    rule,
                });
            }
        }
    }
    out
}

#[derive(Serialize)]
struct ShowRuleJson {
    source: String,
    #[serde(flatten)]
    rule: Value,
}

#[derive(Serialize)]
struct ShowJson<'a> {
    scope: &'a str,
    sources: &'a [String],
    history_enabled: bool,
    rules: Vec<ShowRuleJson>,
    warnings: &'a [String],
}

fn print_json(scope: &str, loaded: &config::LoadedConfig, rules: &[SourcedRule]) {
    let payload = ShowJson {
        scope,
        sources: &loaded.sources,
        history_enabled: loaded.history.enabled,
        rules: rules
            .iter()
            .map(|sourced| ShowRuleJson {
                source: sourced.source.clone(),
                rule: sourced.rule.clone(),
            })
            .collect(),
        warnings: &loaded.warnings,
    };
    if let Ok(line) = serde_json::to_string(&payload) {
        println!("{line}");
    }
}

fn print_human(scope: &str, loaded: &config::LoadedConfig, rules: &[SourcedRule]) {
    println!("allowlister configuration — {scope}");
    println!();

    println!("sources ({}):", loaded.sources.len());
    if loaded.sources.is_empty() {
        println!("  (none found)");
    }
    for source in &loaded.sources {
        println!("  - {source}");
    }
    println!();

    println!("rules ({}):", rules.len());
    if rules.is_empty() {
        println!("  (none)");
    } else {
        let name_width = rules
            .iter()
            .map(|sourced| display_name(&sourced.rule).len())
            .max()
            .unwrap_or(0)
            .min(40);
        for sourced in rules {
            let action = sourced
                .rule
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("allow")
                .to_uppercase();
            println!(
                "  {:<6} {:<nw$}  {:<32}  <- {}",
                action,
                display_name(&sourced.rule),
                matcher_desc(&sourced.rule),
                sourced.source,
                nw = name_width
            );
        }
    }
    println!();

    println!(
        "history recording: {}",
        if loaded.history.enabled { "on" } else { "off" }
    );

    if !loaded.warnings.is_empty() {
        println!();
        println!("warnings:");
        for warning in &loaded.warnings {
            println!("  ! {warning}");
        }
    }
}

/// The name a rule is shown under: its `name`, else its matcher, else a
/// placeholder — mirroring how the loader names an unnamed rule.
fn display_name(rule: &Value) -> String {
    if let Some(name) = rule.get("name").and_then(Value::as_str) {
        if !name.is_empty() {
            return name.to_string();
        }
    }
    if let Some(pattern) = rule.get("match").and_then(Value::as_str) {
        return pattern.to_string();
    }
    if let Some(argv) = rule.get("argv").and_then(Value::as_array) {
        return join_strings(argv, " ");
    }
    if let Some(tool) = rule.get("tool").and_then(Value::as_str) {
        return tool.to_string();
    }
    "(unnamed)".to_string()
}

/// A compact one-line description of what a rule matches.
fn matcher_desc(rule: &Value) -> String {
    if let Some(pattern) = rule.get("match").and_then(Value::as_str) {
        return format!("match {pattern}");
    }
    if let Some(argv) = rule.get("argv").and_then(Value::as_array) {
        return format!("argv {}", join_strings(argv, " "));
    }
    if let Some(tool) = rule.get("tool").and_then(Value::as_str) {
        let mut desc = format!("tool {tool}");
        let constraints = describe_constraints(rule.get("params"))
            .into_iter()
            .chain(describe_constraints(rule.get("jsonpath")))
            .collect::<Vec<_>>();
        if !constraints.is_empty() {
            desc.push(' ');
            desc.push_str(&constraints.join(" "));
        }
        return desc;
    }
    "(no matcher)".to_string()
}

/// Render a `params`/`jsonpath` map as `key=glob[,glob]` fragments for the
/// compact rule line.
fn describe_constraints(map: Option<&Value>) -> Vec<String> {
    let Some(Value::Object(map)) = map else {
        return Vec::new();
    };
    map.iter()
        .map(|(key, value)| {
            let globs = match value {
                Value::String(s) => s.clone(),
                Value::Array(items) => join_strings(items, ","),
                other => other.to_string(),
            };
            format!("{key}={globs}")
        })
        .collect()
}

fn join_strings(values: &[Value], sep: &str) -> String {
    values
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string())
        })
        .collect::<Vec<_>>()
        .join(sep)
}

/// Assemble the rule JSON object from the `add` flags, including only the fields
/// that were set so the rule stays minimal.
fn build_rule(args: &AddArgs) -> Result<Value> {
    let mut obj = Map::new();
    if let Some(name) = &args.name {
        obj.insert("name".to_string(), Value::String(name.clone()));
    }
    obj.insert("action".to_string(), Value::String(args.action.clone()));
    if let Some(pattern) = &args.match_pattern {
        obj.insert("match".to_string(), Value::String(pattern.clone()));
    }
    if !args.argv.is_empty() {
        obj.insert(
            "argv".to_string(),
            Value::Array(args.argv.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(tool) = &args.tool {
        obj.insert("tool".to_string(), Value::String(tool.clone()));
    }
    if let Some(kind) = &args.kind {
        obj.insert("kind".to_string(), Value::String(kind.clone()));
    }
    if !args.roles.is_empty() {
        obj.insert(
            "roles".to_string(),
            Value::Array(args.roles.iter().cloned().map(Value::String).collect()),
        );
    }
    if !args.params.is_empty() {
        obj.insert("params".to_string(), parse_kv(&args.params, "param")?);
    }
    if !args.jsonpath.is_empty() {
        obj.insert(
            "jsonpath".to_string(),
            parse_kv(&args.jsonpath, "jsonpath")?,
        );
    }
    if let Some(description) = &args.description {
        obj.insert(
            "description".to_string(),
            Value::String(description.clone()),
        );
    }
    Ok(Value::Object(obj))
}

/// Parse repeated `key=glob` flags into a JSON object whose values are arrays of
/// globs; a key given more than once accumulates its globs.
fn parse_kv(entries: &[String], flag: &str) -> Result<Value> {
    let mut map = Map::new();
    for entry in entries {
        let (key, value) = entry.split_once('=').ok_or_else(|| Error::InvalidConfig {
            origin: "config add".to_string(),
            message: format!("--{flag} must be key=value, got '{entry}'"),
        })?;
        let slot = map
            .entry(key.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(globs) = slot {
            globs.push(Value::String(value.to_string()));
        }
    }
    Ok(Value::Object(map))
}

/// Where an `add`/`remove` edit lands: an explicit `--output`, a project
/// `.allowlister.jsonc` under `--local`, or (the default) the user config.
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn add_args() -> AddArgs {
        AddArgs {
            name: None,
            action: "allow".to_string(),
            match_pattern: None,
            argv: Vec::new(),
            tool: None,
            kind: None,
            roles: Vec::new(),
            params: Vec::new(),
            jsonpath: Vec::new(),
            description: None,
            local: false,
            output: None,
        }
    }

    fn read(path: &Path) -> Value {
        let text = fs::read_to_string(path).unwrap();
        serde_json::from_str(&config::strip_jsonc_comments(&text)).unwrap()
    }

    #[test]
    fn add_creates_a_file_with_one_rule() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        let mut args = add_args();
        args.name = Some("ls".to_string());
        args.match_pattern = Some("ls*".to_string());
        args.output = Some(target.clone());
        assert_eq!(add(args).unwrap(), 0);

        let doc = read(&target);
        assert_eq!(doc["rules"][0]["name"], "ls");
        assert_eq!(doc["rules"][0]["match"], "ls*");
        assert_eq!(doc["rules"][0]["action"], "allow");
        // What we wrote loads cleanly.
        let loaded = config::load_from_paths(&[target]);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.rules.len(), 1);
    }

    #[test]
    fn add_merges_into_an_existing_file_and_dedupes_by_name() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.jsonc");
        fs::write(
            &target,
            "{\n  // mine\n  \"rules\": [\n    { \"name\": \"ls\", \"match\": \"ls*\", \"action\": \"allow\" } // keep\n  ]\n}\n",
        )
        .unwrap();

        let mut args = add_args();
        args.name = Some("pwd".to_string());
        args.match_pattern = Some("pwd".to_string());
        args.output = Some(target.clone());
        add(args).unwrap();

        let text = fs::read_to_string(&target).unwrap();
        assert!(text.contains("// mine"), "comments survive: {text}");
        assert!(text.contains("// keep"));
        let doc = read(&target);
        assert_eq!(doc["rules"].as_array().unwrap().len(), 2);

        // Re-adding the same name changes nothing.
        let mut again = add_args();
        again.name = Some("pwd".to_string());
        again.match_pattern = Some("pwd".to_string());
        again.output = Some(target.clone());
        add(again).unwrap();
        assert_eq!(read(&target)["rules"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn add_builds_a_tool_rule_with_params() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        let mut args = add_args();
        args.name = Some("reads".to_string());
        args.tool = Some("read".to_string());
        args.params = vec!["path=/repo/**".to_string()];
        args.output = Some(target.clone());
        add(args).unwrap();

        let doc = read(&target);
        assert_eq!(doc["rules"][0]["tool"], "read");
        assert_eq!(doc["rules"][0]["params"]["path"][0], "/repo/**");
        let loaded = config::load_from_paths(&[target]);
        assert_eq!(loaded.tool_rules.len(), 1);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn add_builds_a_shell_rule_with_argv_kind_roles_and_description() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        let mut args = add_args();
        args.name = Some("gh".to_string());
        args.argv = vec!["gh".to_string(), "pr".to_string(), "**".to_string()];
        args.kind = Some("glob".to_string());
        args.roles = vec!["pipe_filter".to_string()];
        args.description = Some("gh pr subcommands".to_string());
        args.output = Some(target.clone());
        add(args).unwrap();

        let doc = read(&target);
        assert_eq!(doc["rules"][0]["argv"][0], "gh");
        assert_eq!(doc["rules"][0]["kind"], "glob");
        assert_eq!(doc["rules"][0]["roles"][0], "pipe_filter");
        assert_eq!(doc["rules"][0]["description"], "gh pr subcommands");
        let loaded = config::load_from_paths(&[target]);
        assert_eq!(loaded.rules.len(), 1);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn add_builds_a_tool_rule_with_jsonpath() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        let mut args = add_args();
        args.name = Some("mcp-deny".to_string());
        args.action = "deny".to_string();
        args.tool = Some("mcp__github__*".to_string());
        args.jsonpath = vec!["owner=evilcorp".to_string()];
        args.output = Some(target.clone());
        add(args).unwrap();

        let doc = read(&target);
        assert_eq!(doc["rules"][0]["tool"], "mcp__github__*");
        assert_eq!(doc["rules"][0]["jsonpath"]["owner"][0], "evilcorp");
        let loaded = config::load_from_paths(&[target]);
        assert_eq!(loaded.tool_rules.len(), 1);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn add_rejects_an_invalid_rule_before_writing() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        let mut args = add_args();
        args.action = "maybe".to_string(); // not a real action
        args.match_pattern = Some("x".to_string());
        args.output = Some(target.clone());
        assert!(matches!(add(args), Err(Error::InvalidConfig { .. })));
        assert!(!target.exists(), "nothing is written on a bad rule");
    }

    #[test]
    fn add_rejects_a_malformed_param_flag() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        let mut args = add_args();
        args.tool = Some("read".to_string());
        args.params = vec!["noequalsign".to_string()];
        args.output = Some(target.clone());
        let err = add(args).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
        assert!(format!("{err}").contains("must be key=value"));
    }

    #[test]
    fn remove_deletes_a_rule_by_name() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        fs::write(
            &target,
            r#"{"rules":[{"name":"a","match":"a*","action":"allow"},{"name":"b","match":"b*","action":"allow"}]}"#,
        )
        .unwrap();
        assert_eq!(remove("a", false, Some(&target)).unwrap(), 0);
        let doc = read(&target);
        assert_eq!(doc["rules"].as_array().unwrap().len(), 1);
        assert_eq!(doc["rules"][0]["name"], "b");
    }

    #[test]
    fn remove_of_an_absent_name_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        let body = r#"{"rules":[{"name":"a","match":"a*","action":"allow"}]}"#;
        fs::write(&target, body).unwrap();
        assert_eq!(remove("nope", false, Some(&target)).unwrap(), 0);
        assert_eq!(fs::read_to_string(&target).unwrap(), body);
    }

    #[test]
    fn remove_from_a_missing_file_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("does-not-exist.json");
        assert_eq!(remove("a", false, Some(&target)).unwrap(), 0);
        assert!(!target.exists());
    }

    #[test]
    fn show_lists_rules_with_their_source() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("c.json");
        fs::write(
            &path,
            r#"{"rules":[{"name":"ls","match":"ls*","action":"allow"},{"name":"reads","tool":"read","action":"allow","params":{"path":["/repo/**"]}}]}"#,
        )
        .unwrap();
        let paths = [path.clone()];
        let loaded = config::load_from_paths(&paths);
        let rules = read_rules(&paths);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].source, path.display().to_string());

        // The human renderer mentions both rules and the source.
        assert_eq!(display_name(&rules[0].rule), "ls");
        assert_eq!(matcher_desc(&rules[0].rule), "match ls*");
        assert!(matcher_desc(&rules[1].rule).contains("tool read"));
        assert!(matcher_desc(&rules[1].rule).contains("path=/repo/**"));
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn show_json_flattens_rules_with_source() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("c.json");
        fs::write(
            &path,
            r#"{"rules":[{"name":"ls","match":"ls*","action":"allow"}]}"#,
        )
        .unwrap();
        let paths = [path.clone()];
        let loaded = config::load_from_paths(&paths);
        let rules = read_rules(&paths);
        let payload = ShowJson {
            scope: "combined",
            sources: &loaded.sources,
            history_enabled: loaded.history.enabled,
            rules: rules
                .iter()
                .map(|s| ShowRuleJson {
                    source: s.source.clone(),
                    rule: s.rule.clone(),
                })
                .collect(),
            warnings: &loaded.warnings,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["rules"][0]["name"], "ls");
        assert_eq!(value["rules"][0]["source"], path.display().to_string());
        assert_eq!(value["scope"], "combined");
    }

    #[test]
    fn display_name_falls_back_through_match_argv_tool() {
        assert_eq!(display_name(&serde_json::json!({ "match": "ls*" })), "ls*");
        assert_eq!(
            display_name(&serde_json::json!({ "argv": ["gh", "pr", "list"] })),
            "gh pr list"
        );
        assert_eq!(display_name(&serde_json::json!({ "tool": "read" })), "read");
        assert_eq!(display_name(&serde_json::json!({})), "(unnamed)");
    }

    #[test]
    fn matcher_desc_covers_argv_jsonpath_and_no_matcher() {
        assert_eq!(
            matcher_desc(&serde_json::json!({ "argv": ["gh", "pr", "list"] })),
            "argv gh pr list"
        );
        let tool = matcher_desc(&serde_json::json!({
            "tool": "mcp__github__*",
            "jsonpath": { "owner": "acme" }
        }));
        assert!(tool.contains("tool mcp__github__*"));
        assert!(tool.contains("owner=acme"));
        // A rule with none of match/argv/tool (never produced by `add`, but the
        // renderer must not panic on hand-written config).
        assert_eq!(matcher_desc(&serde_json::json!({})), "(no matcher)");
    }

    #[test]
    fn parse_kv_accumulates_repeated_keys() {
        let value = parse_kv(
            &[
                "path=/a/**".to_string(),
                "path=/b/**".to_string(),
                "url=https://x/**".to_string(),
            ],
            "param",
        )
        .unwrap();
        assert_eq!(value["path"][0], "/a/**");
        assert_eq!(value["path"][1], "/b/**");
        assert_eq!(value["url"][0], "https://x/**");
    }
}

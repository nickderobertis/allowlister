//! `allowlister install` — merge an allowlist (or a built-in profile) into a
//! target config, creating it if absent. The merge is by rule name, so
//! re-running is idempotent: a rule already present is left in place rather than
//! duplicated. An existing target is edited in place — its comments and
//! formatting survive — and a new one receives the source text verbatim, so a
//! profile's explanatory comments land in the file. This is the "layer a curated
//! ruleset onto what you already have" path; `init` writes a fresh config (and
//! wires up the hook). Both share the source resolution and merge in
//! [`super::profile`].

use std::fs;
use std::path::{Path, PathBuf};

use crate::errors::Error;
use crate::io::configfs::{self, Env};
use crate::{commands::profile, errors::Result};

/// Merge `source` into the chosen target config. `--global` (the default)
/// targets the user config, `--local` a project `.allowlister.jsonc`, and
/// `output` an explicit path. `source` is a built-in profile name or a file
/// path.
pub fn run(source: &str, _global: bool, local: bool, output: Option<&Path>) -> Result<i32> {
    let source = profile::resolve_source(source)?;
    profile::validate(&source)?;
    let incoming = profile::incoming_rules(&source)?;

    let target = target_path(local, output)?;
    let created = !target.exists();
    let merge = if created {
        // A fresh target gets the source text (comments and all), stamped with a
        // leading "$schema" so the new file validates in an editor.
        let total = incoming.len();
        profile::write_file(&target, &profile::ensure_schema(&source.text))?;
        profile::Merge {
            added: total,
            skipped: 0,
            total,
        }
    } else {
        let text = fs::read_to_string(&target).map_err(|err| Error::Read {
            path: target.clone(),
            source: err,
        })?;
        let label = target.display().to_string();
        let (updated, merge) = profile::merge_rules_text(&text, &label, incoming)?;
        // Backfill the "$schema" key on an existing config that lacks one; a
        // config that already declares it is left untouched, so this writes only
        // when rules were merged or the key was newly added.
        let updated = profile::ensure_schema(&updated);
        if updated != text {
            profile::write_file(&target, &updated)?;
        }
        merge
    };

    let verb = if created { "Created" } else { "Updated" };
    println!("{verb} {} from {}.", target.display(), source.label);
    println!(
        "  {} rule(s) added, {} already present ({} total).",
        merge.added, merge.skipped, merge.total
    );
    // A brand-new config still needs the hook wired up to do anything; point the
    // user at the snippet `init` would print, since `install` does not touch
    // harness settings itself.
    if created {
        println!();
        super::init::print_hook_setup();
    }

    Ok(0)
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
    configfs::default_user_config_path(&Env::from_process())
        .ok_or(crate::errors::Error::NoConfigHome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::errors::Error;
    use serde_json::Value;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::TempDir;

    // A created target carries the profile text verbatim — comments included —
    // so strip them the way the loader does before a strict parse.
    fn read(path: &Path) -> Value {
        let text = fs::read_to_string(path).unwrap();
        serde_json::from_str(&config::strip_jsonc_comments(&text)).unwrap()
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
    fn a_new_target_receives_the_profile_text_with_its_comments() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.jsonc");
        run("read-only", true, false, Some(target.as_path())).unwrap();
        let text = fs::read_to_string(&target).unwrap();
        assert!(
            text.contains("//"),
            "the profile's explanatory comments must land in the file"
        );
        // A fresh config is stamped with the canonical "$schema" as its first key.
        assert!(
            text.starts_with(&format!("{{\n  \"$schema\": \"{}\",\n", config::SCHEMA_URL)),
            "the new config must lead with a $schema key: {text}"
        );
        assert_eq!(read(&target)["$schema"], config::SCHEMA_URL);
    }

    #[test]
    fn merging_preserves_the_targets_comments_and_formatting() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.jsonc");
        let original = "{\n  // hand-written notes\n  \"rules\": [\n    { \"name\": \"keep\", \"match\": \"ls*\", \"action\": \"allow\" } // why I allow this\n  ]\n}\n";
        fs::write(&target, original).unwrap();
        let src = dir.path().join("src.json");
        fs::write(
            &src,
            r#"{"rules":[{"name":"x","match":"x*","action":"allow"}]}"#,
        )
        .unwrap();

        run(src.to_str().unwrap(), true, false, Some(target.as_path())).unwrap();
        let text = fs::read_to_string(&target).unwrap();
        // The hand-written comment survives; a leading "$schema" is backfilled
        // before the rules, whose trailing comment stays attached after the new
        // comma. Everything else is byte-for-byte untouched.
        let expected_prefix = format!(
            "{{\n  // hand-written notes\n  \"$schema\": \"{url}\",\n  \"rules\": [\n    {{ \"name\": \"keep\", \"match\": \"ls*\", \"action\": \"allow\" }}, // why I allow this\n",
            url = config::SCHEMA_URL,
        );
        assert!(
            text.starts_with(&expected_prefix),
            "comments and formatting must survive the merge: {text}"
        );
        assert_eq!(rule_names(&read(&target)), vec!["keep", "x"]);

        // A re-install adds nothing (rules present, $schema present) and leaves
        // the file byte-identical.
        run(src.to_str().unwrap(), true, false, Some(target.as_path())).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), text);
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

    #[test]
    fn reading_a_directory_target_errors() {
        let dir = TempDir::new().unwrap();
        // The target path is a directory, so reading it back as config fails.
        let target = dir.path().join("a-directory");
        fs::create_dir(&target).unwrap();
        let err = run("read-only", true, false, Some(target.as_path())).unwrap_err();
        assert!(matches!(err, Error::Read { .. }));
    }

    #[test]
    fn merging_into_a_non_object_target_errors() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        // Valid JSON, but a top-level array is not a config object.
        fs::write(&target, "[]").unwrap();
        let err = run("read-only", true, false, Some(target.as_path())).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
    }

    #[test]
    fn merging_into_a_target_with_non_array_rules_errors() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("config.json");
        fs::write(&target, r#"{"rules": 5}"#).unwrap();
        let err = run("read-only", true, false, Some(target.as_path())).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig { .. }));
    }
}

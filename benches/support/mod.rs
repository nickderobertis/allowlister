//! Fixtures shared by the bench targets (`engine`, `engine_allocs`). This file
//! lives in a subdirectory so cargo's bench auto-discovery never treats it as a
//! target of its own; each bench pulls it in with `#[path]`.

// Each bench target uses a subset of these helpers; the unused remainder in any
// one target is expected.
#![allow(dead_code)]

use std::path::PathBuf;

use allowlister::config;
use allowlister::domain::Rule;

/// Labelled commands covering the structural shapes the analyzer handles: a
/// bare command, a multi-stage pipeline, a redirection, command substitution,
/// an unsupported construct (a function definition, which must defer), and a
/// long `&&` chain that stresses fragment composition.
pub fn corpus() -> Vec<(&'static str, String)> {
    vec![
        ("simple", "ls -la".to_string()),
        ("pipeline", "gh pr list | head -20 | wc -l".to_string()),
        ("redirection", "echo hi > /tmp/x.txt".to_string()),
        ("substitution", "echo $(cat foo.txt | grep bar)".to_string()),
        ("unsupported", "f() { rm -rf /; }; f".to_string()),
        ("chain", chain(32)),
    ]
}

/// An `&&` chain of `len` simple commands, for stressing fragment composition.
pub fn chain(len: usize) -> String {
    (0..len)
        .map(|i| format!("echo step{i}"))
        .collect::<Vec<_>>()
        .join(" && ")
}

/// The canonical example config files (the same fixtures the e2e suite loads).
pub fn example_config_paths() -> [PathBuf; 2] {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    [
        manifest.join("examples/user-config.json"),
        manifest.join("examples/project-config.json"),
    ]
}

/// The example user + project rules, merged exactly as the e2e tests do.
/// `load_from_paths` touches the filesystem, so callers hoist it out of the
/// timed loop.
pub fn example_rules() -> Vec<Rule> {
    config::load_from_paths(&example_config_paths()).rules
}

/// Write a config of `n` synthetic rules — an even glob/regex/argv mix — to a
/// temp file and return it (keep the `TempDir` alive while the path is in use).
/// None of the rules match the [`corpus`] commands, so a `decide` over them is
/// the worst case: every rule is scanned for every fragment.
pub fn synthetic_config_file(n: usize) -> (tempfile::TempDir, PathBuf) {
    let rules: Vec<String> = (0..n)
        .map(|i| match i % 3 {
            0 => format!(r#"{{"name":"glob-{i}","match":"tool{i} *","action":"allow"}}"#),
            1 => format!(
                r#"{{"name":"regex-{i}","kind":"regex","match":"^tool{i}\\s+--safe(\\s.*)?$","action":"allow"}}"#
            ),
            _ => format!(
                r#"{{"name":"argv-{i}","argv":["tool{i}","sub*","--flag"],"action":"allow"}}"#
            ),
        })
        .collect();
    let json = format!(r#"{{"rules":[{}]}}"#, rules.join(","));
    let dir = tempfile::tempdir().expect("create temp dir for synthetic config");
    let path = dir.path().join("synthetic.json");
    std::fs::write(&path, json).expect("write synthetic config");
    (dir, path)
}

/// Compile `n` synthetic worst-case rules (see [`synthetic_config_file`]).
/// Asserts every rule compiled: a silently skipped rule would shrink the set
/// and corrupt the scaling curve rather than fail loudly.
pub fn synthetic_rules(n: usize) -> Vec<Rule> {
    let (_dir, path) = synthetic_config_file(n);
    let config = config::load_from_paths(&[path]);
    assert!(
        config.warnings.is_empty(),
        "synthetic rules must compile cleanly: {:?}",
        config.warnings
    );
    assert_eq!(config.rules.len(), n, "synthetic rule count mismatch");
    config.rules
}

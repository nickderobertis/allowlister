//! Integration coverage for the live-e2e CI matrix drift gate
//! (`scripts/check-e2e-matrix.sh`). The script is the single source of the
//! contract every `.github/workflows/e2e-*.yml` must match; these tests drive its
//! two observable outcomes — pass on the committed workflows, fail on a drifted
//! copy — so a regression in the gate (or an unnoticed contract change) is caught
//! here rather than only in CI. Unix-only: the gate runs under bash (its `just`
//! recipe pins a bash shell), and this exercises it the same way.
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use tempfile::TempDir;

/// Stage the gate script plus the real e2e workflows into a temp tree. The script
/// `cd`s to its own `../`, so it checks the copied fixtures — letting a test
/// mutate one without touching the repo.
fn stage() -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tmp = TempDir::new().unwrap();
    let scripts = tmp.path().join("scripts");
    let workflows = tmp.path().join(".github/workflows");
    fs::create_dir_all(&scripts).unwrap();
    fs::create_dir_all(&workflows).unwrap();
    fs::copy(
        root.join("scripts/check-e2e-matrix.sh"),
        scripts.join("check-e2e-matrix.sh"),
    )
    .unwrap();
    for entry in fs::read_dir(root.join(".github/workflows")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("e2e-") && name.ends_with(".yml") {
            fs::copy(&path, workflows.join(&name)).unwrap();
        }
    }
    tmp
}

fn run(tmp: &TempDir) -> Output {
    Command::new("bash")
        .arg(tmp.path().join("scripts/check-e2e-matrix.sh"))
        .output()
        .expect("failed to spawn bash")
}

fn mutate(tmp: &TempDir, file: &str, from: &str, to: &str) {
    let path = tmp.path().join(".github/workflows").join(file);
    let text = fs::read_to_string(&path).unwrap();
    let drifted = text.replacen(from, to, 1);
    assert_ne!(text, drifted, "fixture mutation for {file} changed nothing");
    fs::write(&path, drifted).unwrap();
}

#[test]
fn gate_passes_on_the_committed_workflows() {
    let tmp = stage();
    let out = run(&tmp);
    assert!(
        out.status.success(),
        "gate should pass on the committed workflows.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("all e2e workflows match the matrix contract"),
        "expected the success line in stdout",
    );
}

#[test]
fn gate_fails_when_a_workflow_readds_the_push_trigger() {
    let tmp = stage();
    mutate(
        &tmp,
        "e2e-goose.yml",
        "on:\n  pull_request:",
        "on:\n  push:\n    branches: [main]\n  pull_request:",
    );
    let out = run(&tmp);
    assert!(!out.status.success(), "gate should fail on a push trigger");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("still triggers on push"),
        "expected the push-trigger drift message.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn gate_fails_when_a_dispatch_option_is_dropped() {
    let tmp = stage();
    // Remove a canonical `os` dispatch option the gate now requires (the finding
    // that motivated widening the gate beyond the matrix arms).
    mutate(&tmp, "e2e-goose.yml", "          - macos-latest\n", "");
    let out = run(&tmp);
    assert!(
        !out.status.success(),
        "gate should fail on a missing option"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("missing option"),
        "expected the missing-option drift message.\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

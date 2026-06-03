//! Criterion micro-benchmarks for the pure decision engine.
//!
//! These measure the in-process work a single `allowlister` invocation does
//! after process start and config load: parse the bash AST into role-tagged
//! fragments (`analyze`), match those fragments against rules (`decide`), and
//! the two composed (`evaluate`). A fourth group times config load plus rule
//! compilation. Process startup and terminal I/O are deliberately excluded here
//! — `scripts/bench.sh` covers those end to end with hyperfine.
//!
//! Rules come from the canonical `examples/` fixtures (the same files the e2e
//! suite loads), read once outside every timed loop so the numbers reflect the
//! engine rather than fixture parsing. That rule set is intentionally small
//! (~15 rules); `decide` cost scales with rule count and regex complexity, so
//! these are a floor, not a worst case.

use std::hint::black_box;
use std::path::PathBuf;

use allowlister::config;
use allowlister::domain::{self, Analysis, Rule};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

/// Labelled commands covering the structural shapes the analyzer handles: a
/// bare command, a multi-stage pipeline, a redirection, command substitution,
/// an unsupported construct (a function definition, which must defer), and a
/// long `&&` chain that stresses fragment composition.
fn corpus() -> Vec<(&'static str, String)> {
    let chain = (0..32)
        .map(|i| format!("echo step{i}"))
        .collect::<Vec<_>>()
        .join(" && ");
    vec![
        ("simple", "ls -la".to_string()),
        ("pipeline", "gh pr list | head -20 | wc -l".to_string()),
        ("redirection", "echo hi > /tmp/x.txt".to_string()),
        ("substitution", "echo $(cat foo.txt | grep bar)".to_string()),
        ("unsupported", "f() { rm -rf /; }; f".to_string()),
        ("chain", chain),
    ]
}

/// The example user + project rules, merged exactly as the e2e tests do.
/// `load_from_paths` touches the filesystem, so callers hoist it out of the
/// timed loop.
fn example_rules() -> Vec<Rule> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    config::load_from_paths(&[
        manifest.join("examples/user-config.json"),
        manifest.join("examples/project-config.json"),
    ])
    .rules
}

fn bench_analyze(c: &mut Criterion) {
    let mut group = c.benchmark_group("analyze");
    for (name, cmd) in corpus() {
        group.bench_with_input(BenchmarkId::from_parameter(name), &cmd, |b, cmd| {
            b.iter(|| domain::analyze(black_box(cmd)));
        });
    }
    group.finish();
}

fn bench_decide(c: &mut Criterion) {
    let rules = example_rules();
    let mut group = c.benchmark_group("decide");
    for (name, cmd) in corpus() {
        // Parse once, outside the timer: this group isolates rule matching.
        let analysis: Analysis = domain::analyze(&cmd);
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &analysis,
            |b, analysis| {
                b.iter(|| domain::decide(black_box(analysis), black_box(rules.as_slice())));
            },
        );
    }
    group.finish();
}

fn bench_evaluate(c: &mut Criterion) {
    let rules = example_rules();
    let mut group = c.benchmark_group("evaluate");
    for (name, cmd) in corpus() {
        group.bench_with_input(BenchmarkId::from_parameter(name), &cmd, |b, cmd| {
            b.iter(|| domain::evaluate(black_box(cmd), black_box(rules.as_slice())));
        });
    }
    group.finish();
}

fn bench_config_load(c: &mut Criterion) {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let paths = [
        manifest.join("examples/user-config.json"),
        manifest.join("examples/project-config.json"),
    ];
    c.bench_function("config_load/examples", |b| {
        b.iter(|| config::load_from_paths(black_box(&paths)));
    });
}

criterion_group!(
    benches,
    bench_analyze,
    bench_decide,
    bench_evaluate,
    bench_config_load
);
criterion_main!(benches);

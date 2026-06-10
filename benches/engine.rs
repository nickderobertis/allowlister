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
//! (~15 rules), so those groups are a realistic floor; the `decide_scaling` and
//! `config_load/synthetic` groups chart how cost grows with rule count and
//! fragment count, using synthetic worst-case rule sets where nothing matches
//! and every rule is scanned.

use std::hint::black_box;

use allowlister::config;
use allowlister::domain::{self, Analysis};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

#[path = "support/mod.rs"]
mod support;

use support::{corpus, example_rules, synthetic_config_file, synthetic_rules};

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

/// How `decide` scales with rule count: a fixed three-stage pipeline against
/// synthetic rule sets where nothing matches, so every rule is tried for every
/// fragment — the full-scan worst case that bounds large user allowlists.
fn bench_decide_rule_scaling(c: &mut Criterion) {
    let analysis = domain::analyze("gh pr list | head -20 | wc -l");
    let mut group = c.benchmark_group("decide_scaling/rules");
    for n in [10usize, 100, 1000] {
        let rules = synthetic_rules(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &rules, |b, rules| {
            b.iter(|| domain::decide(black_box(&analysis), black_box(rules.as_slice())));
        });
    }
    group.finish();
}

/// How `decide` scales with fragment count: `&&` chains of growing length
/// against a fixed 100-rule synthetic set. Together with `decide_scaling/rules`
/// this charts both axes of the fragments × rules scan.
fn bench_decide_fragment_scaling(c: &mut Criterion) {
    let rules = synthetic_rules(100);
    let mut group = c.benchmark_group("decide_scaling/fragments");
    for len in [4usize, 16, 64] {
        let analysis = domain::analyze(&support::chain(len));
        group.bench_with_input(
            BenchmarkId::from_parameter(len),
            &analysis,
            |b, analysis| {
                b.iter(|| domain::decide(black_box(analysis), black_box(rules.as_slice())));
            },
        );
    }
    group.finish();
}

fn bench_config_load(c: &mut Criterion) {
    let paths = support::example_config_paths();
    c.bench_function("config_load/examples", |b| {
        b.iter(|| config::load_from_paths(black_box(&paths)));
    });
}

/// How config load + rule compilation scales with rule count — the startup
/// cost a large allowlist adds to every spawned invocation.
fn bench_config_load_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("config_load/synthetic");
    for n in [10usize, 100, 1000] {
        // The TempDir must outlive the timed loop that reads the file.
        let (_dir, path) = synthetic_config_file(n);
        let paths = [path];
        group.bench_with_input(BenchmarkId::from_parameter(n), &paths, |b, paths| {
            b.iter(|| config::load_from_paths(black_box(paths)));
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_analyze,
    bench_decide,
    bench_evaluate,
    bench_decide_rule_scaling,
    bench_decide_fragment_scaling,
    bench_config_load,
    bench_config_load_scaling
);
criterion_main!(benches);

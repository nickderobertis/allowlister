# allowlister task runner.
#
# Conventions:
# - Successful recipes print little or nothing beyond the tool's own minimal
#   output. Diagnostics live in explicit recipes (`doctor`, `cargo-tree`).
# - Failing recipes preserve actionable output (paths, lints, diffs, codes).
# - Every recipe pins dependencies with `--locked`.

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# Minimum coverage enforced by `test-cov` — applied to lines, functions, and
# regions alike (a miss in any fails the command). Actual coverage sits just
# above this on all three metrics; most of what remains uncovered is defensive
# I/O error handling that cannot fail under test (root reads everything, valid
# JSON always serializes). New code that adds reachable branches should ship with
# tests rather than lean on the margin.
cov-min := "95"

# Pinned developer tool versions (installed by `bootstrap`). CI installs the
# latest of each via the install action; these pins keep local setups reproducible.
nextest-version := "0.9.137"
llvmcov-version := "0.8.7"
deny-version := "0.19.8"
machete-version := "0.9.2"
audit-version := "0.22.1"

# Tools for the informational performance suite (`bench*`, `profile`). Not part
# of the quality gate; CI installs the latest via the install action.
hyperfine-version := "1.20.0"
critcmp-version := "0.1.8"
samply-version := "0.13.1"

_default:
    @just --list --unsorted

# Install developer tools (cargo subcommands + git hooks) reproducibly.
bootstrap:
    @echo "» installing cargo-binstall (if missing)"
    @command -v cargo-binstall >/dev/null || cargo install --locked cargo-binstall
    @echo "» installing pinned dev tools"
    cargo binstall --no-confirm --disable-telemetry \
        cargo-nextest@{{nextest-version}} \
        cargo-llvm-cov@{{llvmcov-version}} \
        cargo-deny@{{deny-version}} \
        cargo-machete@{{machete-version}} \
        cargo-audit@{{audit-version}}
    @command -v lefthook >/dev/null || cargo binstall --no-confirm --disable-telemetry lefthook
    @echo "» installing benchmark + profiling tools"
    cargo binstall --no-confirm --disable-telemetry \
        hyperfine@{{hyperfine-version}} \
        critcmp@{{critcmp-version}} \
        samply@{{samply-version}}
    @just hooks-install
    @echo "✓ bootstrap complete"

# Fetch locked dependencies and verify the pinned toolchain is present.
sync:
    cargo fetch --locked
    @rustc --version

# Run the CLI with arbitrary arguments, e.g. `just run explain 'git status'`.
run *args:
    @cargo run --quiet --locked -- {{args}}

# Format the workspace in place.
fmt:
    cargo fmt --all

# Check formatting without writing (fails on any diff).
fmt-check:
    cargo fmt --all --check

# Type-check all targets and features.
check:
    cargo check --locked --all-targets --all-features

# Lint with every warning treated as an error.
clippy:
    cargo clippy --locked --all-targets --all-features -- -D warnings

# Apply machine-applicable clippy fixes.
clippy-fix:
    cargo clippy --fix --allow-dirty --allow-staged --locked --all-targets --all-features

# Run unit + integration tests (excludes the slower binary e2e suite).
test:
    cargo nextest run --locked --status-level fail -E 'not binary(e2e)'

# Re-run tests on change (requires cargo-watch; not part of the quality gate).
test-watch:
    cargo watch -x 'nextest run --locked -E "not binary(e2e)"'

# Run the end-to-end suite that drives the compiled binary.
test-e2e:
    cargo nextest run --locked --status-level fail -E 'binary(e2e)'

# Live check against the real `claude` CLI (needs Claude Code + auth + network; opt-in, not in full-check).
test-claude:
    @bash scripts/e2e-claude.sh

# Live check against the real `cursor-agent` CLI (needs Cursor CLI + auth + network; opt-in, not in full-check).
test-cursor:
    @bash scripts/e2e-cursor.sh

# Live check against the real `qwen` CLI (needs Qwen Code + a provider key + network; opt-in, not in full-check).
test-qwen:
    @bash scripts/e2e-qwen.sh

# Enforce line, function, and region coverage across all tests; a miss in any
# one fails the command.
test-cov:
    cargo llvm-cov nextest --locked --all-features \
        --ignore-filename-regex '(src/main\.rs|tests/)' \
        --fail-under-lines {{cov-min}} \
        --fail-under-functions {{cov-min}} \
        --fail-under-regions {{cov-min}}

# Build the API docs (warnings are errors).
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features

# Security advisories for the dependency tree.
security:
    cargo deny --locked check advisories

# Dependency hygiene: bans, licenses, sources, and unused dependencies.
deps-check:
    cargo deny --locked check bans licenses sources
    cargo machete

# Check the crate against its declared minimum supported Rust version.
# Requires the MSRV toolchain (`rustup toolchain install 1.88.0`).
msrv:
    cargo +1.88.0 check --locked --all-targets --all-features

# Install git hooks.
hooks-install:
    lefthook install

# Run the pre-commit hook set against the working tree.
hooks:
    lefthook run pre-commit --all-files

# Debug build.
build:
    cargo build --locked

# Optimized release build.
build-release:
    cargo build --release --locked

# Verify the release plan (targets + packaging) without publishing.
dist-plan:
    @bash scripts/dist.sh plan

# Build and package a release archive for the host target locally.
dist-build:
    @bash scripts/dist.sh build

# --- Performance suite (informational; never part of `full-check`) -----------
# Benchmarks are non-deterministic on shared hardware, so they measure rather
# than gate — like the live `test-claude` check. `just check`/`clippy` already
# type-check `benches/`, so the bench can't rot without a gate phase of its own.

# Engine micro-benchmarks (Criterion); saves the `current` baseline for bench-compare.
bench:
    cargo bench --locked --bench engine -- --save-baseline current

# Save current engine benchmarks as the `base` baseline (run on the comparison point).
bench-base:
    cargo bench --locked --bench engine -- --save-baseline base

# Diff the latest `bench` run against `base` (run `bench-base` first; needs critcmp).
bench-compare:
    critcmp base current

# End-to-end CLI latency for every command (hyperfine); writes target/bench/results.*.
bench-cli:
    @bash scripts/bench.sh

# Fast smoke check of the CLI benchmark harness (one run, no warmup, no stable numbers).
bench-cli-smoke:
    @bash scripts/bench.sh --dry-run

# Run both benchmark layers (Criterion + hyperfine).
bench-all: bench bench-cli

# Record a sampling profile to find bottlenecks (samply); see scripts/profile.sh for modes.
profile *args:
    @bash scripts/profile.sh {{args}}

# Full quality gate. Stops at the first failing phase; minimal output on success.
full-check:
    #!/usr/bin/env bash
    set -euo pipefail
    phase() { printf '\n» %s\n' "$1"; }
    phase "format";        just fmt-check
    phase "check";         just check
    phase "clippy";        just clippy
    phase "test";          just test
    phase "test-e2e";      just test-e2e
    phase "coverage";      just test-cov
    phase "deps-check";    just deps-check
    phase "security";      just security
    phase "docs";          just doc
    phase "release build"; just build-release
    phase "dist-plan";     just dist-plan
    printf '\n✓ full-check passed\n'

# Remove build artifacts.
clean:
    cargo clean

# Noisy environment diagnostics (never part of the quality gate).
doctor:
    @echo "## toolchain" && rustc --version && cargo --version
    @echo "## components" && (rustup component list --installed 2>/dev/null || echo "rustup not present")
    @echo "## tools" && for t in just cargo-nextest cargo-llvm-cov cargo-deny cargo-machete lefthook hyperfine critcmp samply; do printf '%-16s ' "$t"; command -v "$t" || echo "MISSING"; done
    @echo "## outdated (informational)" && (cargo outdated 2>/dev/null || echo "cargo-outdated not installed")

# Print the full dependency tree (diagnostic).
cargo-tree:
    cargo tree --locked --all-features

# Update dependencies and the lockfile. Review the diff before committing.
# May change Cargo.lock, transitive versions, and (re)generated release config.
upgrade:
    cargo update
    @echo "✓ updated Cargo.lock — run 'just full-check' and review 'git diff'"

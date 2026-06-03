# allowlister task runner.
#
# Conventions:
# - Successful recipes print little or nothing beyond the tool's own minimal
#   output. Diagnostics live in explicit recipes (`doctor`, `cargo-tree`).
# - Failing recipes preserve actionable output (paths, lints, diffs, codes).
# - Every recipe pins dependencies with `--locked`.

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# Minimum line coverage enforced by `test-cov` (a miss fails the command).
cov-min := "85"

# Pinned developer tool versions (installed by `bootstrap`).
nextest-version := "0.9.137"
llvmcov-version := "0.6.20"
deny-version := "0.18.5"
machete-version := "0.8.0"
audit-version := "0.21.2"

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

# Enforce line coverage across all tests; a miss fails the command.
test-cov:
    cargo llvm-cov nextest --locked --all-features \
        --ignore-filename-regex '(src/main\.rs|tests/)' \
        --fail-under-lines {{cov-min}}

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
    @echo "## tools" && for t in just cargo-nextest cargo-llvm-cov cargo-deny cargo-machete lefthook; do printf '%-16s ' "$t"; command -v "$t" || echo "MISSING"; done
    @echo "## outdated (informational)" && (cargo outdated 2>/dev/null || echo "cargo-outdated not installed")

# Print the full dependency tree (diagnostic).
cargo-tree:
    cargo tree --locked --all-features

# Update dependencies and the lockfile. Review the diff before committing.
# May change Cargo.lock, transitive versions, and (re)generated release config.
upgrade:
    cargo update
    @echo "✓ updated Cargo.lock — run 'just full-check' and review 'git diff'"

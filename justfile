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

# One-command machine setup (asdf + direnv + toolchain + tools + hooks; idempotent).
setup:
    @bash scripts/setup.sh

# Fast check of whether this machine is set up (no installs; non-zero if not).
setup-check:
    @bash scripts/setup-check.sh

# Install developer tools (cargo subcommands + git hooks) reproducibly.
# Resilient to locked-down networks: when no prebuilt binary is reachable it
# falls back to a source build, which the pinned toolchain is kept new enough to
# complete (the cargo tools' deps require a recent rustc).
bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "» installing cargo-binstall (if missing)"
    if ! command -v cargo-binstall >/dev/null; then
        # Prefer the prebuilt installer over `cargo install` from source: building
        # cargo-binstall itself can need a newer rustc than the tools below.
        curl -L --proto '=https' --tlsv1.2 -sSf \
            https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
    fi
    # Prefer a prebuilt binary; if every binary source is unreachable, build from
    # source so a network-restricted environment can still provision.
    # `--force` so a binary already present (a warm CI cache, a prior run) is
    # reinstalled rather than erroring with "already exists in destination".
    # cargo-binstall reads GITHUB_TOKEN from the env (set in CI) to authenticate
    # its GitHub API calls and avoid the rate-limit 403 that triggers the
    # source-build fallback in the first place.
    binstall_or_build() {
        cargo binstall --no-confirm --disable-telemetry --force "$1" \
            || { echo "» no prebuilt binary reachable for $1 — building from source"; cargo install --locked --force "$1"; }
    }
    echo "» installing pinned dev tools"
    for tool in \
        cargo-nextest@{{nextest-version}} \
        cargo-llvm-cov@{{llvmcov-version}} \
        cargo-deny@{{deny-version}} \
        cargo-machete@{{machete-version}} \
        cargo-audit@{{audit-version}}; do
        binstall_or_build "$tool"
    done
    # lefthook is a Go binary (no cargo source build), so install the prebuilt
    # only and warn rather than fail if it cannot be reached.
    if ! command -v lefthook >/dev/null; then
        cargo binstall --no-confirm --disable-telemetry --force lefthook \
            || echo "! lefthook unavailable (no prebuilt reachable); install it manually to enable git hooks"
    fi
    echo "» installing benchmark + profiling tools"
    for tool in \
        hyperfine@{{hyperfine-version}} \
        critcmp@{{critcmp-version}} \
        samply@{{samply-version}}; do
        binstall_or_build "$tool"
    done
    if command -v lefthook >/dev/null; then
        just hooks-install
    else
        echo "» skipping git hooks (lefthook missing)"
    fi
    echo "✓ bootstrap complete"

# Fetch locked dependencies and verify the pinned toolchain is present.
sync:
    cargo fetch --locked
    @rustc --version

# Run the CLI with arbitrary arguments, e.g. `just run explain 'git status'`.
run *args:
    @cargo run --quiet --locked -- {{args}}

# Format the workspace in place.
format:
    cargo fmt --all

# Alias for `format` (kept for muscle memory and existing docs).
fmt: format

# Check formatting without writing (fails on any diff).
fmt-check:
    cargo fmt --all --check

# Type-check all targets and features (a phase of the `check` gate).
typecheck:
    cargo check --locked --all-targets --all-features

# Lint with every warning treated as an error.
lint:
    cargo clippy --locked --all-targets --all-features -- -D warnings

# Alias for `lint` (kept for muscle memory and existing docs).
clippy: lint

# Apply machine-applicable clippy fixes.
clippy-fix:
    cargo clippy --fix --allow-dirty --allow-staged --locked --all-targets --all-features

# Drift gate for the live-e2e CI matrix contract: assert every
# .github/workflows/e2e-*.yml matches the single source in
# scripts/check-e2e-matrix.sh (no push trigger; claude/codex keep the full PR
# matrix, the rest Linux-only on PR; on-demand `os` dispatch). Keeps the matrix
# duplication GitHub Actions forces from drifting. See .github/AGENTS.md.
lint-workflows:
    @bash scripts/check-e2e-matrix.sh

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

# Live check against the real `codex` CLI (needs Codex CLI + auth + network; opt-in, not in full-check).
test-codex:
    @bash scripts/e2e-codex.sh

# Live check against the real `copilot` CLI (needs Copilot CLI + auth + network; opt-in, not in full-check).
test-copilot:
    @bash scripts/e2e-copilot.sh

# Live check against the real `crush` CLI (needs Crush + a provider key + network; opt-in, not in full-check).
test-crush:
    @bash scripts/e2e-crush.sh

# Live check against the real `qwen` CLI (needs Qwen Code + a provider key + network; opt-in, not in full-check).
test-qwen:
    @bash scripts/e2e-qwen.sh

# Live check against the real `goose` CLI (needs Goose + a provider key + network; opt-in, not in full-check).
test-goose:
    @bash scripts/e2e-goose.sh

# Live check against the real `opencode` CLI (needs OpenCode + a provider key + network; opt-in, not in full-check).
test-opencode:
    @bash scripts/e2e-opencode.sh

# Install the refine-allowlist skill via `gh skill` and assert its CLI contract (needs gh 2.93+; opt-in, not in full-check).
verify-skill:
    @bash scripts/verify-skill-install.sh

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

# Validate every shipped config against the published JSON Schema, catching a
# schema that drifts too strict for the configs the loader accepts. Python-based
# (needs `jsonschema`: CI installs it, locally `pip install jsonschema`), so it
# stays out of the Rust `check` gate and runs as its own CI job.
schema-check:
    python3 scripts/validate-schema.py

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

# Deterministic engine allocation counts (counting allocator; exact, comparable across commits).
bench-allocs:
    cargo bench --locked --quiet --bench engine_allocs

# Deterministic end-to-end CLI instruction counts (cachegrind; Linux-only, needs valgrind).
bench-instructions:
    @bash scripts/bench-instructions.sh

# Run the portable benchmark layers (Criterion + hyperfine + allocation counts).
bench-all: bench bench-cli bench-allocs

# Record a sampling profile to find bottlenecks (samply); see scripts/profile.sh for modes.
profile *args:
    @bash scripts/profile.sh {{args}}

# Full quality gate. Stops at the first failing phase; minimal output on success.
# This is THE gate: format, type-check, lint, the full test suite (unit +
# integration + binary e2e), enforced coverage, then dependency/security/docs/
# release checks. `bootstrap` then `check` is what CI runs and what proves the
# artifact; nothing here is warnings-only.
check:
    #!/usr/bin/env bash
    set -euo pipefail
    phase() { printf '\n» %s\n' "$1"; }
    phase "format";        just fmt-check
    phase "typecheck";     just typecheck
    phase "lint";          just lint
    phase "lint-workflows"; just lint-workflows
    phase "test";          just test
    phase "test-e2e";      just test-e2e
    phase "coverage";      just test-cov
    phase "deps-check";    just deps-check
    phase "security";      just security
    phase "docs";          just doc
    phase "release build"; just build-release
    phase "dist-plan";     just dist-plan
    printf '\n✓ check passed\n'

# Alias for `check` (kept so existing docs/scripts that say `full-check` work).
full-check: check

# Remove build artifacts.
clean:
    cargo clean

# Noisy environment diagnostics (never part of the quality gate).
doctor:
    @echo "## toolchain" && rustc --version && cargo --version
    @echo "## components" && (rustup component list --installed 2>/dev/null || echo "rustup not present")
    @echo "## tools" && for t in just cargo-nextest cargo-llvm-cov cargo-deny cargo-machete lefthook hyperfine critcmp samply valgrind; do printf '%-16s ' "$t"; command -v "$t" || echo "MISSING"; done
    @echo "## outdated (informational)" && (cargo outdated 2>/dev/null || echo "cargo-outdated not installed")

# Print the full dependency tree (diagnostic).
cargo-tree:
    cargo tree --locked --all-features

# Update dependencies and the lockfile. Review the diff before committing.
# May change Cargo.lock, transitive versions, and (re)generated release config.
upgrade:
    cargo update
    @echo "✓ updated Cargo.lock — run 'just full-check' and review 'git diff'"

# Install/refresh the optional llmlint toolchain. Idempotent.
setup-llmlint:
    ./scripts/setup-llmlint.sh

# Optional LLM-as-judge lint; non-deterministic and out of `check`.
lint-llm *paths:
    llmlint {{paths}}

# Deterministic llmlint config/ignore/version-bump validation.
lint-llm-validate *args:
    PATH="$HOME/.local/bin:$PATH" llmlint validate {{args}}

# llmlint scoped to changed files since the merge-base with main.
lint-llm-diff base="origin/main" *args:
    llmlint --diff --diff-base "{{base}}" {{args}}

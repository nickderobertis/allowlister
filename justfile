# allowlister task runner.
#
# Conventions:
# - Successful recipes print little or nothing beyond the tool's own minimal
#   output. Diagnostics live in explicit recipes (`doctor`, `cargo-tree`).
# - Failing recipes preserve actionable output (paths, lints, diffs, codes).
# - Every recipe pins dependencies with `--locked`.
# - The gate recipes DELEGATE to Nx (`scripts/nx`): each project.json declares
#   what its targets run; the root only chooses which projects run them, by tier
#   (`affected`, the default, from the base scripts/nx-base.sh prints; or `all`).
#   A mistyped tier aborts rather than quietly buying a weaker one.

set shell := ["bash", "-eu", "-o", "pipefail", "-c"]


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
    # Nx (the orchestrator every gate recipe delegates to) from the locked
    # package-lock.json; scripts/nx runs `npm ci` whenever the lock moved.
    bash scripts/nx --version >/dev/null
    echo "✓ bootstrap complete"

# Fetch locked dependencies and verify the pinned toolchain is present.
sync:
    cargo fetch --locked
    @rustc --version

# Run the CLI with arbitrary arguments, e.g. `just run explain 'git status'`.
run *args:
    @cargo run --quiet --locked -- {{args}}

# Run Nx targets at a tier: `affected` (projects this change can reach, from an
# explicit merge base) or `all` (every gate-eligible project). The live harness
# suites and the skill install check declare no gate target (theirs are `live` /
# `verify-install`), and every tiered run also excludes `tag:type:live`, so
# neither tier can reach them.
[private]
nx-tier tier +args:
    #!/usr/bin/env bash
    set -euo pipefail
    case {{ quote(tier) }} in
        affected) base="$(bash scripts/nx-base.sh)"; exec bash scripts/nx affected --base="$base" --exclude=tag:type:live {{ args }} ;;
        all) exec bash scripts/nx run-many --exclude=tag:type:live {{ args }} ;;
        *) printf "unknown tier '%s' — use 'affected' (the default) or 'all'\n" {{ quote(tier) }} >&2; exit 2 ;;
    esac

# Format in place (each project's `format` target).
format tier="affected": (nx-tier tier "-t format")

# Alias for `format` (kept for muscle memory and existing docs).
fmt tier="affected": (format tier)

# Check formatting without writing (fails on any diff).
fmt-check tier="affected": (nx-tier tier "-t format-check")

# llmlint: ignore-block[diagnostics_error_or_absent] every rustc warning already fails the gate: the `lint` target runs clippy with -D warnings over the same crates, --all-targets and --all-features, so a warning cargo check prints here is an error one target over; repeating -D warnings via RUSTFLAGS would make cargo rebuild the dependency graph for each flag set.
# Type-check all targets and features of each affected crate.
typecheck tier="affected": (nx-tier tier "-t typecheck")
# llmlint: ignore-end[diagnostics_error_or_absent]

# Covers clippy per crate, the workflow-matrix drift gate and the project
# boundaries.
# Lint with every warning treated as an error.
lint tier="affected": (nx-tier tier "-t lint")

# Clippy (-D warnings) over the Rust crates only: their `lint` targets.
clippy tier="affected": (nx-tier tier "-t lint --projects=tag:lang:rust")

# Apply machine-applicable clippy fixes across the workspace.
clippy-fix:
    cargo clippy --fix --allow-dirty --allow-staged --locked --workspace --all-targets --all-features

# The ci-workflows project's lint target (scripts/check-e2e-matrix.sh), so `lint`
# runs it too; the contract it holds is in .github/AGENTS.md.
# Drift gate for the live-e2e CI matrix contract.
lint-workflows:
    @bash scripts/nx run ci-workflows:lint

# Covers unit + integration, the schema validation, and the workflow-contract
# and boundary-checker suites.
# Run every test target except the binary e2e suite.
test tier="affected": (nx-tier tier "-t test --exclude=tag:type:e2e")

# Re-run tests on change (requires cargo-watch; not part of the quality gate).
test-watch:
    cargo watch -x 'nextest run --locked -p allowlister'

# allowlister-e2e:test, after allowlister:build; `check` runs it too.
# Run the end-to-end suite that drives the compiled binary.
test-e2e tier="affected": (nx-tier tier "-t test --projects=tag:type:e2e")

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

# coverage:coverage runs after both crates' instrumented `test` targets and
# merges their profiles; the floor lives in tools/coverage/coverage.sh.
# Enforce 95% line, function, and region coverage over the unit + e2e runs.
test-cov tier="affected": (nx-tier tier "-t coverage")

# Build the API docs (warnings are errors).
doc tier="affected": (nx-tier tier "-t doc")

# Supply chain: cargo-deny (advisories, bans, licenses, sources) + cargo-machete.
supply-chain tier="affected": (nx-tier tier "-t supply-chain")

# Catches a schema that drifts too strict for the configs the loader accepts
# (the config-schema project's `test`; uv resolves `jsonschema`).
# Validate every shipped config against the published JSON Schema.
schema-check:
    @bash scripts/nx run config-schema:test

# The version is Cargo.toml's rust-version (scripts/msrv.sh); install that
# toolchain first (`rustup toolchain install <version>`).
# Check the workspace against its declared minimum supported Rust version.
msrv:
    @bash scripts/nx run workspace:msrv

# Install git hooks.
hooks-install:
    lefthook install

# Run the pre-commit hook set against the working tree.
hooks:
    lefthook run pre-commit --all-files

# Debug build.
build:
    @bash scripts/nx run allowlister:build

# Optimized release build.
build-release:
    cargo build --release --locked

# Release build + release plan: the allowlister:release-check target the gate runs.
release-check:
    @bash scripts/nx run allowlister:release-check

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
    @bash scripts/nx run allowlister:bench

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

# This is THE gate: format check, type-check, lint (clippy, the workflow-matrix
# drift gate, project boundaries), every test target (unit + integration, binary
# e2e, schema validation, workflow contracts), enforced coverage, docs, the
# release build + plan, and the supply chain — over the projects this change can
# reach, or every project with `just check all` (the release-PR sweep).
# `bootstrap` then `check` is what CI runs; nothing here is warnings-only, and
# any failing target fails the recipe.
# `skip` names gate targets (space-separated) to leave out, only for a CI job
# whose sibling context runs that target itself: the `test (<os>)` jobs pass
# `supply-chain`, which `deps & security` runs once. A name outside the gate's
# target list aborts rather than quietly skipping nothing.
# Full quality gate (`just check all` sweeps every project).
check tier="affected" skip="":
    #!/usr/bin/env bash
    set -euo pipefail
    targets=(format-check lint typecheck test build doc release-check coverage supply-chain)
    # `-d ''` reads every line of `skip`, so a newline cannot hide a name from the check below.
    read -r -d '' -a skips <<< {{ quote(skip) }} || true
    # The `${a[@]+...}` form keeps an empty array legal under `set -u` on bash 3.2 (macOS).
    for s in ${skips[@]+"${skips[@]}"}; do
        [[ " ${targets[*]} " == *" $s "* ]] || { printf "unknown gate target '%s' to skip — choose from: %s\n" "$s" "${targets[*]}" >&2; exit 2; }
        kept=()
        for t in "${targets[@]}"; do [ "$t" = "$s" ] || kept+=("$t"); done
        targets=(${kept[@]+"${kept[@]}"})
    done
    case {{ quote(tier) }} in
        # llmlint: ignore-block[diagnostics_error_or_absent] the build, test and release-check compilations are of the same sources `lint` checks with clippy -D warnings over --all-targets --all-features, so any rustc warning already fails this recipe through that target; denying again per invocation would rebuild the graph per RUSTFLAGS set.
        affected) base="$(bash scripts/nx-base.sh)"; exec bash scripts/nx affected --base="$base" --exclude=tag:type:live -t "${targets[@]}" ;;
        all) exec bash scripts/nx run-many --exclude=tag:type:live -t "${targets[@]}" ;;
        # llmlint: ignore-end[diagnostics_error_or_absent]
        *) printf "unknown tier '%s' — use 'affected' (the default) or 'all'\n" {{ quote(tier) }} >&2; exit 2 ;;
    esac

# Alias for `check` (kept so existing docs/scripts that say `full-check` work).
full-check tier="affected": (check tier)

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

# An upgrade can reach any project, so the affected set would understate it.
# Review `git diff Cargo.lock` before committing. The Nx toolchain is pinned
# exactly in package.json and bumped deliberately, not here.
# Update Cargo.lock (`cargo update`), then re-run the gate as the full sweep.
upgrade:
    cargo update
    @just check all

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

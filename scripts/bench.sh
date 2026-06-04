#!/usr/bin/env bash
#
# End-to-end CLI latency benchmark. Drives the optimized release binary the way
# an agent harness does — one process per command — and measures wall-clock time
# with hyperfine across every verb. This captures the cost that matters in
# production: process startup + config discovery (fs) + bash-AST parse + rule
# matching, which the in-process Criterion benches (`benches/engine.rs`)
# deliberately exclude.
#
# Usage:
#   scripts/bench.sh            Full run (warmup + adaptive sampling).
#   scripts/bench.sh --dry-run  One run, no warmup — a fast smoke check that the
#                               harness and every command still work (used by CI
#                               and `just`), without depending on stable numbers.
#
# Results: human table on stdout plus machine-readable exports under
# ${BENCH_OUT:-target/bench} (results.json, results.md).
#
# Environment overrides:
#   BENCH_OUT       output directory (default: <repo>/target/bench)
#   BENCH_WARMUP    warmup runs before timing (default: 10)
#   BENCH_KEEP      set to 1 to keep the temp sandbox for inspection
#
# Every benchmarked command runs through a shell (hyperfine's default) because
# the hook case redirects stdin and `init` needs a working directory; the small,
# constant shell-spawn cost is therefore included uniformly in every row.

set -euo pipefail

mode="${1:-run}"
case "$mode" in
    run | --dry-run) ;;
    *)
        echo "usage: bench.sh [--dry-run]" >&2
        exit 2
        ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/release/allowlister"
out="${BENCH_OUT:-$repo_root/target/bench}"
warmup="${BENCH_WARMUP:-10}"

note() { printf '%s\n' "$*"; }
fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

if ! command -v hyperfine >/dev/null 2>&1; then
    fail "hyperfine not found on PATH. Install dev tools with 'just bootstrap' (or 'cargo binstall hyperfine')."
fi

# A `--dry-run` proves the harness and commands work without spending time on
# statistics; the full run warms up and lets hyperfine sample adaptively.
runs_opt=()
if [[ "$mode" == "--dry-run" ]]; then
    warmup=0
    runs_opt=(--runs 1)
fi

note "» building release binary"
(cd "$repo_root" && cargo build --release --locked --quiet)
[ -x "$bin" ] || fail "release binary not found at $bin"

# Hermetic config sandbox, mirroring tests/e2e: a user config under
# XDG_CONFIG_HOME and a project config in a `.git`-rooted directory, both from
# the canonical examples/ fixtures so the benchmarked verdicts are reproducible
# and the host machine's own config never leaks in.
sandbox="$(mktemp -d)"
cleanup() { [ "${BENCH_KEEP:-0}" = "1" ] || rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
initdir="$sandbox/initdir"
installdir="$sandbox/installdir"
payload="$sandbox/payload.json"
mkdir -p "$proj/.git" "$initdir" "$installdir" "$sandbox/xdg/allowlister"
cp "$repo_root/examples/user-config.json" "$sandbox/xdg/allowlister/config.json"
cp "$repo_root/examples/project-config.json" "$proj/.allowlister.json"

# A PreToolUse payload whose cwd points at the sandbox project (shape matches
# tests/e2e/main.rs). The sandbox path comes from mktemp, so it needs no JSON
# escaping.
printf '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"gh pr list | head -20"},"cwd":"%s"}\n' \
    "$proj" >"$payload"

# Hermetic environment for every spawned process.
export XDG_CONFIG_HOME="$sandbox/xdg"
export HOME="$sandbox"

mkdir -p "$out"

note "» benchmarking $bin"
# One invocation so a single export holds every command. `--prepare` clears the
# write targets before each run — `init` refuses to overwrite, and `install`
# should measure the create-from-empty path each time, not an idempotent re-run;
# removing both is a harmless no-op for the read-only commands. The deny case
# exits 2 by design, so it is wrapped with `|| true` to keep hyperfine from
# treating it as a failure.
hyperfine \
    --warmup "$warmup" "${runs_opt[@]}" \
    --prepare "rm -f '$initdir/.allowlister.json' '$installdir/config.json'" \
    --export-json "$out/results.json" \
    --export-markdown "$out/results.md" \
    -n "version" "'$bin' --version" \
    -n "help" "'$bin' --help" \
    -n "check:allow" "'$bin' check 'ls -la' --cwd '$proj'" \
    -n "check:pipeline" "'$bin' check 'gh pr list | head -20 | wc -l' --cwd '$proj'" \
    -n "check:deny" "'$bin' check 'rm -rf /' --cwd '$proj' || true" \
    -n "check:json" "'$bin' check 'gh pr list' --json --cwd '$proj'" \
    -n "explain" "'$bin' explain 'gh pr list | head -20 | wc -l' --cwd '$proj'" \
    -n "hook:allow" "'$bin' hook claude-code < '$payload'" \
    -n "init:local" "cd '$initdir' && '$bin' init --local > /dev/null" \
    -n "install:profile" "'$bin' install read-only --output '$installdir/config.json' > /dev/null"

note ""
note "✓ wrote $out/results.json"
note "       $out/results.md"

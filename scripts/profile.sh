#!/usr/bin/env bash
#
# Sampling profiler for finding engine bottlenecks, built on samply (records a
# trace you open in the Firefox Profiler UI).
#
# Both modes build the dedicated `profiling` profile (Cargo.toml): the shipped
# release optimizations, but with symbols kept so samply can attribute time to
# functions. The real `[profile.release]` artifact stays stripped.
#
# Usage:
#   scripts/profile.sh                       Profile the whole engine hot path.
#   scripts/profile.sh engine [FILTER]       Profile one or more Criterion
#                                            benchmarks (e.g. evaluate/pipeline).
#   scripts/profile.sh check 'rm -rf /'      Profile a real CLI invocation
#   scripts/profile.sh explain 'a | b'       (startup + config + parse + match),
#   scripts/profile.sh config show           looped so the sub-millisecond
#                                            process yields enough samples.
#
# A single CLI run is far too short to sample, which is why the engine mode
# (Criterion's `--profile-time`, a long-running in-process loop) is the right
# tool for optimizing the engine, and the CLI mode loops the binary.
#
# Environment overrides:
#   PROFILE_SECONDS   engine mode: seconds to sample (default: 10)
#   PROFILE_REPEAT    cli mode: invocations to loop under the profiler (default: 3000)
#   SAMPLY_ARGS       extra args passed to `samply record` (e.g. --save-only)

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
seconds="${PROFILE_SECONDS:-10}"
repeat="${PROFILE_REPEAT:-3000}"
# shellcheck disable=SC2206  # intentional word-splitting of optional flags.
samply_args=(${SAMPLY_ARGS:-})

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

command -v samply >/dev/null 2>&1 ||
    fail "samply not found on PATH. Install dev tools with 'just bootstrap' (or 'cargo install --locked samply')."

mode="${1:-engine}"

if [[ "$mode" == "engine" ]]; then
    shift || true
    filter="${1:-}"
    echo "» building bench (profiling profile)"
    # Build the bench with symbols, then read its executable path from cargo's
    # JSON output (no jq dependency, matching scripts/dist.sh).
    artifact="$(cargo build --profile profiling --bench engine --locked --message-format=json -q |
        grep -F '"name":"engine"' | grep -F '"executable":' | tail -1)"
    bench_exe="$(printf '%s' "$artifact" | grep -o '"executable":"[^"]*"' | cut -d'"' -f4)"
    [ -n "$bench_exe" ] && [ -x "$bench_exe" ] || fail "could not locate the profiling bench executable"
    echo "» profiling engine for ${seconds}s (${filter:-all benchmarks})"
    # `--profile-time` makes Criterion run the bench in a plain loop with no
    # statistical analysis — exactly what an external sampler wants.
    samply record "${samply_args[@]}" -- \
        "$bench_exe" --bench --profile-time "$seconds" ${filter:+"$filter"}
    exit 0
fi

# CLI mode: profile a real invocation, looped so a sub-millisecond process is
# sampled enough times to be meaningful (covers startup + config + parse).
bin="$repo_root/target/profiling/allowlister"
echo "» building binary (profiling profile)"
(cd "$repo_root" && cargo build --profile profiling --locked --quiet)
[ -x "$bin" ] || fail "profiling binary not found at $bin"

echo "» profiling '$bin $*' over $repeat invocations"
samply record "${samply_args[@]}" -- \
    bash -c 'n="$1"; shift; for ((i = 0; i < n; i++)); do "$@" >/dev/null 2>&1 || true; done' \
    _ "$repeat" "$bin" "$@"

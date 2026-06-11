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
#   scripts/profile.sh callgrind check 'a'   Deterministic per-function
#                                            attribution of one CLI invocation
#                                            (valgrind callgrind; Linux-only).
#
# A single CLI run is far too short to sample, which is why the engine mode
# (Criterion's `--profile-time`, a long-running in-process loop) is the right
# tool for optimizing the engine, and the CLI mode loops the binary.
#
# samply needs perf-event access the kernel often withholds in containers and
# CI. The callgrind mode is the fallback that works anywhere valgrind does: it
# runs ONE invocation (no looping — counts are exact, not sampled), writes the
# raw callgrind output under target/profile/, and prints the top functions by
# instruction count. It attributes the same totals `just bench-instructions`
# reports, so a regression found there can be dug into here.
#
# Environment overrides:
#   PROFILE_SECONDS   engine mode: seconds to sample (default: 10)
#   PROFILE_REPEAT    cli mode: invocations to loop under the profiler (default: 3000)
#   PROFILE_TOP       callgrind mode: function rows to print (default: 30)
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

mode="${1:-engine}"

# Deterministic per-function attribution of a single CLI invocation. No samply
# (and no perf-event access) needed, so it works in containers and CI.
if [[ "$mode" == "callgrind" ]]; then
    shift
    [[ $# -ge 1 ]] || fail "usage: profile.sh callgrind <allowlister args…> (e.g. callgrind check 'ls -la')"
    command -v valgrind >/dev/null 2>&1 ||
        fail "valgrind not found on PATH (Linux-only; install it with your package manager)."
    command -v callgrind_annotate >/dev/null 2>&1 ||
        fail "callgrind_annotate not found on PATH (ships with valgrind)."
    bin="$repo_root/target/profiling/allowlister"
    echo "» building binary (profiling profile)"
    (cd "$repo_root" && cargo build --profile profiling --locked --quiet)
    [ -x "$bin" ] || fail "profiling binary not found at $bin"
    outdir="$repo_root/target/profile"
    mkdir -p "$outdir"
    out="$outdir/callgrind.out"
    echo "» running '$bin $*' under callgrind"
    # Non-zero exits are tolerated: a deny exits 2 by design and the profile of
    # that path is exactly what was asked for.
    valgrind --tool=callgrind --callgrind-out-file="$out" -- "$bin" "$@" >/dev/null || true
    echo
    echo "» top ${PROFILE_TOP:-30} functions by instruction count (full data: $out)"
    callgrind_annotate --threshold=99 "$out" | head -n "$((${PROFILE_TOP:-30} + 12))"
    exit 0
fi

command -v samply >/dev/null 2>&1 ||
    fail "samply not found on PATH. Install dev tools with 'just bootstrap' (or 'cargo install --locked samply')."

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

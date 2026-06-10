#!/usr/bin/env bash
#
# Deterministic end-to-end CLI cost via instruction counts (valgrind's
# cachegrind, no cache simulation). Wall-clock timings (scripts/bench.sh) are
# noisy on shared hardware, so a small regression hides inside the jitter;
# instruction counts are reproducible to within ~0.1% (ASLR and environment
# size leave a little), which makes a base-vs-PR delta trustworthy where a
# hyperfine delta is not. Linux-only: it needs valgrind on PATH.
#
# Counts come from the `profiling` Cargo profile — codegen-matched to the
# shipped release profile, with symbols kept so a regression can be dug into
# with callgrind/cachegrind annotation tools afterwards.
#
# Usage:
#   scripts/bench-instructions.sh                   Run the suite.
#   scripts/bench-instructions.sh report BASE HEAD  Print a markdown delta table
#                                                   from two instructions.tsv files.
#
# Results: markdown table on stdout plus machine-readable exports under
# ${BENCH_OUT:-target/bench} (instructions.tsv, instructions.md).
#
# Environment overrides:
#   BENCH_OUT   output directory (default: <repo>/target/bench)

set -euo pipefail

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

# `report` joins a base and a head TSV (case<TAB>instructions) into a markdown
# delta table; it needs no valgrind, so CI can run it after checking back out
# of the base revision.
if [[ "${1:-}" == "report" ]]; then
    [[ $# -eq 3 && -s "$2" && -s "$3" ]] ||
        fail "usage: bench-instructions.sh report BASE.tsv HEAD.tsv (both non-empty)"
    awk -F'\t' '
        NR == FNR { base[$1] = $2; next }
        FNR == 1 {
            print "| command | base | head | Δ instructions |"
            print "|---|---:|---:|---:|"
        }
        {
            if ($1 in base && base[$1] > 0) {
                delta = ($2 - base[$1]) / base[$1] * 100
                printf "| %s | %s | %s | %+.2f%% |\n", $1, base[$1], $2, delta
            } else {
                printf "| %s | — | %s | new |\n", $1, $2
            }
        }
    ' "$2" "$3"
    exit 0
fi

[[ "${1:-}" == "" ]] || fail "usage: bench-instructions.sh [report BASE.tsv HEAD.tsv]"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo_root/target/profiling/allowlister"
out="${BENCH_OUT:-$repo_root/target/bench}"

note() { printf '%s\n' "$*"; }

command -v valgrind >/dev/null 2>&1 ||
    fail "valgrind not found on PATH (Linux-only; install it with your package manager, e.g. 'apt-get install valgrind')."

note "» building binary (profiling profile)"
(cd "$repo_root" && cargo build --profile profiling --locked --quiet)
[ -x "$bin" ] || fail "profiling binary not found at $bin"

# Hermetic config sandbox, mirroring scripts/bench.sh: user + project config
# from the canonical examples/ fixtures so counts are reproducible and the host
# machine's own config never leaks in.
sandbox="$(mktemp -d)"
cleanup() { rm -rf "$sandbox"; }
trap cleanup EXIT

proj="$sandbox/project"
payload="$sandbox/payload.json"
mkdir -p "$proj/.git" "$sandbox/xdg/allowlister"
cp "$repo_root/examples/user-config.json" "$sandbox/xdg/allowlister/config.json"
cp "$repo_root/examples/project-config.json" "$proj/.allowlister.json"
printf '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"gh pr list | head -20"},"cwd":"%s"}\n' \
    "$proj" >"$payload"

export XDG_CONFIG_HOME="$sandbox/xdg"
export HOME="$sandbox"

# A small, fixed history store so the `history` rows read non-trivial but
# byte-identical input on every run.
note "» seeding usage history"
for cmd in 'gh pr list | head -20' 'git status' 'npm run build' 'cargo test' 'some_unknown_tool'; do
    printf '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"%s"},"cwd":"%s"}\n' \
        "$cmd" "$proj" | ALLOWLISTER_HISTORY=1 "$bin" hook claude-code >/dev/null
done

mkdir -p "$out"
tsv="$out/instructions.tsv"
md="$out/instructions.md"
: >"$tsv"

# Run one case under cachegrind and append its instruction count to the TSV.
# The first argument names the case; the rest is the command. Callers wire any
# stdin redirect. Non-zero exits (the deny case exits 2 by design) still
# produce a count, so they are tolerated.
measure() {
    local name="$1"
    shift
    local log="$sandbox/cachegrind.log"
    set +e
    valgrind --tool=cachegrind --cache-sim=no \
        --cachegrind-out-file="$sandbox/cachegrind.out" \
        --log-file="$log" -- "$@" >/dev/null
    set -e
    local refs
    refs="$(awk '/I +refs:/ { gsub(",", "", $4); print $4; exit }' "$log")"
    [ -n "$refs" ] || fail "no instruction count for '$name' (see $log)"
    printf '%s\t%s\n' "$name" "$refs" >>"$tsv"
    note "  $name: $refs instructions"
}

note "» counting instructions ($bin)"
measure "version" "$bin" --version
measure "check:allow" "$bin" check 'ls -la' --cwd "$proj"
measure "check:pipeline" "$bin" check 'gh pr list | head -20 | wc -l' --cwd "$proj"
measure "check:deny" "$bin" check 'rm -rf /' --cwd "$proj"
measure "explain" "$bin" explain 'gh pr list | head -20 | wc -l' --cwd "$proj"
measure "hook:allow" "$bin" hook claude-code <"$payload"
ALLOWLISTER_HISTORY=1 measure "hook:record" "$bin" hook claude-code <"$payload"
measure "history" "$bin" history
measure "history:json" "$bin" history --json

{
    echo "| command | instructions |"
    echo "|---|---:|"
    awk -F'\t' '{ printf "| %s | %s |\n", $1, $2 }' "$tsv"
} >"$md"

note ""
note "✓ wrote $tsv"
note "       $md"

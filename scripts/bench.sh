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
#   BENCH_SEED      distinct synthetic commands seeded into the usage history
#                   (default: 500; 5 under --dry-run)
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
# statistics; the full run warms up and lets hyperfine sample adaptively. The
# history seed shrinks with it so the smoke check stays fast.
runs_opt=()
seed_default=500
if [[ "$mode" == "--dry-run" ]]; then
    warmup=0
    runs_opt=(--runs 1)
    seed_default=5
fi
seed="${BENCH_SEED:-$seed_default}"

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
cfgdir="$sandbox/cfgdir"
payload="$sandbox/payload.json"
mkdir -p "$proj/.git" "$initdir" "$installdir" "$cfgdir" "$sandbox/xdg/allowlister"
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

# Seed a usage-history store so the `history` report and the recording hook
# benchmark a grown store, not the empty fast path. Report cost scales with the
# number of *distinct* commands in the store, so beyond a handful of realistic
# mixed-verdict commands, seed BENCH_SEED distinct synthetic ones (each via the
# real hook so the on-disk format can never drift from the binary). Recording is
# opt-in, so force it on for the seed only (the timed `history` rows read with
# it off). This writes under the hermetic XDG dir, never the host.
note "» seeding usage history (5 + $seed distinct commands)"
for cmd in 'gh pr list | head -20' 'git status' 'npm run build' 'cargo test' 'some_unknown_tool'; do
    printf '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"%s"},"cwd":"%s"}\n' \
        "$cmd" "$proj" | ALLOWLISTER_HISTORY=1 "$bin" hook claude-code >/dev/null
done
for ((i = 0; i < seed; i++)); do
    printf '{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"tool%d build --input file%d.txt"},"cwd":"%s"}\n' \
        "$i" "$i" "$proj" | ALLOWLISTER_HISTORY=1 "$bin" hook claude-code >/dev/null
done

# Snapshot the seeded store and restore it before every timed run (hyperfine's
# --prepare): the hook:record row appends on each run, and without the reset the
# later history rows would read a store that grew during the benchmark itself.
hist="$sandbox/xdg/allowlister/history"
hist_seed="$sandbox/history-seed"
[ -d "$hist" ] || fail "history seeding produced no store at $hist"
cp -a "$hist" "$hist_seed"

mkdir -p "$out"

note "» benchmarking $bin"
# One invocation so a single export holds every command. `--prepare` clears the
# write targets before each run — `init` refuses to overwrite, and `install`
# should measure the create-from-empty path each time, not an idempotent re-run —
# resets the seeded `config` files (so `config add` measures a real merge and
# `config remove` always has its target rule), and resets the history store to
# the seeded snapshot; all are harmless no-ops for the read-only commands. The
# deny case exits 2 by design, so it is wrapped with `|| true` to keep hyperfine
# from treating it as a failure.
hyperfine \
    --warmup "$warmup" "${runs_opt[@]}" \
    --prepare "rm -f '$initdir/.allowlister.json' '$initdir/.allowlister.jsonc' '$installdir/config.json' && cp '$repo_root/examples/user-config.json' '$cfgdir/config.json' && rm -rf '$hist' && cp -a '$hist_seed' '$hist'" \
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
    -n "hook:record" "ALLOWLISTER_HISTORY=1 '$bin' hook claude-code < '$payload'" \
    -n "history" "'$bin' history > /dev/null" \
    -n "history:json" "'$bin' history --json > /dev/null" \
    -n "config:show" "'$bin' config show --cwd '$proj' > /dev/null" \
    -n "config:show:json" "'$bin' config show --json --cwd '$proj' > /dev/null" \
    -n "config:add" "'$bin' config add --name bench --match 'benchtool *' --output '$cfgdir/config.json' > /dev/null" \
    -n "config:remove" "'$bin' config remove 'rm -rf — never' --output '$cfgdir/config.json' > /dev/null" \
    -n "init:local" "cd '$initdir' && '$bin' init --local > /dev/null" \
    -n "install:profile" "'$bin' install read-only --output '$installdir/config.json' > /dev/null"

note ""
note "✓ wrote $out/results.json"
note "       $out/results.md"

#!/usr/bin/env bash
# Claude Code SessionStart hook: a fast, NON-BLOCKING dev-environment check.
#
# It must never run the install itself. Provisioning takes minutes (a full rustup
# toolchain download plus tool installs), and doing that synchronously inside a
# SessionStart hook freezes the session until it finishes — the session waits on
# the hook. So this only runs the lightweight check and, when the environment is
# not ready, prints guidance for the agent to run `just setup` as a visible,
# interruptible first step. Stdout is injected as session context, so a ready
# environment stays silent.
#
# Set ALLOWLISTER_AUTO_SETUP=1 to opt into hands-off provisioning: setup is then
# launched detached in the background (still non-blocking) instead of advised.
set -eu

# Skip only in this repo's own GitHub Actions CI (the live-harness e2e job spins
# up a real session). Escape hatch for any other automated context:
# ALLOWLISTER_SKIP_SETUP.
[ -n "${GITHUB_ACTIONS:-}" ] && exit 0
[ -n "${ALLOWLISTER_SKIP_SETUP:-}" ] && exit 0

ROOT="${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT"
# shellcheck source=scripts/setup-lib.sh
. scripts/setup-lib.sh
_load_tool_env

# Ready -> stay silent and cheap.
_check_ready && exit 0

# Opt-in: provision hands-off, but DETACHED so the session is never blocked.
# A flock keeps two concurrent sessions from launching setup twice; the lock is
# held by the background job for its whole run, not by this returning hook.
if [ -n "${ALLOWLISTER_AUTO_SETUP:-}" ]; then
  mkdir -p .dev
  launcher="nohup"
  command -v setsid >/dev/null 2>&1 && launcher="setsid"
  "$launcher" bash -c 'exec 9>.dev/setup.lock; flock -n 9 || exit 0; exec bash scripts/setup.sh' \
    >.dev/setup.log 2>&1 </dev/null &
  printf '%s\n' \
    "[allowlister] Dev environment not ready (${REASON}); provisioning in the BACKGROUND" \
    "(log: .dev/setup.log). It does not block this session. Tools appear within a few minutes:" \
    "check with 'just setup-check', then run 'direnv reload' (or open a new shell)."
  exit 0
fi

# Default: advise. Do NOT block the session on a multi-minute install.
printf '%s\n' \
  "[allowlister] Dev environment not set up yet (${REASON})." \
  "ACTION: run 'just setup' (or './scripts/setup.sh' if just is missing) as your FIRST step," \
  "before building or testing. It installs asdf + direnv, the pinned Rust toolchain, and the" \
  "cargo dev tools (several minutes on a fresh machine), then allows the .envrc." \
  "Verify anytime with 'just setup-check'. After it completes, run 'direnv reload' (or open a" \
  "new shell) so asdf and direnv are on PATH."
exit 0

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
#
# Every exit path hands off to scripts/setup-llmlint.sh (the optional llmlint
# tier) through the EXIT trap below. That installer reaches PyPI, so it runs
# detached with its output in .dev/setup-llmlint.log: the hook returns at once
# and exits 0 whether the install succeeds, fails, or finds uv missing.
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

handoff_llmlint() {
  local setup="$SCRIPT_DIR/setup-llmlint.sh" dev="$SCRIPT_DIR/../.dev"
  local log="$dev/setup-llmlint.log" err
  [ -f "$setup" ] || return 0
  # Open the log here, not in the detached job: a failure is then reported, with
  # its cause and the next step, instead of dying unseen in the background.
  if ! err="$( { mkdir -p "$dev" && : >"$log"; } 2>&1)"; then
    printf 'session-setup: llmlint not provisioned: cannot write %s (%s); fix that, then run just setup-llmlint\n' "$log" "$err" >&2
    return 0
  fi
  # Detach from the hook's stdout/stderr (the session waits for them to close)
  # and from its process group, so neither the wait nor a hook timeout reaches
  # the install.
  # llmlint: ignore-block[work_goes_through_command_surface] this hook runs before `just` is guaranteed to exist (it installs just itself a few lines below), so it launches the script `just setup-llmlint` wraps directly, as the create-repo session-setup template does.
  if command -v setsid >/dev/null 2>&1; then
    setsid bash "$setup" >>"$log" 2>&1 </dev/null &
  else
    nohup bash "$setup" >>"$log" 2>&1 </dev/null &
  fi
  # llmlint: ignore-end[work_goes_through_command_surface]
  return 0
}
trap 'handoff_llmlint || true' EXIT

# Cloud sessions may have uv but not just; ensure the command-surface entry point
# exists before printing any advice that names `just setup`.
if ! command -v just >/dev/null 2>&1 && command -v uv >/dev/null 2>&1; then
  uv tool install rust-just >/dev/null 2>&1 || true
  export PATH="$HOME/.local/bin:$PATH"
fi

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

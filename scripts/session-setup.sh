#!/usr/bin/env bash
# Claude Code SessionStart hook: keep the dev environment provisioned.
#
# Fast path (the common case): the lightweight check passes and this exits
# silently. Anything printed to stdout is injected into the session as context,
# so a ready environment says nothing. When setup is needed it runs once and
# reports the outcome; a previous failure switches to advise-only so a broken
# machine never re-runs a multi-minute install every session.
set -eu

# Skip only in this repo's own GitHub Actions CI (the live-harness e2e job spins
# up a real session). We key on GITHUB_ACTIONS, not the broad CI flag, because
# headless cloud agents often set CI=true yet are exactly who should provision.
# Escape hatch for any other automated context: ALLOWLISTER_SKIP_SETUP.
[ -n "${GITHUB_ACTIONS:-}" ] && exit 0
[ -n "${ALLOWLISTER_SKIP_SETUP:-}" ] && exit 0

ROOT="${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT"
# shellcheck source=scripts/setup-lib.sh
. scripts/setup-lib.sh
_load_tool_env

# Already set up → stay silent and cheap.
_check_ready && exit 0

mkdir -p .dev
LOG=".dev/setup.log"

# A prior setup failed: don't re-attempt the long install automatically; advise.
if [ -e .dev/setup.failed ]; then
  echo "[allowlister] dev environment not ready (${REASON}); a previous setup failed."
  echo "Run \`just setup\` and check .dev/setup.log; that clears this state on success."
  exit 0
fi

# Single-flight so two concurrent sessions don't both launch setup.
if command -v flock >/dev/null 2>&1; then
  exec 9>".dev/setup.lock"
  if ! flock -n 9; then
    echo "[allowlister] dev environment setup is already running (see .dev/setup.log)."
    exit 0
  fi
else
  if ! mkdir .dev/setup.lock.d 2>/dev/null; then
    echo "[allowlister] dev environment setup is already running (see .dev/setup.log)."
    exit 0
  fi
  trap 'rmdir .dev/setup.lock.d 2>/dev/null || true' EXIT INT TERM
fi

echo "[allowlister] dev environment not ready (${REASON}); running \`just setup\` now."
echo "First-time provisioning installs asdf/direnv/Rust tools and can take several minutes."
if bash scripts/setup.sh >"$LOG" 2>&1; then
  echo "[allowlister] ✓ setup complete (full log: .dev/setup.log)."
  echo "Open a new shell or run \`direnv reload\` so the asdf/direnv PATH changes take effect."
else
  : > .dev/setup.failed
  echo "[allowlister] ✗ setup failed. Last lines of .dev/setup.log:"
  tail -n 20 "$LOG" 2>/dev/null || true
  echo "Re-run with \`just setup\` once the cause is fixed."
fi
exit 0

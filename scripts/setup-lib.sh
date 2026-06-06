# Shared helpers for the local-setup scripts (setup.sh, setup-check.sh) and the
# session hook (session-setup.sh). Sourced, not executed: callers set their own
# `set -eu`. All functions assume the current directory is the repo root.

# Binaries that must resolve for the dev environment to be considered ready.
# asdf/direnv (the version + env layer), just (task runner), the Rust toolchain,
# and the test runner installed by `just bootstrap`.
REQUIRED_BINS="asdf direnv just rustc cargo cargo-nextest"

# Soft requirements: their absence is an advisory, never a "not ready" verdict.
# lefthook is a Go binary with no cargo source fallback, so on a network where
# its prebuilt is unreachable `just bootstrap` warns rather than fails; missing,
# it only disables the local git-hook convenience — building and testing still
# work. Kept consistent with bootstrap so setup-check does not nag forever on a
# restricted network where setup otherwise succeeded.
OPTIONAL_BINS="lefthook"

# Machine-local setup state. Lives outside target/ so `cargo clean` does not
# wipe it (cleaning build artifacts does not un-provision the machine).
STAMP=".dev/setup.stamp"

# Put the installed toolchains on PATH for this process. A non-interactive shell
# (and some hook contexts) does not source the user's rc, so asdf/cargo binaries
# may be installed yet unresolved; this normalises that without requiring a fresh
# login. Idempotent and safe when nothing is installed.
_load_tool_env() {
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  if ! command -v asdf >/dev/null 2>&1 && [ -f "$HOME/.asdf/asdf.sh" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.asdf/asdf.sh"
  fi
  local shims="${ASDF_DATA_DIR:-$HOME/.asdf}/shims"
  if [ -d "$shims" ]; then
    case ":$PATH:" in
      *":$shims:"*) : ;;
      *) PATH="$shims:$PATH"; export PATH ;;
    esac
  fi
}

# SHA-256 of stdin using whatever tool is available; a stable sentinel if none
# is (so the stamp comparison still works, falling back to binary-presence only).
_sha256_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 | awk '{print $NF}'
  else
    printf 'no-sha256-tool\n'
  fi
}

# Fingerprint of the inputs that setup depends on: the pinned Rust toolchain, the
# asdf tool versions, and the developer-tool version pins in the justfile. A
# change to any of these invalidates the stamp so setup re-runs (e.g. after
# `just upgrade`).
_fingerprint() {
  {
    [ -f rust-toolchain.toml ] && cat rust-toolchain.toml
    [ -f .tool-versions ] && cat .tool-versions
    [ -f justfile ] && grep -E '^[a-z][a-z-]*-version :=' justfile || true
  } 2>/dev/null | _sha256_stdin
}

# Echo the subset of $1 (a space-separated list of binary names) that does not
# resolve on PATH, each prefixed with a space; empty when all resolve.
_missing_bins() {
  local b out=""
  for b in $1; do
    command -v "$b" >/dev/null 2>&1 || out="$out $b"
  done
  printf '%s' "$out"
}

# Is the dev environment ready? Returns 0 when every required binary resolves and
# the stamp matches the current fingerprint; otherwise returns 1 and sets REASON.
# Soft requirements (OPTIONAL_BINS) do not affect readiness; surface them with
# _missing_bins where an advisory is wanted.
_check_ready() {
  REASON=""
  local missing
  missing="$(_missing_bins "$REQUIRED_BINS")"
  if [ -n "$missing" ]; then
    REASON="missing tools:$missing"
    return 1
  fi
  local want have_fp
  want="$(_fingerprint)"
  have_fp="$(cat "$STAMP" 2>/dev/null || true)"
  if [ -z "$have_fp" ]; then
    REASON="no setup stamp (first run on this machine)"
    return 1
  fi
  if [ "$want" != "$have_fp" ]; then
    REASON="toolchain or tool versions changed since last setup"
    return 1
  fi
  return 0
}

# Record the current fingerprint as the stamp of a successful setup.
_write_stamp() {
  mkdir -p "$(dirname "$STAMP")"
  _fingerprint > "$STAMP"
}

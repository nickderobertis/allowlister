#!/usr/bin/env bash
# allowlister local setup — make a fresh machine ready to run the quality gate.
#
# Idempotent and safe to re-run. It:
#   1. installs + loads asdf (version manager) if absent,
#   2. installs direnv through asdf and wires the asdf+direnv integration,
#   3. installs the asdf-pinned tools from .tool-versions (just),
#   4. ensures the Rust toolchain — rust-toolchain.toml stays the source of
#      truth; rustup just realises it,
#   5. installs the cargo dev tools + git hooks via `just bootstrap`,
#   6. allows the .envrc and records a setup stamp for the fast session check.
#
# Fresh machine (no `just` yet):  ./scripts/setup.sh
# Once `just` is available:        just setup
set -eu

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
# shellcheck source=scripts/setup-lib.sh
. scripts/setup-lib.sh

say()  { printf '» %s\n' "$*"; }
ok()   { printf '✓ %s\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

# Pinned classic asdf, cloned only when asdf is not already on PATH. A single
# git-clone path keeps the integration identical across macOS and Linux.
ASDF_VERSION="v0.14.1"

ensure_asdf() {
  if have asdf; then
    ok "asdf present ($(asdf --version 2>/dev/null || echo unknown))"
    return
  fi
  # Installed on a previous run but not on PATH in this (non-interactive) shell,
  # which does not source the shell rc: load it instead of re-cloning.
  if [ -f "$HOME/.asdf/asdf.sh" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.asdf/asdf.sh"
    have asdf && { ok "asdf loaded from ~/.asdf"; return; }
  fi
  have git || { printf 'error: git is required to install asdf\n' >&2; exit 1; }
  say "installing asdf ${ASDF_VERSION} into ~/.asdf"
  git clone --quiet --depth 1 --branch "$ASDF_VERSION" \
    https://github.com/asdf-vm/asdf.git "$HOME/.asdf"
  # Wire asdf into common shell rc files (idempotent; guarded by a marker).
  local rc line='. "$HOME/.asdf/asdf.sh"  # asdf (allowlister setup)'
  for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
    [ -e "$rc" ] || continue
    grep -qF 'asdf (allowlister setup)' "$rc" 2>/dev/null \
      || printf '\n%s\n' "$line" >> "$rc"
  done
  [ -e "$HOME/.bashrc" ] || printf '%s\n' "$line" > "$HOME/.bashrc"
  # Load asdf into THIS shell so the rest of setup can use it.
  # shellcheck disable=SC1091
  . "$HOME/.asdf/asdf.sh"
  ok "asdf installed"
}

ensure_plugin() {
  asdf plugin list 2>/dev/null | grep -qx "$1" || asdf plugin add "$1"
}

ensure_direnv() {
  # direnv is installed *through asdf* and pinned globally (it is the loader, so
  # it is not a per-project .tool-versions entry). `asdf direnv setup` wires the
  # shell hook and the `use asdf` function that .envrc relies on.
  ensure_plugin direnv
  if ! asdf list direnv 2>/dev/null | grep -q '[0-9]'; then
    say "installing direnv via asdf"
    asdf install direnv latest
    asdf global direnv latest
  fi
  local sh; sh="$(basename "${SHELL:-bash}")"
  case "$sh" in bash | zsh | fish) : ;; *) sh="bash" ;; esac
  asdf direnv setup --shell "$sh" --version latest >/dev/null 2>&1 || true
  ok "direnv ready (${sh} integration)"
}

ensure_rust() {
  if ! have rustup; then
    say "installing rustup (minimal); rust-toolchain.toml drives the toolchain"
    have curl || { printf 'error: curl is required to install rustup\n' >&2; exit 1; }
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --no-modify-path
  fi
  # shellcheck disable=SC1091
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  say "resolving the pinned toolchain (rustup show)"
  rustup show >/dev/null
  ok "rust toolchain ready ($(rustc --version 2>/dev/null || echo unknown))"
}

main() {
  ensure_asdf
  ensure_plugin just
  ensure_direnv
  say "installing asdf tools from .tool-versions"
  asdf install
  ensure_rust
  say "installing dev tools + git hooks (just bootstrap)"
  just bootstrap
  say "allowing .envrc (direnv allow)"
  direnv allow . 2>/dev/null || true
  _write_stamp
  rm -f .dev/setup.failed
  ok "setup complete — stamp written to ${STAMP}"
  printf '\nOpen a new shell (or run `direnv reload`) so asdf/direnv take effect.\n'
}

main "$@"

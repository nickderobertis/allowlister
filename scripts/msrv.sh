#!/usr/bin/env bash
# Check the workspace under its declared minimum supported Rust version — the
# workspace project's `msrv` target. The MSRV has one source, `rust-version` in
# Cargo.toml's [workspace.package]; the toolchain is derived from it, and
# clippy.toml's `msrv` (which makes clippy flag too-new APIs) must restate the
# same value or this fails before building anything.
#
# Needs that toolchain installed (`rustup toolchain install <version>`).
# Exit status: 0 the workspace checks under the MSRV; 1 the declarations disagree,
# are missing, or the check fails.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

fail() {
  echo "msrv: $*" >&2
  exit 1
}

# `key = "value"` from one TOML table, without a TOML parser.
table_value() {
  awk -v table="$1" -v key="$2" '
    /^\[/ { in_table = ($0 == "[" table "]") }
    in_table && $0 ~ "^" key "[[:space:]]*=" {
      sub(/^[^=]*=[[:space:]]*/, ""); gsub(/^"|"[[:space:]]*(#.*)?$/, ""); print; exit
    }
  ' "$3"
}

msrv="$(table_value workspace.package rust-version Cargo.toml)"
printf '%s' "$msrv" | grep -Eq '^[0-9]+\.[0-9]+(\.[0-9]+)?$' \
  || fail "Cargo.toml's [workspace.package] must declare rust-version as MAJOR.MINOR[.PATCH] (got '${msrv}'); set it there (e.g. rust-version = \"1.88\"), then re-run 'just msrv'."
clippy_msrv="$(awk -F'"' '/^msrv[[:space:]]*=/ { print $2; exit }' clippy.toml)"
[ "$clippy_msrv" = "$msrv" ] \
  || fail "clippy.toml msrv '${clippy_msrv}' differs from Cargo.toml rust-version '${msrv}'; set both to the same version, then re-run 'just msrv'."

# rust-version "1.88" means the 1.88.0 release.
toolchain="$msrv"
case "$toolchain" in *.*.*) ;; *) toolchain="$toolchain.0" ;; esac
exec cargo "+$toolchain" check --locked --workspace --all-targets --all-features

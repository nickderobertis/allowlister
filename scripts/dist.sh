#!/usr/bin/env bash
#
# Local release helpers used by `just dist-plan` and `just dist-build`.
#
#   dist.sh plan    Verify package metadata and print the release target matrix.
#                   Does not build or publish.
#   dist.sh build   Build an optimized binary for the host target and package it
#                   into a checksummed archive under dist/ (a local smoke test of
#                   the artifact the release workflow produces).
#
# The authoritative cross-platform release is produced by
# .github/workflows/release.yml; this script mirrors a single target locally.

set -euo pipefail

# The platforms the release workflow publishes binaries for.
TARGETS=(
    x86_64-unknown-linux-gnu
    aarch64-unknown-linux-gnu
    x86_64-apple-darwin
    aarch64-apple-darwin
    x86_64-pc-windows-msvc
)

bin_name="allowlister"

# Extract `key = "value"` from the [package] section of Cargo.toml using awk so
# the script needs no JSON tooling (no python/jq).
pkg_field() {
    awk -v key="$1" '
        /^\[/ { in_pkg = ($0 == "[package]") }
        in_pkg && $0 ~ "^"key"[[:space:]]*=" {
            sub(/^[^=]*=[[:space:]]*/, ""); gsub(/^"|"[[:space:]]*$/, ""); print; exit
        }
    ' Cargo.toml
}

plan() {
    # Confirm the manifest itself parses, then read fields directly.
    cargo metadata --no-deps --format-version 1 >/dev/null

    local version license repository description
    version="$(pkg_field version)"
    license="$(pkg_field license)"
    repository="$(pkg_field repository)"
    description="$(pkg_field description)"

    local missing=0
    for field in version license repository description; do
        if [[ -z "${!field}" ]]; then
            echo "error: Cargo.toml is missing required package field: $field" >&2
            missing=1
        fi
    done
    [[ "$missing" -eq 0 ]] || exit 1

    echo "package:    ${bin_name} ${version}"
    echo "license:    ${license}"
    echo "repository: ${repository}"
    echo "release targets:"
    for target in "${TARGETS[@]}"; do
        case "$target" in
            *windows*) ext="zip" ;;
            *) ext="tar.gz" ;;
        esac
        echo "  - ${target}  ->  ${bin_name}-${version}-${target}.${ext}(.sha256)"
    done
}

build() {
    local version host target outdir archive
    version="$(pkg_field version)"
    host="$(rustc -vV | sed -n 's/^host: //p')"
    target="${1:-$host}"

    cargo build --release --locked --target "$target"

    outdir="dist"
    mkdir -p "$outdir"
    local stage
    stage="$(mktemp -d)"
    cp "target/${target}/release/${bin_name}" "$stage/"
    [[ -f README.md ]] && cp README.md "$stage/"
    [[ -f LICENSE ]] && cp LICENSE "$stage/"

    archive="${bin_name}-${version}-${target}.tar.gz"
    tar -czf "${outdir}/${archive}" -C "$stage" .
    rm -rf "$stage"

    ( cd "$outdir" && sha256sum "$archive" > "${archive}.sha256" )
    echo "built ${outdir}/${archive}"
    echo "      ${outdir}/${archive}.sha256"
}

cmd="${1:-plan}"
shift || true
case "$cmd" in
    plan) plan ;;
    build) build "$@" ;;
    *)
        echo "usage: dist.sh {plan|build [target]}" >&2
        exit 2
        ;;
esac

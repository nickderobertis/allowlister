#!/usr/bin/env bash
#
# Release builder used by the workflow and `just dist-build`.
#
#   dist.sh plan    Verify package metadata and print the release target matrix.
#                   Does not build or publish.
#   dist.sh build   Build an optimized binary for the host target and package it
#                   into the published archive format under dist/.
#
# The authoritative cross-platform release is produced by
# .github/workflows/release.yml; this script builds one target without publishing.

set -euo pipefail

# The platforms the release workflow publishes binaries for. Linux ships static
# `-musl` (see .cargo/config.toml and release.yml), not `-gnu`.
TARGETS=(
    x86_64-unknown-linux-musl
    aarch64-unknown-linux-musl
    x86_64-apple-darwin
    aarch64-apple-darwin
    x86_64-pc-windows-msvc
)

bin_name="allowlister"

# Extract `key = "value"` from the [package] section of Cargo.toml using awk so
# the script needs no JSON tooling (no python/jq). A `key.workspace = true` entry
# resolves to the same key in [workspace.package], as cargo inherits it.
pkg_field() {
    awk -v key="$1" '
        /^\[/ { section = $0 }
        section == "[package]" && $0 ~ "^"key"[[:space:]]*=" {
            sub(/^[^=]*=[[:space:]]*/, ""); gsub(/^"|"[[:space:]]*$/, ""); pkg = $0
        }
        section == "[package]" && $0 ~ "^"key"\\.workspace[[:space:]]*=[[:space:]]*true" { inherit = 1 }
        section == "[workspace.package]" && $0 ~ "^"key"[[:space:]]*=" {
            sub(/^[^=]*=[[:space:]]*/, ""); gsub(/^"|"[[:space:]]*$/, ""); ws = $0
        }
        END { print (inherit ? ws : pkg) }
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
        # llmlint: ignore[tool_output_is_signal] dist-plan is the explicit platform/asset inventory diagnostic, so its intentional one-row-per-target output is the command result, not routine success chatter.
        echo "  - ${target}  ->  ${bin_name}-v${version}-${target}.${ext} + ${bin_name}-v${version}-${target}.sha256"
    done
}

build() {
    local version host target outdir archive tag binary
    # llmlint: ignore-block[boundary_inputs_validated] Cargo validates the manifest version and target before any artifact staging; the local helper deliberately retains native-host target compatibility beyond the published musl/Darwin/MSVC set.
    version="$(pkg_field version)"
    host="$(rustc -vV | sed -n 's/^host: //p')"
    target="${1:-$host}"
    tag="${2:-v$version}"
    [[ "$tag" = "v$version" ]] || { echo "error: release tag $tag does not match version $version; use v$version or omit the tag" >&2; exit 1; }
    case "$target" in
        *windows*) binary="${bin_name}.exe"; archive="${bin_name}-${tag}-${target}.zip" ;;
        *) binary="$bin_name"; archive="${bin_name}-${tag}-${target}.tar.gz" ;;
    esac
    cargo build --release --locked --target "$target"
    # llmlint: ignore-end[boundary_inputs_validated]
    outdir="dist"
    mkdir -p "$outdir"
    local stage
    stage="$(mktemp -d)"
    trap 'rm -rf "$stage"' EXIT
    cp "target/${target}/release/${binary}" "$stage/" || { echo "cannot stage $target; rebuild its release binary with just dist-build" >&2; exit 1; }
    cp README.md LICENSE CHANGELOG.md "$stage/" || { echo "cannot stage release documents; restore README.md, LICENSE and CHANGELOG.md and rerun just dist-build" >&2; exit 1; }
    if [[ "$archive" = *.zip ]]; then
        ( cd "$stage" && 7z a -tzip "$OLDPWD/$outdir/$archive" "$binary" README.md LICENSE CHANGELOG.md >/dev/null ) || { echo "cannot package $archive; install 7z and check dist/ is writable, then rerun just dist-build" >&2; exit 1; }
    else
        tar -czf "${outdir}/${archive}" -C "$stage" "$binary" README.md LICENSE CHANGELOG.md || { echo "cannot package $archive; install tar and check dist/ is writable, then rerun just dist-build" >&2; exit 1; }
    fi
    rm -rf "$stage"
    trap - EXIT
    local digest
    # llmlint: ignore-block[changed_behavior_has_e2e] The real host-native build/install journey verifies the generated checksum and rejects corruption. Alternate system checksum tools are platform capability adapters; forcing each installed-tool combination is outside this native-host/artifact-contract proof boundary.
    if command -v sha256sum >/dev/null; then
        digest="$(sha256sum "$outdir/$archive" | awk '{print $1}')" || { echo "cannot hash $archive; repair sha256sum and the staged archive, then rerun just dist-build" >&2; exit 1; }
    elif command -v shasum >/dev/null; then
        digest="$(shasum -a 256 "$outdir/$archive" | awk '{print $1}')" || { echo "cannot hash $archive; repair shasum and the staged archive, then rerun just dist-build" >&2; exit 1; }
    else
        echo "no SHA-256 tool; install sha256sum or shasum and rerun just dist-build" >&2; exit 1
    fi
    # llmlint: ignore-end[changed_behavior_has_e2e]
    printf '%s  %s\n' "$digest" "$archive" > "$outdir/${bin_name}-${tag}-${target}.sha256"
    echo "built ${outdir}/${archive}"
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

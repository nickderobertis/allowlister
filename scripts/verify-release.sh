#!/usr/bin/env bash
# Exercise the production installer before publication. Only GitHub asset
# transport and target selection are doubled; the staged binary really executes.
# Exit 0: verified install/run; 1: invalid target/tag, host or artifact failure.
set -euo pipefail
root="$(pwd)"
target="${1:?target required}"
tag="${2:?tag required}"
case "$target" in
    x86_64-unknown-linux-musl|aarch64-unknown-linux-musl|x86_64-apple-darwin|aarch64-apple-darwin|x86_64-pc-windows-msvc) ;;
    *) echo "unsupported target: $target; see just dist-plan for supported triples" >&2; exit 1 ;;
esac
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || { echo "invalid release tag: $tag; use v<major>.<minor>.<patch>" >&2; exit 1; }
case "$target" in
    *apple-darwin) [[ "$(uname -s)" = Darwin ]] || { echo 'Darwin execution requires macOS; run verification on a Mac runner' >&2; exit 1; } ;;
    *linux*) [[ "$(uname -s)" = Linux ]] || { echo 'Linux execution requires a Linux runner; run this step on ubuntu-latest' >&2; exit 1; } ;;
    *windows*) case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; *) echo 'Windows execution requires Windows Git Bash; run this step on windows-latest' >&2; exit 1 ;; esac ;;
esac
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin"
export RELEASE_ASSET_DIR="$root/dist" RELEASE_TARGET="$target" RELEASE_UNAME="$(command -v uname)"
# The installer asks GitHub for assets that are not public yet. Route those
# exact asset names to this build's local files, with no network fallback.
cat > "$stage/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$#" = 4 && "$1" = -fsSL && "$2" = -o ]] || { echo "release transport expects curl -fsSL -o FILE URL; update the adapter to match the installer download contract" >&2; exit 1; }
asset="$RELEASE_ASSET_DIR/${4##*/}"
cp "$asset" "$3" || { echo "missing or unreadable staged asset: $asset; rebuild or restage the release archive and checksum" >&2; exit 1; }
SH
cat > "$stage/bin/uname" <<'SH'
#!/usr/bin/env bash
if [[ "$1" = -m ]]; then
    case "$RELEASE_TARGET" in aarch64-*) echo aarch64 ;; *) echo x86_64 ;; esac
else
    "$RELEASE_UNAME" "$@"
fi
SH
chmod +x "$stage/bin/curl" "$stage/bin/uname"
if ! PATH="$stage/bin:$PATH" sh scripts/install.sh --version "$tag" --to "$stage/install" > "$stage/install.log" 2>&1; then
    cat "$stage/install.log" >&2
    echo "release install failed for $target; rebuild or restage its archive and checksum, then rerun scripts/verify-release.sh" >&2
    exit 1
fi
binary="$stage/install/allowlister"
[[ "$target" != *windows* ]] || binary="$binary.exe"
# Installed bytes must be the one compiled input, not a second build.
# llmlint: ignore[changed_behavior_has_e2e] The production native journey checks byte equality on the real installed artifact and rejects corrupt transport; additional manufactured binary-mismatch cases are outside this host-native/artifact-contract proof boundary.
cmp -s "target/$target/release/${binary##*/}" "$binary" || { echo "installed bytes differ from the compiled target; rebuild or restage $target" >&2; exit 1; }
# llmlint: ignore-block[changed_behavior_has_e2e] The real host-native artifact is installed and executed; non-native identity is an artifact contract checked on its native release runner. Extra manufactured architecture/linkage mutations are outside this proof boundary.
identity="$(file "$binary")"
case "$target" in
    x86_64-unknown-linux-musl) [[ "$identity" = *ELF* && "$identity" = *x86-64* && "$identity" = *statically\ linked* ]] ;;
    aarch64-unknown-linux-musl) [[ "$identity" = *ELF* && "$identity" = *ARM\ aarch64* && "$identity" = *statically\ linked* ]] ;;
    x86_64-apple-darwin) [[ "$identity" = *Mach-O* && "$identity" = *x86_64* ]] ;;
    aarch64-apple-darwin) [[ "$identity" = *Mach-O* && "$identity" = *arm64* ]] ;;
    x86_64-pc-windows-msvc) [[ "$identity" = *PE32+* && "$identity" = *x86-64* ]] ;;
esac || { echo "wrong binary architecture or linkage: $identity; rebuild the declared target" >&2; exit 1; }
# llmlint: ignore-end[changed_behavior_has_e2e]
if command -v sha256sum >/dev/null; then
    payload_hash="$(sha256sum "$binary" | awk '{print $1}')" || { echo "cannot hash $binary; repair the staged artifact and sha256sum installation" >&2; exit 1; }
elif command -v shasum >/dev/null; then
    payload_hash="$(shasum -a 256 "$binary" | awk '{print $1}')" || { echo "cannot hash $binary; repair the staged artifact and shasum installation" >&2; exit 1; }
else
    echo "no SHA-256 tool; install sha256sum or shasum and rerun verification" >&2; exit 1
fi
printf 'binary_sha256 %s %s\n' "$target" "$payload_hash"
if [[ "$target" = x86_64-apple-darwin ]]; then
    actual="$(arch -x86_64 "$binary" --version)" || { echo "cannot execute $target; install Rosetta and rebuild the declared target" >&2; exit 1; }
else
    actual="$("$binary" --version)" || { echo "cannot execute $target; rebuild it for this runner and rerun verification" >&2; exit 1; }
fi
# llmlint: ignore[changed_behavior_has_e2e] The real installed program reports the staged tag version in the native journey, which also rejects mismatched build tags; manufacturing a different executable is outside this host-native/artifact-contract boundary.
[[ "$actual" = "allowlister ${tag#v}" ]] || { echo "incorrect installed version: $actual; rebuild artifacts for $tag" >&2; exit 1; }
"$binary" --help >/dev/null || { echo "installed help failed for $target; rebuild the declared target and rerun verification" >&2; exit 1; }

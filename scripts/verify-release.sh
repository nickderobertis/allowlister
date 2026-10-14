#!/usr/bin/env bash
# Exercise the production installer before publication. Only GitHub asset
# transport is doubled; host detection and the staged binary really execute.
# Exit 0: verified install/run; 1: invalid target/tag, host or artifact failure.
set -euo pipefail
root="$(pwd)"
[[ "$#" = 2 ]] || { echo "usage: scripts/verify-release.sh TARGET TAG; for example x86_64-unknown-linux-musl v0.1.0" >&2; exit 1; }
target="$1"
tag="$2"
case "$target" in
    x86_64-unknown-linux-musl|aarch64-unknown-linux-musl|aarch64-apple-darwin|x86_64-pc-windows-msvc) ;;
    *) echo "unsupported target: $target; see just dist-plan for supported triples" >&2; exit 1 ;;
esac
numeric='(0|[1-9][0-9]*)'
prerelease='(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)'
tag_pattern="v${numeric}\\.${numeric}\\.${numeric}(-${prerelease}(\\.${prerelease})*)?(\\+[0-9A-Za-z-]+(\\.[0-9A-Za-z-]+)*)?"
[[ "$tag" =~ ^${tag_pattern}$ ]] || { echo "invalid release tag: $tag; use v<major>.<minor>.<patch>" >&2; exit 1; }
case "$target" in
    *apple-darwin) [[ "$(uname -s)" = Darwin ]] || { echo 'Darwin execution requires macOS; run verification on a Mac runner' >&2; exit 1; } ;;
    *linux*) [[ "$(uname -s)" = Linux ]] || { echo 'Linux execution requires a Linux runner; run this step on ubuntu-latest' >&2; exit 1; } ;;
    *windows*) case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; *) echo 'Windows execution requires Windows Git Bash; run this step on windows-latest' >&2; exit 1 ;; esac ;;
esac
stage="$(mktemp -d)" || { echo "cannot create release verification staging; check temporary-directory permissions and free disk space, then rerun verification" >&2; exit 1; }
trap 'rm -rf "$stage"' EXIT
# llmlint: ignore[changed_behavior_has_e2e] The real native journey prepares and uses this staging directory. This diagnostic handles a filesystem permission/resource failure after successful mktemp; manufacturing that race requires filesystem-tool doubles outside the forge-only transport boundary.
mkdir -p "$stage/bin" || { echo "cannot prepare $stage; repair staging permissions and rerun verification" >&2; exit 1; }
export RELEASE_ASSET_DIR="$root/dist"
# The installer asks GitHub for assets that are not public yet. Route those
# exact asset names to this build's local files, with no network fallback.
cat > "$stage/bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$#" = 4 && "$1" = -fsSL && "$2" = -o ]] || { echo "release transport expects curl -fsSL -o FILE URL; update the adapter to match the installer download contract" >&2; exit 1; }
asset="$RELEASE_ASSET_DIR/${4##*/}"
cp "$asset" "$3" || { echo "missing or unreadable staged asset: $asset; rebuild or restage the release archive and checksum" >&2; exit 1; }
SH
# llmlint: ignore[changed_behavior_has_e2e] The native installer executes the actual transport adapter. This recovery reports a staging filesystem capability failure; replacing chmod or racing staging permissions would exceed the forge-only transport proof boundary.
chmod +x "$stage/bin/curl" || { echo "cannot make transport adapter executable; repair staging permissions and rerun verification" >&2; exit 1; }
if ! PATH="$stage/bin:$PATH" sh scripts/install.sh --version "$tag" --to "$stage/install" > "$stage/install.log" 2>&1; then
    cat "$stage/install.log" >&2
    echo "release install failed for $target; rebuild or restage its archive and checksum, then rerun scripts/verify-release.sh" >&2
    exit 1
fi
binary="$stage/install/allowlister"
[[ "$target" != *windows* ]] || binary="$binary.exe"
# Installed bytes must be the one compiled input, not a second build.
cmp -s "target/$target/release/${binary##*/}" "$binary" || { echo "installed bytes differ from the compiled target; rebuild or restage $target" >&2; exit 1; }
# llmlint: ignore-block[changed_behavior_has_e2e] The real host-native artifact is installed and executed; non-native identity is an artifact contract checked on its native release runner. Extra manufactured architecture/linkage mutations are outside this proof boundary.
identity="$(file "$binary")"
case "$target" in
    x86_64-unknown-linux-musl) [[ "$identity" = *ELF* && "$identity" = *x86-64* && "$identity" = *statically\ linked* ]] ;;
    aarch64-unknown-linux-musl) [[ "$identity" = *ELF* && "$identity" = *ARM\ aarch64* && "$identity" = *statically\ linked* ]] ;;
    aarch64-apple-darwin) [[ "$identity" = *Mach-O* && "$identity" = *arm64* ]] ;;
    x86_64-pc-windows-msvc) [[ "$identity" = *PE32+* && "$identity" = *x86-64* ]] ;;
esac || { echo "wrong binary architecture or linkage: $identity; rebuild the declared target" >&2; exit 1; }
# llmlint: ignore-end[changed_behavior_has_e2e]
# llmlint: ignore-block[changed_behavior_has_e2e] The real native journey checks the installed payload hash and rejects corruption. Hash-command failures are runner tool-capability adapters; replacing these tools with failing executables would expand the agreed production journey beyond forge transport doubles.
if command -v sha256sum >/dev/null; then
    payload_hash="$(sha256sum "$binary" | awk '{print $1}')" || { echo "cannot hash $binary; repair the staged artifact and sha256sum installation" >&2; exit 1; }
elif command -v shasum >/dev/null; then
    payload_hash="$(shasum -a 256 "$binary" | awk '{print $1}')" || { echo "cannot hash $binary; repair the staged artifact and shasum installation" >&2; exit 1; }
else
    echo "no SHA-256 tool; install sha256sum or shasum and rerun verification" >&2; exit 1
fi
# llmlint: ignore-end[changed_behavior_has_e2e]
# llmlint: ignore[changed_behavior_has_e2e] The native journey executes the real compiled product and rejects wrong staged versions. This recovery handles a runner execution-capability failure; an alternate fixture binary would be rejected first by compiled-byte equality and cannot prove this staged product.
actual="$("$binary" --version)" || { echo "cannot execute $target; rebuild it for this runner and rerun verification" >&2; exit 1; }
[[ "$actual" = "allowlister ${tag#v}" ]] || { echo "incorrect installed version: $actual; rebuild artifacts for $tag" >&2; exit 1; }
# llmlint: ignore[changed_behavior_has_e2e] The native journey drives real installed help; this recovery handles an unexpected runner/process failure of those same bytes, rather than a replacement fixture executable.
"$binary" --help >/dev/null || { echo "installed help failed for $target; rebuild the declared target and rerun verification" >&2; exit 1; }
printf 'binary_sha256 %s %s\n' "$target" "$payload_hash"

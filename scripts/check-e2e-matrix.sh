#!/usr/bin/env bash
# Drift gate for the live-e2e CI matrix contract.
#
# The per-harness e2e workflows (.github/workflows/e2e-*.yml) encode one shared
# contract in several places GitHub Actions cannot centralize: the
# workflow_dispatch `os` choice list is a literal per workflow, and each job's
# matrix restates the PR-default platform set inside a `fromJSON(...)` expression.
# This script is the single source of that contract and fails if any workflow
# drifts from it -- so the duplication is *checked*, not free-floating (see
# .github/AGENTS.md > "Live e2e in CI").
#
# The contract (change it HERE, then update the workflows to match):
#   - No workflow may trigger on `push` (the release-plz release PR re-runs the
#     suite on pull_request as the pre-release gate, so an on-main run was
#     redundant paid model calls).
#   - Every workflow exposes a workflow_dispatch `os` input for on-demand single
#     harness/platform runs.
#   - claude and codex are the primary harnesses and keep their full PR matrix;
#     every other harness runs Linux-only on PR and widens only on demand.
#   - codex and copilot never offer/use windows (their hooks do not load there);
#     their on-demand `all` is ubuntu+macos.
set -euo pipefail

cd "$(dirname "$0")/.."

FULL='["ubuntu-latest","macos-latest","windows-latest"]'
UM='["ubuntu-latest","macos-latest"]'
LINUX='["ubuntu-latest"]'

# One row per workflow: "<id> <pr-default-json> <all-json> <offers-windows>".
# pr-default is the matrix used on pull_request; all is what `-f os=all`
# expands to; offers-windows is whether windows-latest is a dispatch option.
CONTRACT="
claude   $FULL  $FULL  yes
codex    $UM    $UM    no
copilot  $LINUX $UM    no
crush    $LINUX $FULL  yes
cursor   $LINUX $FULL  yes
goose    $LINUX $FULL  yes
opencode $LINUX $FULL  yes
qwen     $LINUX $FULL  yes
"

fails=0
fail() {
	printf 'e2e-matrix drift: %s\n' "$1" >&2
	fails=$((fails + 1))
}

check_one() {
	local id="$1" prd="$2" all="$3" win="$4"
	local f=".github/workflows/e2e-${id}.yml"
	[ -f "$f" ] || { fail "$f is missing"; return; }

	# The `on:` trigger block ends at `permissions:`; no `push` may appear in it.
	if awk '/^on:/{a=1} /^permissions:/{a=0} a' "$f" | grep -qE '^[[:space:]]*push:'; then
		fail "$f still triggers on push (must be pull_request + workflow_dispatch only)"
	fi
	grep -qE '^[[:space:]]*workflow_dispatch:' "$f" || fail "$f missing workflow_dispatch trigger"
	grep -qE '^[[:space:]]+os:$' "$f" || fail "$f missing the workflow_dispatch 'os' input"

	# The matrix is a single fromJSON expression; match its PR-default and 'all'
	# arms as literals (the GitHub expression is not shell-expanded).
	local line
	# shellcheck disable=SC2016
	line="$(grep -F 'os: ${{ fromJSON(' "$f" || true)"
	[ -n "$line" ] || { fail "$f has no fromJSON matrix expression"; return; }
	printf '%s' "$line" | grep -qF "|| '$prd') }}" ||
		fail "$f PR-default matrix is not $prd"
	printf '%s' "$line" | grep -qF "'all' && '$all'" ||
		fail "$f dispatch 'all' matrix is not $all"

	# windows-latest must be offered as a dispatch option iff the harness runs it.
	if [ "$win" = yes ]; then
		grep -qE '^[[:space:]]+- windows-latest$' "$f" ||
			fail "$f missing windows-latest dispatch option"
	else
		if grep -qE '^[[:space:]]+- windows-latest$' "$f"; then
			fail "$f must not offer windows-latest (its hook does not load on Windows)"
		fi
	fi
}

while read -r id prd all win; do
	[ -n "$id" ] || continue
	check_one "$id" "$prd" "$all" "$win"
done <<<"$CONTRACT"

if [ "$fails" -ne 0 ]; then
	printf '\ncheck-e2e-matrix: %d drift(s) from the contract in scripts/check-e2e-matrix.sh\n' "$fails" >&2
	exit 1
fi
echo "check-e2e-matrix: all e2e workflows match the matrix contract"

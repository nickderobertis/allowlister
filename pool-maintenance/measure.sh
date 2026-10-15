#!/usr/bin/env bash
# Budget-only journey; failures propagate from builds, pruning or verification.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
exec node pool-maintenance/measure.mjs

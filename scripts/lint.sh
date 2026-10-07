#!/usr/bin/env bash
# Syntax-check every shell script in scripts/ with the shell that runs it (POSIX
# `sh -n` for a `#!/bin/sh` script such as install.sh; `bash -n` for the rest,
# including the shebang-less libraries bash scripts source) — the scripts project's
# `lint` target. A script rather than an inline loop because Nx runs inline
# commands through cmd.exe on Windows. Quiet on success; each failure prints the
# shell's own file:line diagnostic.
#
# Exit status: 0 when every script parses; 1 when any does not.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

status=0
for f in *.sh nx; do
  case "$(head -n 1 "$f")" in
    '#!/bin/sh' | '#!/usr/bin/env sh') sh -n "$f" || status=1 ;;
    *) bash -n "$f" || status=1 ;;
  esac
done
exit "$status"

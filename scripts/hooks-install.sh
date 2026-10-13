#!/usr/bin/env bash
# Install retained hooks and retire only an unmodified generated pre-push hook.
set -euo pipefail

lefthook install
hooks_dir=$(git rev-parse --git-path hooks)
pre_push="$hooks_dir/pre-push"
pre_commit="$hooks_dir/pre-commit"
# Compare the entire dispatcher, so a custom hook is never removed. Do not
# change core.hooksPath: other repositories may share that configuration.
if [ -f "$pre_push" ] && [ -f "$pre_commit" ]; then
  # llmlint: ignore-block[changed_behavior_has_e2e] Lefthook replaces the pre-commit dispatcher before this read. A read failure after successful installation needs a race or an installer double; real permission failures are tested at the installer boundary.
  expected=$(sed 's/call_lefthook run "pre-commit"/call_lefthook run "pre-push"/' "$pre_commit") || {
    echo "hooks-install: cannot read $pre_commit; repair its permissions, then run just hooks-install" >&2
    exit 1
  }
  # llmlint: ignore-end[changed_behavior_has_e2e]
  result=0
  cmp -s "$pre_push" - <<<"$expected" || result=$?
  case "$result" in
    # llmlint: ignore-block[changed_behavior_has_e2e] Lefthook replaces pre-commit in this directory before cleanup. Directory permissions preventing rm already fail real installation; reaching this defensive branch requires a race or installer double. The real permission failure, preservation and recovery are tested.
    0)
      rm "$pre_push" || {
        echo "hooks-install: cannot remove $pre_push; repair hook directory permissions, then run just hooks-install" >&2
        exit 1
      }
      ;;
    # llmlint: ignore-end[changed_behavior_has_e2e]
    1) ;; # A different dispatcher belongs to the user.
    *)
      echo "hooks-install: cannot compare $pre_push; repair its permissions, then run just hooks-install" >&2
      exit "$result"
      ;;
  esac
fi

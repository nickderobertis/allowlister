# AGENTS — tools/coverage (coverage)

- The floor (95% lines, functions and regions) and the excluded paths live once,
  in `coverage.sh`. Never lower the floor or widen the exclusions to pass.
- Every crate whose tests exercise `allowlister` runs its `test` target through
  `coverage.sh test <crate>` (profiles only, `--no-report`) and carries the
  `coverage:profiles` tag, which is how this project finds and waits for it — so
  the floor is enforced over the union of all of them, never per crate.
- The test and coverage targets stay uncached: a replayed profile set would not
  match the current build.

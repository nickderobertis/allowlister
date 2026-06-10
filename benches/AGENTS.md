# AGENTS — benches

- Bench the public engine surface (`analyze`, `decide`, `evaluate`, config load)
  so the numbers track what the binary runs, not internals that may be inlined
  away.
- Load rules from the canonical `examples/` fixtures once, outside every timed
  loop; never let fixture parsing or filesystem I/O leak into a measurement.
- Split parse from match: time `decide` over a pre-built `Analysis` so rule-match
  cost is not hidden behind AST parsing.
- Shared fixtures (corpus, example rules, synthetic rule sets) live in
  `support/` — a subdirectory so cargo's bench auto-discovery never treats the
  module as a target — and are pulled in via `#[path]`.
- The example fixtures are the realistic floor; scaling groups use synthetic
  worst-case rule sets where nothing matches. Synthetic sets must compile
  cleanly — assert zero warnings and the exact rule count, so a silently
  skipped rule can never flatten the scaling curve.
- `engine_allocs` reports exact allocator tallies, not time: plain `main`, no
  Criterion, deterministic output for a given commit. Keep it that way — no
  timing, no randomness, no I/O inside a measured closure.
- `cargo check`/`clippy` cover these targets via `--all-targets`; keep them
  warning-clean so they cannot rot. `harness = false` keeps them out of the test
  runner and coverage.

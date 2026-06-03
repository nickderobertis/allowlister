# AGENTS — benches

- Bench the public engine surface (`analyze`, `decide`, `evaluate`, config load)
  so the numbers track what the binary runs, not internals that may be inlined
  away.
- Load rules from the canonical `examples/` fixtures once, outside every timed
  loop; never let fixture parsing or filesystem I/O leak into a measurement.
- Split parse from match: time `decide` over a pre-built `Analysis` so rule-match
  cost is not hidden behind AST parsing.
- `cargo check`/`clippy` cover this target via `--all-targets`; keep it
  warning-clean so it cannot rot. `harness = false` keeps it out of the test
  runner and coverage.

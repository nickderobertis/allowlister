# AGENTS — tools (workspace)

- Checks here span the whole workspace, so they run whenever either crate or
  any project definition changes; keep them cheap, and keep anything that
  reruns the Rust suites (the coverage aggregate) in `coverage/` instead.
- Every project needs a `project.json` with exactly one `type:*` tag, and every
  Cargo member one beside its `Cargo.toml`; a new tag needs a constraint in
  `project-boundaries.json`. Express a real dependency as a Cargo path dependency
  or `implicitDependencies` — the checker reads both, and reconciles the latter
  with `nx graph`.
- Scripts here use Node built-ins only (no npm imports), so they run without the
  Nx install; tests drive them on copies of the real tree.

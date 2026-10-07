# AGENTS — tools (workspace)

- Repo-level checks over the whole workspace and graph: `lint` is the module
  boundary check, `supply-chain` is cargo-deny + cargo-machete, `msrv` builds
  under the declared minimum Rust. `coverage/` is a project of its own.
- Every project needs a `project.json` with exactly one `type:*` tag, and every
  Cargo member one beside its `Cargo.toml`; a new tag needs a constraint in
  `project-boundaries.json`. Express a real dependency as a Cargo path dependency
  or `implicitDependencies` — the checker reads both, and reconciles the latter
  with `nx graph`.
- Scripts here use Node built-ins only (no npm imports), so they run without the
  Nx install; tests drive them on copies of the real tree.

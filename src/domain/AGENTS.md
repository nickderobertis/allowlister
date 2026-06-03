# AGENTS — domain

- Pure: no filesystem, process, env, or stdout. Functions take inputs and return
  values plus diagnostics.
- `analyzer` walks the bash AST into role-tagged fragments; the rule engine only
  ever sees `(argv, role, redirections)`. Do not leak pipeline/subshell structure
  into rules — that boundary is the whole design.
- Patterns compile once into anchored, full-match matchers. extglob negation
  `!(…)` needs look-ahead, so the matcher is backed by `fancy-regex`, not the
  `regex` crate.
- A bad pattern or input is a recoverable error or a defer, never a panic.

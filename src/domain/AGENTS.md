# AGENTS — domain

- Pure: no filesystem, process, env, or stdout. Functions take inputs and return
  values plus diagnostics.
- `analyzer` walks the bash AST into role-tagged fragments; the rule engine only
  ever sees `(argv, role, redirections)`. Do not leak pipeline/subshell structure
  into rules — that boundary is the whole design.
- Patterns compile at most once per process into anchored, full-match matchers.
  extglob negation `!(…)` needs look-ahead, so the matcher is backed by
  `fancy-regex`, not the `regex` crate.
- Regex construction dominates per-spawn cost, so glob matchers defer it behind
  a literal-prefix gate; only constructs whose translation can fail to compile
  (character classes) build eagerly, keeping validation at load time. The gate
  must stay conservative: it may pass values the pattern rejects, never reject
  values the pattern accepts.
- A bad pattern or input is a recoverable error or a defer, never a panic.

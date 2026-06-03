# AGENTS

Rust CLI that gates AI-agent shell commands: parse bash into role-tagged
fragments, match each against rules, compose one verdict. Single binary,
edition 2021, toolchain pinned in `rust-toolchain.toml`.

## Layout

- `src/main.rs` — thin: parse args, dispatch, map the typed result to an exit
  code, print. No behavior here.
- `src/lib.rs` — wiring and the public API; `run()` is the entry point.
- `src/cli.rs` — clap definitions: args, subcommands, defaults in one place.
- `src/domain/` — pure engine (glob, rule, analyzer, decision). No I/O.
- `src/io/` — filesystem config discovery and harness stdin/stdout adapters.
- `src/commands/` — one module per CLI verb; orchestrates domain + io.
- `src/config.rs` — JSON rule schema and user/project merge (a boundary, not
  domain).
- `src/errors.rs` — typed errors.

## Hard rules

- Keep `domain/` free of filesystem, process, env, and terminal I/O. I/O stays
  at `io/` and `commands/` boundaries; never hide it in helpers that look pure.
- The engine never panics and never denies on a parse or internal error:
  unparseable or unsupported input defers.
- Typed errors via `thiserror`; surface them only at the app boundary. Validate
  config and inputs at load/parse time and return typed errors.
- `pub(crate)` by default. The intentional public API is `domain`, `config`,
  `errors`, and `run`.
- Prefer plain functions, structs, and enums; add traits only at real
  boundaries. No speculative abstractions, no `utils` grab-bags.

## Toolchain & dependencies

- `cargo` is the source of truth; `Cargo.lock` is committed (binary crate).
- Bump the pinned toolchain and dependencies deliberately via `just upgrade`,
  then re-run the gate and review the lockfile diff.
- Keep dependency features minimal; one tool per role. Run every task through
  `just`.

## Quality gate (no exceptions)

- Every check is pass/fail, never pass-with-warnings. Lints run with
  `-D warnings`; format and coverage misses fail their command;
  dependency/security/license checks fail on any configured issue.
- Convert warning-level diagnostics to errors or disable the check. No lint
  baselines, no ignored-warning backlog. Keep aspirational checks disabled until
  they can be enforced as errors.
- `just full-check` is the gate and stops at the first failing phase.
- Successful `just` recipes print little; failures preserve paths, line/columns,
  rule names, diffs, and exit codes. Noisy diagnostics belong in explicit recipes
  (`doctor`, `cargo-tree`), never in the default gate.
- The performance suite (`benches/`, the `bench*`/`profile` recipes, the perf CI
  job) is informational and stays out of `full-check`: its timings are
  non-deterministic, so it reports rather than gates, like the live harness check.

## Tests

- Unit-test pure logic; integration-test library and rule behavior; end-to-end
  tests run the compiled binary and assert exit code, stdout, stderr, and file
  effects across critical user journeys — not just that it starts.
- A user-visible change or bug fix ships with a test that fails without it. Tests
  are deterministic, isolated (temp dirs/fixtures), and network-free.
- Coverage is enforced with a floor; a miss fails the command. Do not lower the
  floor to pass.

## Releasing & CI

- CI runs the full gate on Linux/macOS/Windows for every PR and main push with
  least-privilege permissions; it must pass before any release artifact
  publishes.
- Tagging `vX.Y.Z` builds, archives, and checksums cross-platform binaries.
  crates.io publishing is a separate gated step.

## Workflow

- The agent manages git state end to end (branch, add, commit, push) but commits
  only working, gate-passing changes.
- Documentation and comments are written for the future reader, not as a log of
  how the repo was built. Avoid "added", "we decided", "during setup". Comment
  surprising constraints and domain decisions, not obvious code.

## Instruction files

- Encode durable design constraints in the nearest applicable `AGENTS.md`, not in
  one-off notes or commit messages.
- Keep every `AGENTS.md` platform-neutral, minimal, and high-signal: short
  imperatives, repo-specific, non-obvious. No agent-product names, settings
  paths, permission syntax, or allowlist details — those live only in that
  product's own settings file.
- Each `CLAUDE.md` is a symlink to its sibling `AGENTS.md` and must never carry
  independent content. Nested `AGENTS.md` files hold only subtree-specific
  constraints and do not repeat root guidance.

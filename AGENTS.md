# AGENTS

Rust CLI that gates AI-agent shell commands: parse bash into role-tagged
fragments, match each against rules, compose one verdict. Single binary,
edition 2021, toolchain pinned in `rust-toolchain.toml`.

## Two standing goals on every task

The user drives product features and their request is the priority — but carry
two goals into *every* task. When either is the lowest-error path to what the
user asked, fold it into the same task without asking first; surface the rest as
follow-ups (see "Workflow").

1. **Engineer the context for next time.** Make the next agent (and you) see
   more for less: realistic end-to-end tests that exercise what the user
   actually sees — especially when they report a bug existing tests missed (the
   `tests/e2e` suite drives the compiled binary, see "Tests") — scripts and
   skills that automate repetitive steps and shrink their output to signal, and
   terse `AGENTS.md` notes capturing what the code doesn't make obvious.
2. **Engineer the codebase and environment.** Be the engineer the user isn't:
   prioritize the technical initiatives that keep the codebase clean,
   maintainable, and repeatable, and keep environment setup automated and
   consistent (`just bootstrap` from a clean clone). Strict quality gates plus
   local/CI parity (same checks, same pinned toolchain in `rust-toolchain.toml`)
   make results repeatable — not "works on my machine." A clean base and a
   reproducible environment are usually how the user's feature ships with a low
   error rate.

## Stack and composition

- **Product shape:** a command-line tool — a single installable binary users run
  as `allowlister` (and as an agent harness hook). Composed from the create-repo
  skill's `shapes/cli.md`: e2e drives the compiled binary as a subprocess and
  asserts exit code, stdout, stderr, and file effects; arguments and stdin are
  validated at the edge; success is quiet, errors are explicit.
- **Language:** Rust (stable, pinned in `rust-toolchain.toml`). Composed from
  `languages/rust.md`: `rustfmt` + `clippy -D warnings` as strict gates,
  `cargo nextest` for the test runner, `cargo llvm-cov` enforcing the 95%
  coverage floor (lines, functions, regions) in the gate, and `cargo deny` +
  `cargo machete` as a dedicated supply-chain job. The release tier follows the
  reference's tag-driven `release-plz` + per-target native-runner archive model.
- **Intersection:** `intersections/rust-cli.md` — the binary-driving suite is a
  crate of its own, still in the gate, and every install surface shares the
  release workflow's asset names.
- **Base + cross-cutting:** `base.md`, `ci.md`, `llmlint.md`, `releasing.md`
  (see "Commits, releases, and merging") and `project-graph.md` (Nx runs the
  targets; Cargo keeps one `Cargo.lock`). Each crate keeps a `typecheck` target,
  so a type error fails by name ahead of clippy's findings.
- **Excluded — and why:** `shapes/library.md` does not apply: this ships an
  executable, not a published library API (the intentionally public Rust surface
  in `lib.rs` exists only to test the engine, not as a distribution target).

## Layout

- `src/main.rs` — thin: parse args, dispatch, map the typed result to an exit
  code, print. No behavior here.
- `src/lib.rs` — wiring and the public API; `run()` is the entry point.
- `src/cli.rs` — clap definitions: args, subcommands, defaults in one place.
- `src/domain/` — pure engine (glob, rule, analyzer, decision). No I/O.
- `src/io/` — filesystem config discovery, harness stdin/stdout adapters, and the
  opt-in usage-history store.
- `src/commands/` — one module per CLI verb; orchestrates domain + io.
- `src/config.rs` — JSON rule schema and user/project merge (a boundary, not
  domain).
- `src/errors.rs` — typed errors.

Nx projects (`nx show projects`; each `project.json` says what it holds). A
project owns every file under its directory; a file it reads
from elsewhere is a `{workspaceRoot}/...` input of its target, which Nx also
treats as affecting it. Edges (Cargo path deps, `implicitDependencies`) are only
for what a project builds against or drives; a Cargo edge is always restated as
an implicit dependency (Nx reads no manifest; `workspace:lint` checks it).
`.nxignore` keeps files no target reads (agent notes, changelog, contributor
docs) out of the graph — never list a file a target reads there. A suite that
touches an external service is a `type:live` project with no gate target.

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
- `scripts/setup.sh` (`just setup`) is the one idempotent entry point for local
  setup; keep it re-runnable. `scripts/setup-check.sh` (`just setup-check`) stays
  a fast, install-free readiness check — resolved tools plus a fingerprint stamp
  (under `.dev/`) of the pinned versions, so it re-provisions after `just upgrade`.
- asdf (`.tool-versions`) pins only bootstrap tooling such as `just`, never the
  Rust toolchain: `rust-toolchain.toml` + rustup remain its single source of
  truth. direnv (`.envrc`) only layers tool paths onto the shell — no behavior.

## Quality gate (no exceptions)

- Every check is pass/fail, never pass-with-warnings. Lints run with
  `-D warnings`; format and coverage misses fail their command;
  dependency/security/license checks fail on any configured issue.
- Convert warning-level diagnostics to errors or disable the check. No lint
  baselines, no ignored-warning backlog. Keep aspirational checks disabled until
  they can be enforced as errors.
- `just check` (alias `full-check`) is the gate over the affected projects;
  `just check all` sweeps every gate-eligible project; any failing target fails
  the recipe. The affected tier always keys off an explicit base (`NX_BASE`,
  else the merge base with `origin/main`), never Nx's default. A check that
  belongs in the gate is a target in that list, never a step outside Nx.
- Module boundaries are tags checked by `workspace:lint`
  (`tools/project-boundaries.json`): the contract depends on nothing it serves,
  the crate on none of its suites, and nothing outside the live tier on a live
  project.
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
- Coverage is enforced with a floor (95% lines, functions and regions, over the
  crate's sources minus `src/main.rs` and `tests/`, combined across the unit and
  e2e projects by `coverage:coverage`); a miss fails the command. Do not lower
  the floor to pass.

## Commits, releases, and merging

- Releases are **batched**: release-plz's release PR accumulates every merge
  since the last release, so the shipped commit is not one any merge job swept.
  The **broader tier** (`just check all`) therefore runs on that release PR
  (head branch `release-plz-*`) — release-prep — and **merge-to-main stays on the
  affected tier**, as does every other PR. `.github/scripts/gate-tier.mjs` routes
  each event; the same `test (<os>)`, `coverage` and `deps & security` contexts
  report on every PR, so a red sweep keeps the release PR's required contexts red
  and auto-merge cannot cut a release past it.
- CI runs the gate on Linux/macOS/Windows with least-privilege permissions; it
  must pass before any release artifact publishes. `release.yml` re-runs clippy,
  the unit tests and e2e over the whole crate on `release: published`, kept on
  purpose for a release cut by hand.
- Squash-merge makes the PR title the release input: the `pr-title` check admits
  exactly the Conventional types `release-plz.toml`'s `commit_parsers` name.
- Releases are automated from Conventional Commits by release-plz: a merged
  release PR bumps the version + changelog, tags `vX.Y.Z`, and cuts the GitHub
  Release, which builds, archives, and checksums cross-platform binaries. Never
  bump the version or tag by hand. Commit messages are the release input — keep
  them conventional. crates.io publishing is a separate, opt-in step.
- Pre-1.0, the minor slot acts as the major: `fix`/`perf`/`feat` are patches and
  only `feat!`/`BREAKING CHANGE` bumps the minor (`0.1.x`→`0.2.0`). Cut a
  feature-milestone release with `feat!`; a plain `feat` cannot bump the minor
  before 1.0 (a Cargo-semver constraint, not a setting).

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

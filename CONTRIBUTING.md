# Contributing

## Setup

One command provisions a fresh machine — asdf + direnv, the pinned Rust
toolchain, the cargo dev tools, and git hooks:

```sh
./scripts/setup.sh      # or `just setup` once `just` is on PATH
```

It is idempotent: re-running fills in only what is missing.
`just setup-check` is the fast, install-free counterpart — it answers "is this
machine set up?" from the resolved tools plus a fingerprint stamp of the pinned
versions, and setup re-runs automatically after a `just upgrade` changes them.

What setup wires up:

- **asdf** (`.tool-versions`) pins `just`. The Rust toolchain is deliberately
  *not* managed by asdf — `rust-toolchain.toml` + rustup stay the single source
  of truth for the channel, components, and targets.
- **direnv** (`.envrc`) layers the asdf and cargo tool paths onto your shell for
  this directory; setup runs `direnv allow` for you.
- **`just bootstrap`** installs the cargo dev tools (nextest, llvm-cov, deny,
  machete, audit) and the git hooks.

Prefer to do it by hand? Install [rustup](https://rustup.rs) and
[just](https://just.systems), run `rustup show` (installs the pinned toolchain),
then `just bootstrap`.

Then run the full quality gate before pushing:

```sh
just full-check
```

## Quality gate

`just full-check` runs, stopping at the first failing phase: format check,
`cargo check`, clippy (`-D warnings`), unit/integration tests, end-to-end tests,
coverage with an enforced floor, dependency/security/license checks, docs build,
release build, and the dist plan. Run phases individually while iterating:
`just fmt`, `just check`, `just clippy`, `just test`, `just test-e2e`,
`just test-cov`, `just deps-check`, `just security`, `just doc`.

### No warning backlogs

Every check is **pass/fail, never pass-with-warnings**. If a tool emits a
warning, either fix it, convert it to an error, or disable that check until it
can be enforced. Do not add lint baselines or ignored-warning backlogs. Optional
or aspirational checks stay disabled until they are enforceable as errors.

### Quiet success, actionable failure

Successful `just` recipes print little beyond the underlying tool's minimal
output. Failures must preserve enough to debug: file path, line/column, rule
name or diagnostic code, the concise message, and for CLI tests the stdout/stderr
diff and exit-code mismatch. Do not wrap commands in ways that strip this detail.
Noisy inspection lives in dedicated recipes: `just doctor`, `just cargo-tree`,
`just cargo-metadata` is not a quality-gate phase.

## Architecture

- `src/main.rs` is thin: parse args, dispatch, map the typed result to an exit
  code, print. All behavior lives in the library.
- `src/domain/` is the pure engine (parsing, rules, decision) with **no I/O**.
- `src/io/` holds filesystem config discovery and the harness stdin/stdout
  adapters.
- `src/commands/` orchestrates one CLI verb each.
- Errors are typed (`src/errors.rs`); validation happens at load/parse
  boundaries. The hook path never turns an internal error into a deny.

Durable design constraints live in the nearest `AGENTS.md`, not in PR
descriptions or commit messages.

## Tests

Add tests with the change, not after. Cover pure logic with unit tests, library
and rule behavior with integration tests, and **critical user journeys with
end-to-end tests that run the compiled binary** (exit code, stdout, stderr, file
effects). A new user-visible behavior or bug fix should come with an E2E or
golden case that would fail without it. Tests must be deterministic, isolated
(temp dirs, local fixtures), and free of network dependencies.

## Performance

The engine ships with an **informational** performance suite — it measures, it
does not gate, so it is not part of `just full-check`:

- `just bench` — Criterion micro-benchmarks of the engine hot paths
  (`analyze`/`decide`/`evaluate` and config load); in-process and low-noise.
- `just bench-cli` — end-to-end CLI latency for every command via hyperfine
  (real process startup + config + parse), writing `target/bench/results.*`.
- `just bench-compare` — diff the latest `bench` run against a `base` baseline
  saved earlier with `just bench-base` (e.g. on `main`, before a change).
- `just profile [...]` — record a sampling profile with samply to find
  bottlenecks: the engine hot path by default, or a looped real CLI invocation
  (`just profile check 'rm -rf /'`).

`just bootstrap` installs the tools (hyperfine, critcmp, samply). On every pull
request, CI runs the same suite on a fixed runner and posts the numbers as a
sticky comment and a job summary; once the bench lands on `main`, that comment
also shows the regression delta versus the base. Because the timings are noisy,
the job reports rather than blocks — do not add it to required checks.

## Dependency upgrades

```sh
just upgrade        # updates Cargo.lock; review the diff
just full-check
```

`just upgrade` may change `Cargo.lock`, transitive dependency versions, and
cargo-installed tool versions; review `git diff` before committing and re-run the
full gate. Do not mutate dependencies outside this flow without explicit review.

## Releasing

Releases are automated from [Conventional Commits](https://www.conventionalcommits.org)
by [release-plz](https://release-plz.dev) — you never edit the version or tag by
hand:

1. Land changes on `main` with conventional commit messages.
2. release-plz opens a **release PR** that bumps `Cargo.toml` + `Cargo.lock` and
   writes the `CHANGELOG.md` section.
3. Merge the release PR. release-plz tags `vX.Y.Z` and cuts the GitHub Release;
   that triggers the binary build, which archives, checksums, and uploads the
   cross-platform artifacts. crates.io publishing is a separate, opt-in step
   (`PUBLISH_TO_CRATES_IO` + `CARGO_REGISTRY_TOKEN`).

### Commit type → version bump

The commit type drives the bump. **Pre-1.0** the project follows Cargo's 0.x
rules, where the *minor* slot acts as the major:

| Commit type | Effect pre-1.0 | Effect at ≥1.0 |
| --- | --- | --- |
| `fix:` / `perf:` | patch (`0.1.0`→`0.1.1`) | patch |
| `feat:` | patch (`0.1.0`→`0.1.1`) | minor |
| `feat!:` / `BREAKING CHANGE:` | **minor** (`0.1.1`→`0.2.0`) | major |
| `docs:` / `test:` / `chore:` / `ci:` | no release | no release |

So **to cut a feature-milestone (minor) release before 1.0, mark the commit
`feat!:` (or add a `BREAKING CHANGE:` footer)** — a plain `feat:` is only a
patch until the project reaches 1.0. This is a release-plz/Cargo-semver
constraint, not a preference: there is no config to make a plain `feat` bump the
minor pre-1.0.

Because the crate is not published to crates.io, the release workflow feeds
release-plz the previous release tag as its version baseline
(`--registry-manifest-path`), so the bump is computed from git history alone.

## Agent-assisted contributions

This repo is set up for AI coding agents. Repository conventions for agents live
in the platform-neutral `AGENTS.md` files (root and nested); agent-product
permission configuration lives only in that product's own settings file.

For the Claude Code agent, a `SessionStart` hook in `.claude/settings.json` runs
the lightweight `scripts/setup-check.sh` at the start of a session and provisions
the environment once (via `scripts/setup.sh`) if it is not ready — so a fresh
clone, including a cloud agent's, sets itself up automatically. It is a fast
no-op when already set up, never re-runs after a failed attempt (it advises
instead), and is skipped in this repo's GitHub Actions CI or when
`ALLOWLISTER_SKIP_SETUP` is set.

This project uses a narrow, repo-scoped command allowlist for the Claude Code
agent in `.claude/settings.json`: common quality-gate operations are allowed
through `just` recipes (e.g. `just full-check`, `just test`, `just clippy`), with
narrow direct invocations of `cargo`, `rustup`, `nextest`, `llvm-cov`,
`cargo-deny`, `cargo-audit`, `cargo-machete`, `cargo-dist`, `lefthook`, and
git state commands. It deliberately contains **no broad shell allow rules**
(no `Bash(*)`, `Bash(cargo *)`, `Bash(git *)`, etc.) and **no deny list** — it is
allowlist-only.

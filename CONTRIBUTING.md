# Contributing

## Setup

1. Install Rust via [rustup](https://rustup.rs) and [just](https://just.systems).
2. Confirm the pinned toolchain (channel, components, targets):
   ```sh
   rustup show
   ```
   `rust-toolchain.toml` pins everything; rustup installs it on first use.
3. Install developer tools and git hooks:
   ```sh
   just bootstrap
   ```
4. Run the full quality gate before pushing:
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

## Dependency upgrades

```sh
just upgrade        # updates Cargo.lock; review the diff
just full-check
```

`just upgrade` may change `Cargo.lock`, transitive dependency versions, and
cargo-installed tool versions; review `git diff` before committing and re-run the
full gate. Do not mutate dependencies outside this flow without explicit review.

## Releasing

1. Bump `version` in `Cargo.toml`; move the `CHANGELOG.md` "Unreleased" notes
   under a new version heading.
2. `just full-check`.
3. Commit, tag `vX.Y.Z`, and push the tag.
4. CI must pass; the release workflow builds, archives, checksums, and uploads
   the cross-platform binaries. crates.io publishing is a separate gated step
   requiring `CARGO_REGISTRY_TOKEN`.

## Agent-assisted contributions

This repo is set up for AI coding agents. Repository conventions for agents live
in the platform-neutral `AGENTS.md` files (root and nested); agent-product
permission configuration lives only in that product's own settings file.

This project uses a narrow, repo-scoped command allowlist for the Claude Code
agent in `.claude/settings.json`: common quality-gate operations are allowed
through `just` recipes (e.g. `just full-check`, `just test`, `just clippy`), with
narrow direct invocations of `cargo`, `rustup`, `nextest`, `llvm-cov`,
`cargo-deny`, `cargo-audit`, `cargo-machete`, `cargo-dist`, `lefthook`, and
git state commands. It deliberately contains **no broad shell allow rules**
(no `Bash(*)`, `Bash(cargo *)`, `Bash(git *)`, etc.) and **no deny list** — it is
allowlist-only.

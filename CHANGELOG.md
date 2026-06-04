# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/nickderobertis/allowlister/compare/v0.1.1...v0.2.0) - 2026-06-04

### Added

- [**breaking**] promote the init setup consolidation to the 0.2.0 milestone ([#16](https://github.com/nickderobertis/allowlister/pull/16))

## [0.1.1](https://github.com/nickderobertis/allowlister/compare/v0.1.0...v0.1.1) - 2026-06-04

### Added

- *(init)* consolidate setup into init and automate releases ([#13](https://github.com/nickderobertis/allowlister/pull/13))

### Documentation

- add asdf plugin as an install option ([#7](https://github.com/nickderobertis/allowlister/pull/7))

### Other

- Add `install` command to merge allowlists/profiles into a config ([#6](https://github.com/nickderobertis/allowlister/pull/6))
- Add recommended read-only and repo-write allowlist profiles ([#5](https://github.com/nickderobertis/allowlister/pull/5))
- Match metacharacter-free globs as literals, skipping regex build
- Add cross-platform install script for prebuilt binaries ([#3](https://github.com/nickderobertis/allowlister/pull/3))
- Add performance benchmarking and profiling suite

### Performance

- skip redundant validation when installing a built-in profile ([#9](https://github.com/nickderobertis/allowlister/pull/9))

## [0.1.0] - 2026-06-03

### Added

- Bash AST analyzer that decomposes a command into role-tagged fragments
  (`standalone`, `pipe_source`, `pipe_filter`, `subshell`, `substitution`).
- Rule engine matching fragments by `match` (joined argv) or `argv`
  (per-element, with a trailing `**`), gated on role and redirections.
- Glob matcher with bash extglob support (`@(…)`, `?(…)`, `*(…)`, `+(…)`,
  `!(…)`), plus `regex` and `literal` rule kinds.
- Redirection-aware allow selection: out-redirects denied by default, with
  `write_glob`/`read_glob`/`deny` policies.
- Set-theoretic decision algorithm: any deny denies, any allow allows, otherwise
  defer; parse errors and unsupported constructs always defer.
- Process-wrapper stripping (`timeout`, `time`, `nice`, `nohup`, `stdbuf`) and
  bare-`xargs` unwrapping for parity with the harness's own matching.
- Claude Code `PreToolUse` hook adapter; Cursor and Copilot adapter stubs.
- CLI: `hook`, `check`, `explain`, `init`.
- User + project config discovery and merge with strict JSON validation.

[Unreleased]: https://github.com/nickderobertis/allowlister/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/nickderobertis/allowlister/releases/tag/v0.1.0

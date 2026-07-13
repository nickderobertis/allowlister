# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.9](https://github.com/nickderobertis/allowlister/compare/v0.5.8...v0.5.9) - 2026-07-13

### Fixed

- gate edits to allowlister's own config; slim live-e2e CI ([#123](https://github.com/nickderobertis/allowlister/pull/123))

## [0.5.8](https://github.com/nickderobertis/allowlister/compare/v0.5.7...v0.5.8) - 2026-07-08

### Fixed

- *(profiles)* allow read-only glob & grep tools in bundled profiles ([#120](https://github.com/nickderobertis/allowlister/pull/120))

## [0.5.7](https://github.com/nickderobertis/allowlister/compare/v0.5.6...v0.5.7) - 2026-07-08

### Added

- gate built-in file tools in bundled profiles, scoped to the config directory ([#117](https://github.com/nickderobertis/allowlister/pull/117))

## [0.5.6](https://github.com/nickderobertis/allowlister/compare/v0.5.5...v0.5.6) - 2026-06-25

### Fixed

- *(profiles)* tighten repo-write — defer general code execution, config & remote-exec ([#115](https://github.com/nickderobertis/allowlister/pull/115))

## [0.5.5](https://github.com/nickderobertis/allowlister/compare/v0.5.4...v0.5.5) - 2026-06-22

### Documentation

- add allowlister logo banner to README ([#113](https://github.com/nickderobertis/allowlister/pull/113))

## [0.5.4](https://github.com/nickderobertis/allowlister/compare/v0.5.3...v0.5.4) - 2026-06-20

### Added

- forward harness session id to approval plugins (protocol v3) ([#111](https://github.com/nickderobertis/allowlister/pull/111))

## [0.5.3](https://github.com/nickderobertis/allowlister/compare/v0.5.2...v0.5.3) - 2026-06-20

### Fixed

- note container support for the static Linux binaries ([#109](https://github.com/nickderobertis/allowlister/pull/109))

## [0.5.2](https://github.com/nickderobertis/allowlister/compare/v0.5.1...v0.5.2) - 2026-06-20

### Performance

- ship static, non-PIE Linux and static-CRT Windows release binaries ([#106](https://github.com/nickderobertis/allowlister/pull/106))

## [0.5.1](https://github.com/nickderobertis/allowlister/compare/v0.5.0...v0.5.1) - 2026-06-20

### Added

- deliver resolved project identity to approval plugins ([#104](https://github.com/nickderobertis/allowlister/pull/104))

## [0.5.0](https://github.com/nickderobertis/allowlister/compare/v0.4.20...v0.5.0) - 2026-06-19

### Added

- [**breaking**] protocol v2 plugin payload — structured fragments and tool-call support ([#100](https://github.com/nickderobertis/allowlister/pull/100))

## [0.4.20](https://github.com/nickderobertis/allowlister/compare/v0.4.19...v0.4.20) - 2026-06-18

### Fixed

- emit complete Cursor hook responses ([#98](https://github.com/nickderobertis/allowlister/pull/98))

## [0.4.19](https://github.com/nickderobertis/allowlister/compare/v0.4.18...v0.4.19) - 2026-06-18

### Fixed

- let Cursor read defers fall through ([#94](https://github.com/nickderobertis/allowlister/pull/94))

## [0.4.18](https://github.com/nickderobertis/allowlister/compare/v0.4.17...v0.4.18) - 2026-06-17

### Other

- Add external dynamic approval plugins (schema, loader, runtime, example) ([#92](https://github.com/nickderobertis/allowlister/pull/92))

## [0.4.17](https://github.com/nickderobertis/allowlister/compare/v0.4.16...v0.4.17) - 2026-06-16

### Added

- stamp configs with the published "$schema" on init/install ([#89](https://github.com/nickderobertis/allowlister/pull/89))
- publish a JSON Schema for config files ([#88](https://github.com/nickderobertis/allowlister/pull/88))
- track usage history by git repository, not folder ([#85](https://github.com/nickderobertis/allowlister/pull/85))

### Changed

- *(io)* adopt oneharness-core as the hook installer ([#83](https://github.com/nickderobertis/allowlister/pull/83))

### Fixed

- *(copilot)* normalize MCP tool names from the preToolUse dash form ([#81](https://github.com/nickderobertis/allowlister/pull/81))

## [0.4.16](https://github.com/nickderobertis/allowlister/compare/v0.4.15...v0.4.16) - 2026-06-12

### Added

- make init idempotent so a second harness wires cleanly ([#78](https://github.com/nickderobertis/allowlister/pull/78))

## [0.4.15](https://github.com/nickderobertis/allowlister/compare/v0.4.14...v0.4.15) - 2026-06-11

### Performance

- cut per-spawn cost ~70% by deferring regex-engine work ([#76](https://github.com/nickderobertis/allowlister/pull/76))

## [0.4.14](https://github.com/nickderobertis/allowlister/compare/v0.4.13...v0.4.14) - 2026-06-11

### Added

- add `config add`, `config remove`, and `config show` commands ([#74](https://github.com/nickderobertis/allowlister/pull/74))

## [0.4.13](https://github.com/nickderobertis/allowlister/compare/v0.4.12...v0.4.13) - 2026-06-11

### Added

- track recency in usage history and weigh it when refining ([#70](https://github.com/nickderobertis/allowlister/pull/70))

## [0.4.12](https://github.com/nickderobertis/allowlister/compare/v0.4.11...v0.4.12) - 2026-06-11

### Added

- preserve comments on config updates and default to .jsonc

### Other

- Merge pull request #71 from nickderobertis/claude/config-format-json-yaml-jeo8dz

## [0.4.11](https://github.com/nickderobertis/allowlister/compare/v0.4.10...v0.4.11) - 2026-06-11

### Added

- explain every undecided fragment in a defer reason ([#68](https://github.com/nickderobertis/allowlister/pull/68))

## [0.4.10](https://github.com/nickderobertis/allowlister/compare/v0.4.9...v0.4.10) - 2026-06-10

### Performance

- defer glob regex compilation behind a literal-prefix gate

## [0.4.9](https://github.com/nickderobertis/allowlister/compare/v0.4.8...v0.4.9) - 2026-06-10

### Added

- surface the projects each subcommand ran in via history ([#60](https://github.com/nickderobertis/allowlister/pull/60))
- list every asking fragment in the ask verdict reason ([#58](https://github.com/nickderobertis/allowlister/pull/58))
- tier allowlister's own commands in the recommended profiles ([#56](https://github.com/nickderobertis/allowlister/pull/56))
- allow discard redirects to /dev/null and standard-stream devices ([#54](https://github.com/nickderobertis/allowlister/pull/54))
- add refine-allowlist agent skill to tune configs from history ([#48](https://github.com/nickderobertis/allowlister/pull/48))
- record and report allowlister usage history ([#46](https://github.com/nickderobertis/allowlister/pull/46))
- [**breaking**] add ask verdict and re-tier recommended profiles
- *(tools)* complete tool-use gating with OpenCode and Cursor ([#34](https://github.com/nickderobertis/allowlister/pull/34))
- *(tools)* extend tool-use gating to Codex, Copilot, Qwen, Crush, Goose
- *(tools)* gate non-shell tool calls (engine + config + Claude Code) ([#32](https://github.com/nickderobertis/allowlister/pull/32))
- add redirection-only rule type for profile-wide scratch writes ([#30](https://github.com/nickderobertis/allowlister/pull/30))
- *(opencode)* add OpenCode support via a tool.execute.before plugin shim ([#28](https://github.com/nickderobertis/allowlister/pull/28))
- *(goose)* add full Goose support via the PreToolUse hook plugin ([#27](https://github.com/nickderobertis/allowlister/pull/27))
- *(qwen)* add full Qwen Code support via the PreToolUse hook ([#26](https://github.com/nickderobertis/allowlister/pull/26))
- *(crush)* add full Crush support via the PreToolUse hook ([#25](https://github.com/nickderobertis/allowlister/pull/25))
- *(codex)* add full OpenAI Codex CLI support via the PreToolUse hook ([#24](https://github.com/nickderobertis/allowlister/pull/24))
- *(copilot)* [**breaking**] add full GitHub Copilot CLI support via the preToolUse hook ([#23](https://github.com/nickderobertis/allowlister/pull/23))
- *(cursor)* add full Cursor support via the beforeShellExecution hook ([#21](https://github.com/nickderobertis/allowlister/pull/21))
- *(repo-write)* let text filters redirect stdout to scratch/build paths
- [**breaking**] promote the init setup consolidation to the 0.2.0 milestone ([#16](https://github.com/nickderobertis/allowlister/pull/16))
- *(init)* consolidate setup into init and automate releases ([#13](https://github.com/nickderobertis/allowlister/pull/13))

### Documentation

- advertise the cross-platform allowlist with a support matrix ([#29](https://github.com/nickderobertis/allowlister/pull/29))
- add asdf plugin as an install option ([#7](https://github.com/nickderobertis/allowlister/pull/7))

### Fixed

- harden read-only profile against secret-read, write, and code-exec bypasses ([#18](https://github.com/nickderobertis/allowlister/pull/18))

### Other

- Add `install` command to merge allowlists/profiles into a config ([#6](https://github.com/nickderobertis/allowlister/pull/6))
- Add recommended read-only and repo-write allowlist profiles ([#5](https://github.com/nickderobertis/allowlister/pull/5))
- Match metacharacter-free globs as literals, skipping regex build
- Add cross-platform install script for prebuilt binaries ([#3](https://github.com/nickderobertis/allowlister/pull/3))
- Add performance benchmarking and profiling suite
- Initial release: structural allow/deny/defer engine for agent shell commands

### Performance

- skip redundant validation when installing a built-in profile ([#9](https://github.com/nickderobertis/allowlister/pull/9))

## [0.4.8](https://github.com/nickderobertis/allowlister/compare/v0.4.7...v0.4.8) - 2026-06-10

### Added

- surface the projects each subcommand ran in via history ([#60](https://github.com/nickderobertis/allowlister/pull/60))

## [0.4.7](https://github.com/nickderobertis/allowlister/compare/v0.4.6...v0.4.7) - 2026-06-10

### Added

- list every asking fragment in the ask verdict reason ([#58](https://github.com/nickderobertis/allowlister/pull/58))

## [0.4.6](https://github.com/nickderobertis/allowlister/compare/v0.4.5...v0.4.6) - 2026-06-10

### Added

- tier allowlister's own commands in the recommended profiles ([#56](https://github.com/nickderobertis/allowlister/pull/56))

## [0.4.5](https://github.com/nickderobertis/allowlister/compare/v0.4.4...v0.4.5) - 2026-06-10

### Added

- allow discard redirects to /dev/null and standard-stream devices ([#54](https://github.com/nickderobertis/allowlister/pull/54))

## [0.4.4](https://github.com/nickderobertis/allowlister/compare/v0.4.3...v0.4.4) - 2026-06-08

### Added

- add refine-allowlist agent skill to tune configs from history ([#48](https://github.com/nickderobertis/allowlister/pull/48))
- record and report allowlister usage history ([#46](https://github.com/nickderobertis/allowlister/pull/46))
- [**breaking**] add ask verdict and re-tier recommended profiles
- *(tools)* complete tool-use gating with OpenCode and Cursor ([#34](https://github.com/nickderobertis/allowlister/pull/34))
- *(tools)* extend tool-use gating to Codex, Copilot, Qwen, Crush, Goose
- *(tools)* gate non-shell tool calls (engine + config + Claude Code) ([#32](https://github.com/nickderobertis/allowlister/pull/32))
- add redirection-only rule type for profile-wide scratch writes ([#30](https://github.com/nickderobertis/allowlister/pull/30))
- *(opencode)* add OpenCode support via a tool.execute.before plugin shim ([#28](https://github.com/nickderobertis/allowlister/pull/28))
- *(goose)* add full Goose support via the PreToolUse hook plugin ([#27](https://github.com/nickderobertis/allowlister/pull/27))
- *(qwen)* add full Qwen Code support via the PreToolUse hook ([#26](https://github.com/nickderobertis/allowlister/pull/26))
- *(crush)* add full Crush support via the PreToolUse hook ([#25](https://github.com/nickderobertis/allowlister/pull/25))
- *(codex)* add full OpenAI Codex CLI support via the PreToolUse hook ([#24](https://github.com/nickderobertis/allowlister/pull/24))
- *(copilot)* [**breaking**] add full GitHub Copilot CLI support via the preToolUse hook ([#23](https://github.com/nickderobertis/allowlister/pull/23))
- *(cursor)* add full Cursor support via the beforeShellExecution hook ([#21](https://github.com/nickderobertis/allowlister/pull/21))
- *(repo-write)* let text filters redirect stdout to scratch/build paths
- [**breaking**] promote the init setup consolidation to the 0.2.0 milestone ([#16](https://github.com/nickderobertis/allowlister/pull/16))
- *(init)* consolidate setup into init and automate releases ([#13](https://github.com/nickderobertis/allowlister/pull/13))

### Documentation

- advertise the cross-platform allowlist with a support matrix ([#29](https://github.com/nickderobertis/allowlister/pull/29))
- add asdf plugin as an install option ([#7](https://github.com/nickderobertis/allowlister/pull/7))

### Fixed

- harden read-only profile against secret-read, write, and code-exec bypasses ([#18](https://github.com/nickderobertis/allowlister/pull/18))

### Other

- Add `install` command to merge allowlists/profiles into a config ([#6](https://github.com/nickderobertis/allowlister/pull/6))
- Add recommended read-only and repo-write allowlist profiles ([#5](https://github.com/nickderobertis/allowlister/pull/5))
- Match metacharacter-free globs as literals, skipping regex build
- Add cross-platform install script for prebuilt binaries ([#3](https://github.com/nickderobertis/allowlister/pull/3))
- Add performance benchmarking and profiling suite
- Initial release: structural allow/deny/defer engine for agent shell commands

### Performance

- skip redundant validation when installing a built-in profile ([#9](https://github.com/nickderobertis/allowlister/pull/9))

## [0.4.3](https://github.com/nickderobertis/allowlister/compare/v0.4.2...v0.4.3) - 2026-06-08

### Added

- add refine-allowlist agent skill to tune configs from history ([#48](https://github.com/nickderobertis/allowlister/pull/48))

## [0.4.2](https://github.com/nickderobertis/allowlister/compare/v0.4.1...v0.4.2) - 2026-06-07

### Added

- record and report allowlister usage history ([#46](https://github.com/nickderobertis/allowlister/pull/46))

## [0.4.1](https://github.com/nickderobertis/allowlister/compare/v0.4.0...v0.4.1) - 2026-06-06

### Added

- [**breaking**] add ask verdict and re-tier recommended profiles
- *(tools)* complete tool-use gating with OpenCode and Cursor ([#34](https://github.com/nickderobertis/allowlister/pull/34))
- *(tools)* extend tool-use gating to Codex, Copilot, Qwen, Crush, Goose
- *(tools)* gate non-shell tool calls (engine + config + Claude Code) ([#32](https://github.com/nickderobertis/allowlister/pull/32))
- add redirection-only rule type for profile-wide scratch writes ([#30](https://github.com/nickderobertis/allowlister/pull/30))
- *(opencode)* add OpenCode support via a tool.execute.before plugin shim ([#28](https://github.com/nickderobertis/allowlister/pull/28))
- *(goose)* add full Goose support via the PreToolUse hook plugin ([#27](https://github.com/nickderobertis/allowlister/pull/27))
- *(qwen)* add full Qwen Code support via the PreToolUse hook ([#26](https://github.com/nickderobertis/allowlister/pull/26))
- *(crush)* add full Crush support via the PreToolUse hook ([#25](https://github.com/nickderobertis/allowlister/pull/25))
- *(codex)* add full OpenAI Codex CLI support via the PreToolUse hook ([#24](https://github.com/nickderobertis/allowlister/pull/24))
- *(copilot)* [**breaking**] add full GitHub Copilot CLI support via the preToolUse hook ([#23](https://github.com/nickderobertis/allowlister/pull/23))
- *(cursor)* add full Cursor support via the beforeShellExecution hook ([#21](https://github.com/nickderobertis/allowlister/pull/21))
- *(repo-write)* let text filters redirect stdout to scratch/build paths
- [**breaking**] promote the init setup consolidation to the 0.2.0 milestone ([#16](https://github.com/nickderobertis/allowlister/pull/16))
- *(init)* consolidate setup into init and automate releases ([#13](https://github.com/nickderobertis/allowlister/pull/13))

### Documentation

- advertise the cross-platform allowlist with a support matrix ([#29](https://github.com/nickderobertis/allowlister/pull/29))
- add asdf plugin as an install option ([#7](https://github.com/nickderobertis/allowlister/pull/7))

### Fixed

- harden read-only profile against secret-read, write, and code-exec bypasses ([#18](https://github.com/nickderobertis/allowlister/pull/18))

### Other

- Add `install` command to merge allowlists/profiles into a config ([#6](https://github.com/nickderobertis/allowlister/pull/6))
- Add recommended read-only and repo-write allowlist profiles ([#5](https://github.com/nickderobertis/allowlister/pull/5))
- Match metacharacter-free globs as literals, skipping regex build
- Add cross-platform install script for prebuilt binaries ([#3](https://github.com/nickderobertis/allowlister/pull/3))
- Add performance benchmarking and profiling suite
- Initial release: structural allow/deny/defer engine for agent shell commands

### Performance

- skip redundant validation when installing a built-in profile ([#9](https://github.com/nickderobertis/allowlister/pull/9))

## [0.4.0](https://github.com/nickderobertis/allowlister/compare/v0.3.1...v0.4.0) - 2026-06-06

### Added

- [**breaking**] add ask verdict and re-tier recommended profiles

## [0.3.1](https://github.com/nickderobertis/allowlister/compare/v0.3.0...v0.3.1) - 2026-06-05

### Added

- *(tools)* complete tool-use gating with OpenCode and Cursor ([#34](https://github.com/nickderobertis/allowlister/pull/34))
- *(tools)* extend tool-use gating to Codex, Copilot, Qwen, Crush, Goose
- *(tools)* gate non-shell tool calls (engine + config + Claude Code) ([#32](https://github.com/nickderobertis/allowlister/pull/32))
- add redirection-only rule type for profile-wide scratch writes ([#30](https://github.com/nickderobertis/allowlister/pull/30))

## [0.3.0](https://github.com/nickderobertis/allowlister/compare/v0.2.1...v0.3.0) - 2026-06-05

### Added

- *(opencode)* add OpenCode support via a tool.execute.before plugin shim ([#28](https://github.com/nickderobertis/allowlister/pull/28))
- *(goose)* add full Goose support via the PreToolUse hook plugin ([#27](https://github.com/nickderobertis/allowlister/pull/27))
- *(qwen)* add full Qwen Code support via the PreToolUse hook ([#26](https://github.com/nickderobertis/allowlister/pull/26))
- *(crush)* add full Crush support via the PreToolUse hook ([#25](https://github.com/nickderobertis/allowlister/pull/25))
- *(codex)* add full OpenAI Codex CLI support via the PreToolUse hook ([#24](https://github.com/nickderobertis/allowlister/pull/24))
- *(copilot)* [**breaking**] add full GitHub Copilot CLI support via the preToolUse hook ([#23](https://github.com/nickderobertis/allowlister/pull/23))
- *(cursor)* add full Cursor support via the beforeShellExecution hook ([#21](https://github.com/nickderobertis/allowlister/pull/21))

### Documentation

- advertise the cross-platform allowlist with a support matrix ([#29](https://github.com/nickderobertis/allowlister/pull/29))

## [0.2.1](https://github.com/nickderobertis/allowlister/compare/v0.2.0...v0.2.1) - 2026-06-04

### Added

- *(repo-write)* let text filters redirect stdout to scratch/build paths

### Fixed

- harden read-only profile against secret-read, write, and code-exec bypasses ([#18](https://github.com/nickderobertis/allowlister/pull/18))

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

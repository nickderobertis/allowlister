# allowlister

A small, fast Rust CLI that hooks into AI coding agents (Claude Code first;
Cursor/Copilot next) and decides whether to **allow**, **deny**, or **defer**
each shell command the agent wants to run.

It replaces the simplistic string-prefix allow lists that current agents ship
with. Instead of classifying a command as safe or unsafe in the abstract,
allowlister classifies each command by the **structural role** it plays in the
shell expression it appears in.

## Why

String-prefix allow lists fail in three concrete ways:

1. **They can't reason about composition.** You can't safely allow `head`
   because `head /etc/passwd` reads arbitrary files. But `gh pr list | head -20`
   is obviously fine. There's no way to say "`head` is OK reading from a pipe but
   not reading a file."
2. **Scripts of N safe commands get blocked.** `git status && git diff | head -5`
   needs three separate permission grants.
3. **Match syntax is anemic.** No multiple globs per rule, no scoping `gh api` to
   read methods within an org, no "any out-redirect target must live under
   `/tmp/` or `./build/`."

allowlister fixes all three by parsing the bash command, decomposing it into
per-command **fragments** tagged with their structural role, and evaluating each
fragment against rules that match by glob/regex/argv-pattern and gate on role and
redirections.

## The core idea

Walk the bash AST. For every simple command, emit one fragment tagged with its
role and its redirections. The whole input is allowed iff **every** fragment is
allowed. The rule engine never sees pipelines or subshells — only
`(argv, role, redirections)` tuples. Composition is automatic.

Five roles:

| Role | Where the command appears |
|------|---------------------------|
| `standalone` | top-level, output to the terminal |
| `pipe_source` | leftmost command in a pipeline |
| `pipe_filter` | non-leftmost command in a pipeline (stdin is piped) |
| `subshell` | inside `( … )`, `{ …; }`, or a `for`/`while`/`until`/`if` body |
| `substitution` | inside `$(…)`, backticks, or `<(…)`/`>(…)` |

This single move handles all three cases. For example, one rule —
`"match": "head *", "roles": ["pipe_filter", "substitution"]` — allows
`gh pr list | head -20` while a bare `head /etc/passwd` (role `standalone`)
falls through to `defer`.

## Install

### Install script (Linux, macOS)

```sh
curl -fsSL https://raw.githubusercontent.com/nickderobertis/allowlister/main/scripts/install.sh | sh
```

Detects your platform, downloads the matching prebuilt binary, verifies its
SHA-256 checksum, and installs to `~/.local/bin`. Pin a version or change the
target directory:

```sh
curl -fsSL https://raw.githubusercontent.com/nickderobertis/allowlister/main/scripts/install.sh \
  | sh -s -- --version v0.1.0 --to /usr/local/bin
```

It runs under any POSIX shell, including Git Bash and WSL on Windows. For native
Windows PowerShell, use the prebuilt archive or `cargo install` below.

### Prebuilt binaries

Download the archive for your platform from the
[latest release](https://github.com/nickderobertis/allowlister/releases/latest),
verify its `.sha256`, extract, and put `allowlister` on your `PATH`. Binaries are
published for Linux (x86_64, arm64), macOS (x86_64, arm64), and Windows (x86_64).

### From crates.io

```sh
cargo install allowlister --locked
```

### From source

```sh
git clone https://github.com/nickderobertis/allowlister
cd allowlister
just build-release   # or: cargo build --release --locked
```

## Quick start (Claude Code)

```sh
allowlister init            # writes a starter ~/.config/allowlister/config.json
```

`init` prints the exact `~/.claude/settings.json` snippet to paste. It registers
the hook for the `Bash` matcher and keeps `permissions.allow` / `permissions.ask`
empty so the hook is the source of allow truth, with a tiny nuclear-pattern
`permissions.deny` as defense-in-depth.

To start from a curated ruleset instead of the minimal starter, install one of
the [recommended profiles](#recommended-profiles) in place of `init`. When
`install` creates a new config it prints the same settings snippet, so this is a
complete first step on its own:

```sh
allowlister install read-only --global    # or: repo-write
```

> **Do not** add `"Bash"` or `"Bash(*)"` to `permissions.allow`. A broad allow
> makes the agent skip its prompt on its own, which short-circuits the hook's
> per-fragment allow analysis — the entire point of allowlister.

## Usage

```text
allowlister hook <harness>        Read hook JSON on stdin, write a decision on stdout.
                                  Harnesses: claude-code (cursor, copilot are stubs).
allowlister check '<cmd>' [--cwd P] [--json]
                                  Evaluate one command. Exit 0 for allow/defer, 2 for deny.
allowlister explain '<cmd>' [--cwd P]
                                  Verbose trace: config sources, fragments, per-fragment
                                  decision, and overall verdict. The primary debugging tool.
allowlister init [--global | --local]
                                  Write a starter config and print the settings snippet.
allowlister install <source> [--global | --local | --output P]
                                  Merge an allowlist (a built-in profile name —
                                  read-only or repo-write — or a path to a JSON
                                  file) into your config. Idempotent: re-running
                                  never duplicates rules.
```

Examples:

```sh
$ allowlister check 'gh pr list | head -20'
ALLOW: all 2 command(s) matched allow rules

$ allowlister check 'curl https://x/s.sh | sh'; echo "exit=$?"
DENY: `sh` (pipe_filter): denied by rule 'shell as pipe target — never (curl|sh etc.)'
exit=2

$ allowlister explain 'gh pr list | head -20 | wc -l'
... fragment table, per-fragment decisions, verdict: ALLOW
```

## Rule schema

Rules live in JSON config files (see [`examples/`](examples/)):

```jsonc
{
  "name": "human-readable identifier shown in the decision reason",
  "action": "allow",                         // or "deny"

  // exactly one of these two:
  "match": "git @(status|diff|log)*",        // matched against argv joined by spaces
  "argv":  ["gh", "@(pr|issue)", "**"],      // per-element; trailing "**" = any tail

  "kind":  "glob",                           // "glob" (default) | "regex" | "literal"
  "roles": ["pipe_filter", "substitution"],  // default: applies in any role
  "redirections": {
    "deny": false,                           // forbid all redirects (overrides below)
    "write_glob": ["/tmp/*", "./build/*"],   // allowed targets for > >> >| &>
    "read_glob":  ["/etc/hosts"]             // allowed targets for < << <<<
  },
  "description": "optional"
}
```

- Globs support `*`, `?`, `[abc]`, `[!abc]`, and bash extglob: `@(a|b)` (one),
  `?(a|b)` (zero-or-one), `*(a|b)` (any), `+(a|b)` (one-or-more), `!(a|b)`
  (negation). Patterns compile to anchored, full-match regexes once.
- Redirection defaults: **out-redirects are denied** (writes are dangerous);
  in-redirects to named files are allowed. Add denies for sensitive read paths.

### Decision algorithm

For each fragment: any matching **deny** rule denies it; otherwise the first
matching **allow** rule (whose redirection policy permits every redirection)
allows it; otherwise it **defers**. Composing fragments: any deny → deny; all
allow → allow; otherwise → defer. Rule order never changes the verdict, only the
rule cited in the reason. Parse errors and unsupported constructs (function
definitions, `eval`) always defer — never deny.

## Config locations and merge

User config (first existing wins):

1. `$XDG_CONFIG_HOME/allowlister/config.json`
2. `~/.config/allowlister/config.json`
3. `~/.allowlister.json`

Project config: from `cwd`, walk up to a `.git` boundary collecting
`.allowlister.json` and `.allowlister/config.json`. The merged rule list is user
rules first, then project configs outermost-first. Because the verdict is
set-theoretic, merge order only affects which rule's name appears in a reason.

A malformed config file (or a single malformed rule) is skipped with a recorded
warning; loading never crashes the hook.

## Recommended profiles

Two ready-made, self-contained rulesets — sources in
[`examples/recommended/`](examples/recommended/) — **ship embedded in the
binary**, so you install them by name from anywhere, with no repo checkout, file
path, or network needed. Pick one as a starting point and `install` it; that
merges its rules into your config, creating the file if needed. Re-running is
safe: rules already present (matched by name) are left in place, so you can layer
a profile onto an existing config or upgrade `read-only` to `repo-write` later
without duplicates.

```sh
allowlister install read-only --global   # merge into ~/.config/allowlister/config.json
allowlister install repo-write --local    # or into the current repo's .allowlister.json
```

`install` also accepts a path, for installing an allowlist of your own — for
example one your team keeps in a repo:

```sh
allowlister install ./team-allowlist.json --local
```

**`read-only.json`** — auto-allows pure **read** operations and defers
everything else to the harness (which prompts you). It covers the shell and
coreutils, `git`/`gh` inspection, and read-only commands across the common
language ecosystems: `pip`/`uv`/`python`, `npm`/`pnpm`/`yarn`/`bun`/`node`,
`cargo`/`rustup`, `go`, `poetry`, `make`, and `just`. Anything that writes, runs
project code, or otherwise isn't a pure read **defers** rather than being
denied — so a human still approves it case by case. Output redirection on an
allowed command is blocked (writes are not reads), except scratch under `/tmp`.
A short list of irreversible, system-level actions (recursive `rm`, disk/partition
tools, `curl | sh`, reading private keys) is denied outright.

**`repo-write.json`** — a superset of `read-only` that additionally allows the
writes an agent needs to **manage a repository**: `git add`/`commit`/`branch`/
`switch`/`merge`/`rebase`/`pull`/`push`, `gh pr`/`issue` collaboration, and
`install`/`build`/`test`/`format`/`run` across those same ecosystems. Operations
that look **destructive or irreversible are denied** — force-push, `reset --hard`,
`clean -f`, history rewrite, branch/tag deletion, recursive `rm`, disk wipes, and
registry publishing.

> `repo-write` is permissive by design and is **not a sandbox**. Allowing build,
> test, and run tools (and interpreters like `python`/`node`) means project code
> can execute arbitrary commands the gate never sees. Its denies are guardrails
> against obvious mistakes, not a containment boundary. Use `read-only` when you
> want the agent to look but not touch.

Both files are exercised by `tests/recommended.rs`, which pins their
security-critical verdicts.

## Exit codes

| Command | 0 | 1 | 2 |
|---------|---|---|---|
| `hook` | normal operation (decision on stdout) | malformed stdin payload (non-blocking; agent proceeds) | — |
| `check` | allow / defer | usage/internal error | deny |
| `explain`, `init` | success | error | — |

## Development

Requires [`rustup`](https://rustup.rs) and [`just`](https://just.systems).

```sh
rustup show          # confirm the toolchain in rust-toolchain.toml
just bootstrap       # install dev tools (cargo subcommands) + git hooks
just full-check      # the complete quality gate
```

Common recipes: `just fmt`, `just check`, `just clippy`, `just test`,
`just test-e2e`, `just test-cov`, `just security`, `just deps-check`, `just doc`,
`just build-release`, `just dist-plan`. Run `just` with no arguments to list all.

### Diagnostics policy

Every quality check is **pass/fail, never pass-with-warnings**. Lints run with
`-D warnings`, formatting and coverage misses fail their commands, and
dependency/security/license checks fail on any configured issue. Optional or
aspirational checks stay disabled until they can be enforced as errors — the repo
does not carry a warning backlog. Successful `just` recipes print little;
failures preserve actionable output (paths, lints, diffs, exit codes). Noisy
inspection lives in explicit recipes (`just doctor`, `just cargo-tree`).

### Testing policy

Unit tests cover pure logic; integration tests cover the library and the golden
rule table; **end-to-end tests execute the compiled binary** and assert exit
codes, stdout, stderr, and file effects across the critical user journeys
(`--help`/`--version`, hook allow/deny/defer routing, `check`, `explain`, `init`,
and error/recovery paths). Smoke tests are a subset of E2E coverage, not the
whole strategy. Coverage is enforced with a floor that fails the command on a
miss. These tests are hermetic and network-free, so they run in CI on every
platform.

### Live harness check (opt-in)

The hermetic E2E suite drives the binary through its stdin/stdout hook contract.
To verify the wiring against a *real* harness, `just test-claude` runs
[`scripts/e2e-claude.sh`](scripts/e2e-claude.sh), which drives the actual
`claude` CLI headless with a generated `.claude/settings.json` that registers
`allowlister hook claude-code` as the `Bash` PreToolUse hook, then asserts that a
denied command is blocked (and its reason is reported back to the model) while an
allowed command runs without a prompt:

```sh
just test-claude     # needs Claude Code installed, authenticated, and online
```

Because it needs the `claude` binary, network, and a model call, it is **not**
part of `just full-check` or CI. It skips cleanly (exit 0) when `claude` is not
on `PATH`.

## Releasing

1. Bump `version` in `Cargo.toml` and update `CHANGELOG.md`.
2. `just full-check`.
3. Tag `vX.Y.Z` and push the tag.
4. CI must pass; the release workflow then builds, archives, checksums, and
   uploads binaries for all platforms, and (if configured) publishes to
   crates.io.

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full workflow.

## License

[MIT](LICENSE)

# allowlister

A small, fast Rust CLI that gates the shell commands your AI coding agents run —
**a cross-platform allowlist for every major agent.** You write and maintain
**one allowlist**, and allowlister enforces it identically across Claude Code,
Cursor, GitHub Copilot CLI, OpenAI Codex CLI, Crush, Qwen Code, Goose, and
OpenCode. For each command the agent wants to run it decides **allow**, **deny**,
**ask** (surface for human approval), or **defer** — same engine, same config, no
matter which agent is driving.

It replaces the simplistic string-prefix allow lists that current agents ship
with. Instead of classifying a command as safe or unsafe in the abstract,
allowlister classifies each command by the **structural role** it plays in the
shell expression it appears in.

And it isn't only for shell commands: the same allowlist gates an agent's
**non-shell tool calls** too — built-in tools like file **read**/**write**/**edit**
and **web fetch**, plus any **MCP** tool — all through the same engine and one
portable, param-aware rule language (see
[Gating tool calls](#tool-use-rules-built-in--mcp-tools)). So a single policy can
say "never let any agent *read* `~/.ssh`" or "deny every *delete* MCP tool,"
enforced identically everywhere.

## One allowlist, every agent

allowlister's rule engine is **harness-agnostic**: the same `(argv, role,
redirections)` decision pipeline runs behind every agent. Only the thin
stdin/stdout adapter that speaks each agent's hook protocol differs. So you keep a
single allowlist — one `.allowlister.json` (or user-global config) — and it
governs every agent below.

| Agent | `--harness` | How allowlister gates it | Hook/config it writes |
|-------|-------------|--------------------------|-----------------------|
| Claude Code | `claude-code` | `PreToolUse` hook | `~/.claude/settings.json` |
| Cursor | `cursor` | `beforeShellExecution` hook | `~/.cursor/hooks.json` |
| GitHub Copilot CLI | `copilot` | `preToolUse` hook | `.github/hooks/allowlister.json` |
| OpenAI Codex CLI | `codex` | `PreToolUse` hook | `~/.codex/hooks.json` |
| Crush | `crush` | `PreToolUse` hook | `crush.json` |
| Qwen Code | `qwen` | `PreToolUse` hook | `~/.qwen/settings.json` |
| Goose | `goose` | `PreToolUse` hook (plugin) | `.agents/plugins/allowlister/` |
| OpenCode | `opencode` | `tool.execute.before` plugin shim | `.opencode/plugin/allowlister.js` |

Because the engine is shared, **every allowlister feature works on every agent** —
you never re-learn or rewrite rules per tool:

| Feature | Every agent above |
|---------|:-----------------:|
| Role-aware matching (standalone / pipe / subshell / substitution) | ✅ |
| Whole-command composition (N commands → one verdict) | ✅ |
| Glob, regex, and argv-pattern rules | ✅ |
| Redirection policy (scope out-redirects to allowed paths) | ✅ |
| Recommended profiles (`read-only`, `repo-write`) | ✅ |
| One config, auto-merged from user + project scope | ✅ |
| `allow` / `deny` / `ask` / `defer` verdicts | ✅ |
| Auto-registration via `allowlister init --harness <name>` | ✅ |
| Fail-open on any internal error (never a spurious deny) | ✅ |
| Tool-use gating (built-in + MCP tools), param-aware | ✅* |
| Live end-to-end tested against the real CLI | ✅ |

Each adapter is verified end-to-end against the real agent (see
[Live harness check](#live-harness-check-opt-in)) — including, for every agent, a
live built-in-tool deny and an MCP-tool deny, not just shell commands.

`*` **Tool-use coverage depends on what each harness exposes to its hook.** The
rule *language* is identical everywhere; *enforcement* is bounded by which tool
classes a given agent lets allowlister see before they run:

| Agent | shell | file read | file write/edit | web fetch | MCP |
|-------|:-----:|:---------:|:---------------:|:---------:|:---:|
| Claude Code | ✅ | ✅ | ✅ | ✅ | ✅ |
| Cursor | ✅ | ✅ | ❌¹ | via MCP | ✅ |
| GitHub Copilot CLI | ✅ | ✅ | ✅ | ✅ | ✅ |
| OpenAI Codex CLI | ✅ | ❌² | ❌² | via MCP | ✅ |
| Crush | ✅ | ✅ | ✅ | ✅ | ✅ |
| Qwen Code | ✅ | ✅ | ✅ | ✅ | ✅ |
| Goose | ✅ | ✅ | ✅ | via MCP | ✅ |
| OpenCode | ✅ | ✅ | ✅ | ✅ | ✅ |

A class shown as ❌ or *via MCP* isn't wrongly allowed — allowlister never sees it
before it runs, so it falls through to the agent's own flow (defer). "via MCP"
means the agent reaches that capability through an MCP server, which MCP rules
gate. Footnotes: **¹** Cursor has no pre-execution write/edit hook (only read,
MCP, and shell). **²** Codex exposes no built-in read to its hook (reads go via
the shell) and its writes arrive as `apply_patch` patch strings with no discrete
path; gate Codex file access with shell and MCP rules.

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

### With asdf

If you use [asdf](https://asdf-vm.com), the
[`asdf-allowlister`](https://github.com/nickderobertis/asdf-allowlister) plugin
installs the same prebuilt release binaries and manages the version through your
`.tool-versions`:

```sh
asdf plugin add allowlister https://github.com/nickderobertis/asdf-allowlister
asdf install allowlister latest
asdf set allowlister latest          # or pin a specific version, e.g. 0.1.0
```

Linux and macOS only (`x86_64`/`arm64`); see the plugin's README for version
pinning and troubleshooting.

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

## Quick start

```sh
allowlister init            # interactive setup on a terminal
```

Run on a terminal, `init` walks you through a short setup: where the config
lives (user-global or project-local), which starting ruleset to write (the
minimal `starter`, or a curated [recommended profile](#recommended-profiles) —
`read-only` / `repo-write`), and whether to register the harness hook now. It
then writes the config and — by default — **registers the hook for you**. The
config itself is harness-agnostic; only the hook wiring differs per harness, so
`--harness` picks which one to set up (default `claude-code`).

For **Claude Code** it merges the hook into `~/.claude/settings.json` (or
`./.claude/settings.json` for a project). The merge is non-destructive and
idempotent: it preserves your other settings, never duplicates the hook, and adds
a tiny nuclear-pattern `permissions.deny` as defense in depth.
`permissions.allow` / `permissions.ask` are left untouched so the hook stays the
source of allow truth.

For **Cursor** (`--harness cursor`) it merges the `beforeShellExecution` hook
into `~/.cursor/hooks.json` (or `./.cursor/hooks.json` for a project), just as
idempotently. Cursor has no allow list to broaden, so there is no allow-list
warning — the hook is the sole gate.

For every other agent — **GitHub Copilot CLI** (`--harness copilot`), the **OpenAI
Codex CLI** (`codex`), **Crush** (`crush`), **Qwen Code** (`qwen`), **Goose**
(`goose`), and **OpenCode** (`opencode`) — `init` writes that agent's hook into the
location shown in the [supported-harnesses table](#one-allowlist-every-agent),
idempotently. None has an allow list to broaden, so the hook is the sole gate: a
`deny` blocks a command even when the agent runs unattended (e.g. Copilot's
`--allow-all-tools` or `GOOSE_MODE=auto`).

Every choice is also a flag, so the same command scripts cleanly in CI:

```sh
allowlister init --global --profile read-only   # user config, curated reads, Claude hook registered
allowlister init --local  --harness cursor      # project config, Cursor hook registered
allowlister init --local  --harness goose       # project config, Goose hook registered
# …also: copilot, codex, crush, qwen, opencode
allowlister init --local  --no-hooks            # print the snippet instead of registering
allowlister init -y                              # take all defaults, no prompts
```

`--no-hooks` falls back to printing the exact settings snippet for the chosen
harness to paste by hand. Run `init` once per harness to wire more than one; the
config is written on the first run, so the second run needs `--force` to re-write
it alongside the new hook (e.g. `allowlister init --local --harness cursor
--force` after a Claude Code setup). To layer another profile onto an existing
config later, use [`install`](#recommended-profiles).

> **Claude Code only:** do **not** add `"Bash"` or `"Bash(*)"` to
> `permissions.allow`. A broad allow makes the agent skip its prompt on its own,
> which short-circuits the hook's per-fragment allow analysis — the entire point
> of allowlister.

## Usage

```text
allowlister hook <harness>        Read hook JSON on stdin, write a decision on stdout.
                                  Harnesses: claude-code, cursor, copilot, codex,
                                  crush, qwen, goose, opencode.
allowlister check '<cmd>' [--cwd P] [--json]
                                  Evaluate one command. Exit 0 for allow/defer, 2 for deny.
allowlister check --tool <name> [--param key=value]... [--raw JSON] [--cwd P]
                                  Evaluate one tool call instead of a command, e.g.
                                  `check --tool read --param path=~/.ssh/id_rsa`.
allowlister explain '<cmd>' [--cwd P]
                                  Verbose trace: config sources, fragments, per-fragment
                                  decision, and overall verdict. The primary debugging tool.
allowlister init [--global | --local] [--profile SOURCE]
                 [--harness claude-code|cursor|copilot|codex|crush|qwen|goose|opencode]
                 [--hooks | --no-hooks] [-i | -y] [--force]
                                  Set up: write a config from a ruleset (starter,
                                  read-only, repo-write, or a file) and register
                                  the hook in the chosen harness's settings (the
                                  per-harness location is listed in the supported-
                                  harnesses table). Interactive on a terminal;
                                  flags drive it in CI.
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

$ allowlister check --tool read --param path=/home/me/.ssh/id_rsa; echo "exit=$?"
DENY: tool `read` denied by rule 'never read secrets'
exit=2

$ allowlister explain 'gh pr list | head -20 | wc -l'
... fragment table, per-fragment decisions, verdict: ALLOW
```

## Rule schema

Rules live in JSON config files (see [`examples/`](examples/)). `//` line and
`/* */` block comments are allowed, so a config can document itself:

```jsonc
{
  "name": "human-readable identifier shown in the decision reason",
  "action": "allow",                         // or "deny", or "ask" (surface for approval)

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

### Tool-use rules (built-in + MCP tools)

A rule gates a **non-shell tool call** when it sets `tool` instead of
`match`/`argv`. It runs through the same engine, the same glob matcher, and the
same allow/deny/defer composition as shell rules:

```jsonc
{
  "name": "never read secrets",
  "tool": "read",                 // a capability, "mcp", or a raw tool-name glob
  "action": "deny",
  "params": { "path": ["**/.ssh/**", "**/*.pem", "**/.env"] }
}
```

allowlister normalizes each agent's native tool names and parameter keys to one
canonical vocabulary, so **a single rule is portable across every agent**: `read`
matches Claude's `Read`, Qwen's `read_file`, Crush/Copilot's `view`, OpenCode's
`read`, Goose's `text_editor` (view), and Cursor's read event; `path` matches
whichever key each uses underneath (`file_path` / `path` / `filePath`).

- **`tool`** is one of the capabilities `read`, `write`, `edit`, `glob`, `grep`,
  `web_fetch`, `web_search`, `mcp` — or a **raw tool-name glob** (e.g.
  `"mcp__github__*"`) for matching by the harness's literal tool name.
- **`params`** maps a canonical key to one or more globs (any-of). Keys: `path`,
  `url`, `query`, `pattern`, `content`, and `mcp_server` / `mcp_tool` for MCP.
- **`jsonpath`** matches an MCP server's *own* (non-portable) parameters by an
  explicit dotted path into the raw tool input, since allowlister can't know a
  third-party server's argument names in advance.

```jsonc
// allow web fetches only to GitHub
{ "name": "web fetch github only", "tool": "web_fetch", "action": "allow",
  "params": { "url": ["https://github.com/**", "https://*.github.com/**"] } }

// ONE portable MCP rule — matches every agent's wire format (mcp__s__t, mcp_s_t,
// s_t, s(t), ext__t) because the server/tool names are normalized first
{ "name": "deny destructive MCP tools", "tool": "mcp", "action": "deny",
  "params": { "mcp_tool": ["delete*", "*destroy*"] } }

// deny a whole capability regardless of params
{ "name": "no web search", "tool": "web_search", "action": "deny" }

// gate an MCP server's own parameter by JSON path
{ "name": "deploy to staging only", "tool": "mcp", "action": "allow",
  "params":   { "mcp_tool": ["deploy"] },
  "jsonpath": { "target": ["staging/**"] } }
```

A complete, copy-pasteable set is in
[`examples/tool-rules.json`](examples/tool-rules.json).

Matching: the rule applies when its selector matches **and** every listed param
matches (any-of within a param, AND across params). A **missing** param means the
rule does not match — so a call that lacks the param **defers** rather than being
allowed or denied; for a blanket deny of a whole capability, omit `params`. Path
params reject `..` traversal. A bash rule and a tool rule are mutually exclusive
(a `tool` rule must not set `match`/`argv`/`roles`/`redirections`, and vice
versa); a rule that mixes them is skipped with a warning. Which tool classes each
agent actually exposes is in the
[tool-use support matrix](#one-allowlist-every-agent) above.

### Decision algorithm

For each fragment the precedence is **deny > ask > allow**: any matching **deny**
rule denies it; otherwise any matching **ask** rule surfaces it for approval;
otherwise the first matching **allow** rule (whose redirection policy permits
every redirection) allows it; otherwise it **defers**. Composing fragments: any
deny → deny; else any ask → ask; else all allow → allow; otherwise → defer. Rule
order never changes the verdict, only the rule cited in the reason. Parse errors
and unsupported constructs (function definitions, `eval`) always defer — never
deny.

Because `ask` outranks `allow`, an ask rule carves a "confirm first" hole out of
a broad allow without narrowing it: keep `git push?( *)` allowed, add
`ask: git push *--force*`, and ordinary pushes allow while a force-push surfaces a
prompt. Use it for operations that are dangerous but sometimes legitimate, where a
hard `deny` (which a user/project overlay cannot override) would over-block.

Tool-use rules use the same composition: for a tool call, any matching **deny**
wins, else any matching **ask**, else the first matching **allow**, else
**defer**. An unrecognized tool, or one no rule matches, defers — so adding tool
rules never changes behavior until a rule actually matches.

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

Both profiles sort operations into three tiers (see
[`examples/AGENTS.md`](examples/AGENTS.md)): **deny** the never-legitimate core
(reading private keys/credentials, disk/partition wipes, raw block-device writes,
recursive `chmod`/`chown` on absolute/home paths), **ask** for the
dangerous-but-sometimes-legitimate, and **defer** everything unclassified. The
deny set is kept deliberately tiny because a deny can't be overridden in a
user/project overlay; anything a real workflow might need is an `ask`, not a deny.

**`read-only.json`** — auto-allows pure **read** operations. It covers the shell
and coreutils, `git`/`gh` inspection, and read-only commands across the common
language ecosystems: `pip`/`uv`/`python`, `npm`/`pnpm`/`yarn`/`bun`/`node`,
`cargo`/`rustup`, `go`, `poetry`, `make`, and `just`. Anything that writes or runs
project code **defers** to the harness (which prompts you). Risky shortcuts that
an allowed reader could otherwise sneak through — `curl | sh`, recursive `rm`,
in-place edits, `sort -o`, branch/tag deletion, raw `gh api` writes — **ask** for
confirmation. Output redirection on an allowed command is blocked (writes are not
reads), except scratch under `/tmp`.

**`repo-write.json`** — a superset of `read-only` that additionally allows the
writes an agent needs to **manage a repository**: `git add`/`commit`/`branch`/
`switch`/`merge`/`rebase`/`pull`/`push`, `gh pr`/`issue` collaboration, and
`install`/`build`/`test`/`format`/`run` across those same ecosystems. Destructive
or irreversible operations **ask** rather than hard-denying — force-push,
`reset --hard`, `clean -f`, history rewrite, branch/tag deletion, recursive `rm`,
registry publishing, `gh repo delete`, self-uninstall — so a release or
maintenance agent can still do them with a human in the loop.

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
| `check` | allow / defer / ask | usage/internal error | deny |
| `explain`, `init` | success | error | — |

The `hook` verb never denies on its own error, but *how* it signals that differs
by harness. For Claude Code and Cursor a non-zero exit fails *open* (the agent
proceeds), so a malformed payload exits `1`. Codex's `PreToolUse` and Copilot's
`preToolUse` are the opposite — fail-*closed*, where a non-zero exit would deny —
so those adapters **always exit `0`** and carry a deny only in their decision JSON;
a malformed payload exits `0` with empty stdout, which the harness reads as "no
decision" and falls through to its normal flow. A deny is never expressed via the
exit code.

An **ask** verdict maps to the harness's own approval prompt where one exists
(Claude Code, Cursor, Copilot emit an `ask` decision); on harnesses that honor
only a deny it degrades to the same no-decision fall-through as `defer` — a
prompt, never a silent allow and never a hard block. For the `check` verb, `ask`
exits `0` (like `defer`); its verdict shows in the printed output.

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
To verify the wiring against a *real* harness, one opt-in script per agent sets a
sandbox project up with `allowlister init` — exercising the real hook-registration
path — then drives the actual agent headless and asserts that denied work is
blocked while allowed work runs. Each script covers all three surfaces: a denied
**shell** command, a denied **built-in tool** (a `read` of a planted secret, or a
`write` where the agent has no gateable read), and a denied **MCP** tool (a
destructive call routed through a bundled stdio MCP server fixture):

```sh
just test-claude     # drives `claude`;       writes/reads .claude/settings.json
just test-cursor     # drives `cursor-agent`; writes/reads .cursor/hooks.json
just test-copilot    # drives `copilot`;      writes/reads .github/hooks/allowlister.json
just test-codex      # drives `codex`;        writes/reads .codex/hooks.json
just test-crush      # drives `crush`;        writes/reads crush.json
just test-qwen       # drives `qwen`;         writes/reads ~/.qwen/settings.json
just test-goose      # drives `goose`;        writes/reads .agents/plugins/allowlister/
just test-opencode   # drives `opencode`;     writes/reads .opencode/plugin/allowlister.js
```

Each needs its harness binary, network, and a model call, so none is part of
`just full-check` or CI. All skip cleanly (exit 0) when their CLI is not on
`PATH`. Every check additionally proves the `deny` holds even when the agent is
running fully autonomous (no human approver) — the hook is consulted before the
agent's own permission flow, so allowlister is the authoritative gate.

## Releasing

Releases are automated from [Conventional Commits](https://www.conventionalcommits.org)
by [release-plz](https://release-plz.dev). You never bump the version or tag by
hand:

1. Land changes on `main` with conventional commit messages. The type drives the
   bump — and **pre-1.0 the minor slot acts as the major**, per Cargo's 0.x
   rules:

   | Commit type | Pre-1.0 | ≥1.0 |
   | --- | --- | --- |
   | `fix:` / `perf:` | patch (`0.1.0`→`0.1.1`) | patch |
   | `feat:` | patch (`0.1.0`→`0.1.1`) | minor |
   | `feat!:` / `BREAKING CHANGE:` | **minor** (`0.1.1`→`0.2.0`) | major |
   | `docs` / `test` / `chore` / `ci` | no release | no release |

   So before 1.0, cut a feature-milestone (minor) release with `feat!:` — a plain
   `feat:` is only a patch. There is no setting to change this; it's how Cargo
   semver treats `0.x`.
2. The [`release-plz`](.github/workflows/release-plz.yml) workflow opens a
   **release PR** that bumps `Cargo.toml` + `Cargo.lock` and writes the
   `CHANGELOG.md` section from those commits.
3. Merge the release PR. release-plz then tags `vX.Y.Z` and cuts the GitHub
   Release; that triggers [`release.yml`](.github/workflows/release.yml), which
   gates on a passing test run and then builds, archives, checksums, and uploads
   the cross-platform binaries.

Because this project is not published to crates.io, the workflow feeds release-plz
the previous release tag as its version baseline (`--registry-manifest-path`), so
the bump is computed from git history alone. crates.io publishing stays a
separate, opt-in step: set the `PUBLISH_TO_CRATES_IO` repo variable and provide a
`CARGO_REGISTRY_TOKEN` secret. release-plz authenticates with a PAT
(`RELEASE_PLZ_TOKEN`) so the release it creates is allowed to trigger the binary
build.

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full workflow.

## License

[MIT](LICENSE)

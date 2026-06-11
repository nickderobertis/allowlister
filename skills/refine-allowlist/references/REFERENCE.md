# Reference — allowlister rule schema & CLI contract

Detail the `refine-allowlist` skill loads on demand. Authoritative source is the
allowlister docs (`README.md`, `src/config.rs`); this mirrors what the skill needs.

## Verdict model

`deny > ask > allow > defer`. Any matching `deny` denies; absent a deny, any matching `ask`
prompts; absent both, any matching `allow` allows; otherwise the call **defers** to the
harness's own prompt. Merge order (user config first, then project configs) only changes
which rule's *name* is cited, never the verdict. So a refined allowlist is a set, not an
ordered list — pick clear names and let the set-theoretic verdict do the rest.

## Config locations

- Global (user): `$XDG_CONFIG_HOME/allowlister/config.jsonc`, else `~/.config/allowlister/config.jsonc`
  (legacy `~/.allowlister.jsonc` in the home directory is also read). The `.json` spelling of
  each is supported too; `.jsonc` wins when both exist.
- Local (project): `.allowlister.jsonc` (or `.allowlister/config.jsonc`, or their `.json`
  twins), discovered by walking up from the cwd to the repo root.

A config is `{ "rules": [ … ], "history": { "enabled": <bool> } }`. JSONC comments are allowed.

## Rule fields

Bash/shell rule (use `match` **or** `argv`, not both):

| Field | Meaning |
| --- | --- |
| `name` | Short unique label; cited in diagnostics; **keys the idempotent merge**. |
| `action` | `allow` \| `deny` \| `ask` (default `allow`). |
| `match` | Pattern against the joined subcommand string, e.g. `"git status*"`. |
| `argv` | Pattern matched element-by-element, e.g. `["gh", "@(pr|issue)", "**"]`. |
| `kind` | `glob` (default) \| `regex` \| `literal` — how `match`/`argv` is interpreted. |
| `roles` | Restrict to structural roles: `standalone`, `pipe_source`, `pipe_filter`, `subshell`, `substitution`. |
| `redirections` | `{ "deny": bool, "write_glob": [...], "read_glob": [...] }` — gate file redirection targets. |
| `grants` | `command` (default; authorizes the command) \| `redirections` (only widens redirection targets). |
| `description` | Human note. |

Tool (non-shell) rule — mutually exclusive with the bash fields:

| Field | Meaning |
| --- | --- |
| `tool` | A capability (`read`/`write`/`edit`/`glob`/`grep`/`web_fetch`/`web_search`/`mcp`) or a raw tool-name glob (e.g. `mcp__linear__@(list|get)*`). |
| `params` | Canonical params to glob-match (`path`, `url`, `query`, `pattern`, `content`, `mcp_server`, `mcp_tool`); all AND-ed. |
| `jsonpath` | Glob-match server-defined fields by JSON path. |

Glob syntax is extended-glob: `*`, `**` (tail, in `argv`), and `@(a|b)` alternation.

### Examples

```jsonc
{ "name": "cargo test",       "match": "cargo test*",  "action": "allow" }
{ "name": "git read-only",    "match": "git @(status|log|diff|show)*", "action": "allow" }
{ "name": "grep as filter",   "match": "grep *", "action": "allow", "roles": ["pipe_filter"] }
{ "name": "echo scratch",     "match": "echo *", "action": "allow",
  "redirections": { "write_glob": ["/tmp/*", "./build/*"] } }
{ "name": "no rm -rf",        "match": "rm -rf*", "action": "deny" }
{ "name": "confirm publish",  "match": "npm publish*", "action": "ask" }
{ "name": "no secret reads",  "tool": "read", "action": "deny",
  "params": { "path": ["**/.ssh/**", "**/*.pem", "**/.env", "**/.env.*"] } }
```

## CLI contract the skill drives

```bash
allowlister history path                                              # store location
allowlister history --json                                           # full summary (has events_total)
allowlister history --view fragments --verdict <V> --top <N> --json  # V ∈ allow|ask|deny|defer
allowlister history recent --json                                    # recent raw events (with project tag)
allowlister install <file.json> --global                            # merge into user config
allowlister install <file.json> --local                             # merge into ./.allowlister.json
allowlister install <file.json> --output <path>                     # merge into an explicit path
allowlister explain "<command>"                                     # sources + per-fragment decision
allowlister check "<command>"                                       # exit 0 allow/defer, 2 deny
allowlister check --json "<command>"                                # machine-readable verdict
```

### `history … --json` summary shape (default / `--view` reports)

```jsonc
{
  "events_total": 123,
  "first_ts": 1680000000, "last_ts": 1680086400,
  "as_of": 1680090000,                  // report time every `recent` weight is decayed to
  "overall": { "allow": 95, "deny": 10, "ask": 5, "defer": 13, "first_ts": 1680000000, "last_ts": 1680086400 },
  "view": "fragments", "verdict": "defer", "truncated": false,
  "rows": [
    { "key": "cargo test", "total": 12,
      "allow": 0, "ask": 0, "deny": 0, "defer": 12,
      "first_ts": 1679000000, "last_ts": 1680086400,   // first / latest use (Unix seconds)
      "recent": { "defer": 9.1 },       // per-verdict recency weight (zeros omitted)
      "recent_total": 9.1,
      "rules": {} }
  ]
}
```

Recency semantics: each `recent` weight is the sum of `0.5^(age / 30 days)` over that
verdict's events, decayed to `as_of` — steady current use scores near its monthly volume,
while a burst that ended months ago decays toward `0` (and the `recent` field disappears
entirely once fully decayed). Use `recent`/`recent_total` vs the raw counts, plus
`as_of - last_ts`, to tell live candidates from heavy-but-stale ones.

`history recent --json` is instead an array of events, each:
`{ "ts", "harness", "project", "kind", "command", "verdict", "fragments": [ { "cmd", "role", "verdict", "rule? } ] }`.

## Apply via `install`, not by hand

`allowlister install` validates the incoming rules, merges by `name` (idempotent, never
duplicates), and preserves existing rules and top-level keys. It prints `N added, M already
present`. Editing config JSON directly bypasses validation and risks duplicate or malformed
rules — don't.

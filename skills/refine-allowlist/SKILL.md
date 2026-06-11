---
name: refine-allowlist
description: Refine an allowlister allowlist from recorded usage history. Use when the user wants to tune, refine, tighten, or expand their allowlister rules, asks "what should I allow/deny", wants to reduce permission prompts, or wants to turn `allowlister history` into config changes. Reads the recorded history, proposes allow rules for safe deferred commands, allow rules for noisy asks, and deny/ask rules for risky ones, lets the user edit the plan, then applies the approved rules to the global or local config via `allowlister install` and verifies with `explain`/`check`. Requires the allowlister CLI with history recording enabled.
license: MIT
compatibility: Requires the `allowlister` CLI on PATH with usage-history recording enabled (`allowlister init --history` or `ALLOWLISTER_HISTORY=1`). Cross-platform.
allowed-tools: Bash Read Edit Write
---

# Refine an allowlister allowlist from usage history

Turn recorded usage into concrete rule changes: propose, let the user edit, apply, verify.
You never edit config JSON by hand and you never silently widen access — risky commands
are proposed as `deny`/`ask`, never `allow`. The verdict model is **deny > ask > allow >
defer**, and merge order never changes a verdict (`references/REFERENCE.md`).

## When to use

The user asks to refine/tune/tighten/expand their allowlist, asks "what should I allow?",
wants fewer permission prompts, or wants to act on `allowlister history`. Skip if there is
no recorded history yet (see preconditions) — there is nothing to learn from.

## Preconditions (check first, stop early if unmet)

1. CLI present: `allowlister --version`. If missing, tell the user how to install it and stop.
2. History exists and is non-empty:
   ```bash
   allowlister history path        # where the store lives
   allowlister history --json      # full summary
   ```
   If the JSON's `events_total` is `0` (or the command reports no history), recording is
   off or empty. Tell the user to enable it (`allowlister init --history`, or set
   `ALLOWLISTER_HISTORY=1` for their harness) and come back after real usage accrues. Stop.

## Step 1 — Gather the evidence

Pull the three verdict slices as JSON and keep them for analysis. `fragments` is the unit a
rule matches (per parsed subcommand), so refine on it.

```bash
allowlister history --view fragments --verdict defer --top 50 --json   # candidates to ALLOW
allowlister history --view fragments --verdict ask   --top 50 --json   # noisy prompts to maybe ALLOW
allowlister history --view fragments --verdict deny  --top 50 --json   # already blocked (context)
```

Each row has `key` (the subcommand), `total`, per-verdict counts (`allow`/`ask`/`deny`/`defer`),
`rules` (which named rule decided it), and its time shape: `first_ts`/`last_ts` (Unix seconds
of first/latest use), `recent` (per-verdict recency weight, 30-day half-life, decayed to the
report's `as_of`), and `recent_total`. **Weigh recency, not just totals**: a key whose
`recent` is near zero (or missing — fully decayed) was a burst of past use that stopped; raw
counts alone overstate it. Compare `recent_total` to `total` and `as_of - last_ts` to judge
whether a candidate is still live. For project context use `--view commands`, and
`allowlister history recent --json` to see real invocations and their `project` tag.

## Step 2 — Read the current config (avoid duplicates)

Find and read both configs so you neither duplicate an existing rule `name` nor propose
something already covered:

- Global (user): `~/.config/allowlister/config.json` (or `$XDG_CONFIG_HOME/allowlister/config.json`).
- Local (project): `.allowlister.json` walking up to the repo root.

Note the existing rule names — `install` merges idempotently **by name**, so reusing a name
updates in place and a new name adds.

## Step 3 — Classify into proposed rules (broad)

For each frequent `key`, decide an action and a target. Bias toward **specific** globs over
broad ones, and toward **fewer, legible** rules.

| Signal | Proposal |
| --- | --- |
| Frequent **and recent** `defer` (healthy `recent.defer`), clearly safe (read-only / build / test / VCS-read) | `allow` |
| Frequent `ask` with recent activity, clearly safe and only prompting out of caution | `allow` (cut the prompt) |
| Heavy counts but stale (`recent` ≈ 0, last use months ago) | skip, or mention as low priority — past bursts don't justify widening access now |
| Risky: `rm -rf`, `curl … \| sh`, `chmod 777`, `sudo`, secret/key reads, writes outside the repo, history rewrite, force-push | `deny` (hard) or `ask` (sometimes-legit) — **never `allow`** (recency does not soften this) |
| Already decided by a matching rule | skip (don't restate) |

**Target (global vs local):**
- Global for tools that are safe everywhere and recur across projects (`ls`, `git status`,
  `cat`, `rg`). Use the `project` tag spread in `history recent` to judge generality.
- Local (`.allowlister.json`) for project-specific commands (a repo's `just` recipes, its
  task runner, its scripts). When unsure, prefer **local** — narrower blast radius.

**Naming & shape:** give every rule a short, unique, descriptive `name` (it is cited in
diagnostics and keys the idempotent merge). Match on the joined subcommand with a glob, e.g.
`{ "name": "cargo test", "match": "cargo test*", "action": "allow" }`. Add `roles`,
`redirections`, or use `argv`/`kind` only when the evidence calls for it. Rule fields and
examples: `references/REFERENCE.md`.

## Step 4 — Present the plan and let the user edit

Show the proposal as a grouped, scannable table — by **action** then **target** — with the
evidence and a one-line rationale per rule:

```
GLOBAL — allow
  cargo test      (defer ×12, recent 9.1, last 2d)   safe test runner, in active use
  git status      (defer ×9,  recent 7.4, last 1d)   read-only VCS
LOCAL (.allowlister.json) — allow
  just check      (ask ×6, recent 4.2, last 3d)      project gate, always confirmed
LOCAL — deny
  rm -rf *        (defer ×2, last 1d)                destructive; block outright
SKIPPED — stale
  npm run build   (defer ×40, recent 0, last 4mo)    heavy past use, dead since
```

Explicitly invite the user to **add, remove, retarget (global↔local), loosen, or tighten**
any rule, and to change any action. Iterate on the plan until they confirm. Do not apply
anything before explicit confirmation — this review loop is the point of the skill.

## Step 5 — Apply the approved rules

Apply through `allowlister install`, never by editing config JSON directly — it validates,
merges idempotently by name, and never duplicates. Build one ruleset file per target and
install it:

```bash
# Write only the approved rules for a target into a temp file, e.g. /tmp/refine-global.json:
# { "rules": [ { "name": "...", "match": "...", "action": "allow" }, ... ] }

allowlister install /tmp/refine-global.json --global   # → ~/.config/allowlister/config.json
allowlister install /tmp/refine-local.json  --local    # → ./.allowlister.json
```

`install` prints how many rules were added vs already present. Re-running is a no-op for
rules already there.

## Step 6 — Verify

Confirm the new rules actually changed the verdicts, using representative commands from the
history:

```bash
allowlister explain "cargo test"        # config sources + per-fragment decision
allowlister check "cargo test"          # exit 0 allow/defer, 2 deny
allowlister check --json "rm -rf /tmp/x"
```

Show before→after for a couple of cases (e.g. `defer → allow`, risky `defer → deny`).

## Step 7 — Report

Summarize what landed where (global vs local, counts by action), note anything you
deliberately left as `defer`/`deny`, and suggest re-running after more usage accumulates so
the allowlist keeps converging on real behavior.

## Guardrails (hard rules)

- Never propose `allow` for destructive, credential-exposing, or network-piped-to-shell
  commands. Route them to `deny`/`ask`.
- Stay within the evidence: don't widen a pattern beyond what the history shows, and don't
  let stale counts stand in for current need — an `allow` needs recent activity behind it.
- Don't restate rules that already decide a command; don't reuse a name unless you intend to
  overwrite that rule.
- Apply only after explicit user confirmation, and only via `allowlister install`.

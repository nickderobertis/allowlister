# AGENTS — examples

These profiles are advice users copy and run. Maintain them by the verdict tiers
below, not case by case. The `recommended/` profiles are also embedded in the
binary and pinned by `tests/recommended.rs`; a tier change must update those pins.

## Verdict tiers

- **deny** — only operations with no legitimate use in any agent workflow, where
  the failure mode is host destruction or secret exfiltration. A deny cannot be
  overridden in a user or project overlay, so it is a hard wall with no escape
  hatch: keep this set tiny. It is currently just reading private
  keys/credentials, disk and partition wipes, writing a raw block device, and
  recursive permission changes on absolute or home paths.
- **ask** — dangerous but sometimes legitimate. Surfaces the command for human
  approval instead of blocking it. An ask outranks a broad allow, so it carves a
  "confirm first" hole out of a permissive rule *without narrowing that rule* —
  use it for the risky variant (force-push, history rewrite, registry publish,
  recursive delete, raw API writes, network-piped installs) while the safe forms
  keep allowing.
- **allow** — auto-approved reads and the ordinary writes a profile commits to.
- **(defer)** — no rule at all; the command falls through to the harness's own
  prompt. Leave genuinely unclassified operations unconfigured rather than guess.

## Rules of thumb

- Reach for deny only when a prompt would be the wrong answer. If a human could
  ever reasonably approve it, it is an ask, not a deny.
- An ask rule and the broad allow it sits under are a pair: the allow grants the
  family, the ask holds back the variant. Do not "fix" an ask by narrowing its
  allow — that reintroduces the silent-allow gap the ask exists to close.
- Adding a deny is a decision made for every user with no opt-out; give it the
  same scrutiny as removing a safety guard.
- Comments (`//` and `/* */`) are accepted in config files. Use them to record
  why a rule is an ask versus a deny, not to log how the file changed.

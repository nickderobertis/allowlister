#!/usr/bin/env bash
#
# End-to-end check for the `refine-allowlist` agent skill: install it the way a
# user does (via `gh skill`), validate it against the Agent Skills spec, and
# exercise the exact allowlister CLI surface the skill depends on.
#
# This is deliberately NOT part of `just full-check`: it needs the `gh skill`
# command (GitHub CLI 2.93+, preview). It IS hermetic and deterministic, though
# — `gh skill install --from-local` and `gh skill publish --dry-run` are local,
# and the CLI smoke test runs against an isolated HOME with temp configs — so it
# runs in its own CI job (.github/workflows/skill-install.yml), triggered when
# the skill or the binary changes. Run it by hand with `just verify-skill`.
#
# Why the binary, not just the skill: the skill drives a CLI contract
# (`history --json`, `install --output/--global/--local`, `explain`, `check`).
# This asserts that contract still holds, so a change to the CLI that would
# break the skill fails here.
#
# Environment overrides:
#   ALLOWLISTER_BIN   path to the allowlister binary (else target/{release,debug}, else build)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SKILL_NAME="refine-allowlist"
SKILL_DIR="skills/${SKILL_NAME}"

fail() { printf '✗ %s\n' "$1" >&2; exit 1; }
ok()   { printf '✓ %s\n' "$1"; }

# --- Prerequisites -----------------------------------------------------------

command -v python3 >/dev/null 2>&1 || fail "python3 is required for this check."
if ! command -v gh >/dev/null 2>&1 || ! gh skill --help >/dev/null 2>&1; then
  fail "the 'gh skill' command is required (GitHub CLI 2.93+, preview).
  Install/upgrade gh from https://cli.github.com, then re-run 'just verify-skill'."
fi

# Resolve the binary: explicit override, then a built target, else build it.
BIN="${ALLOWLISTER_BIN:-}"
if [ -z "$BIN" ]; then
  if [ -x "target/release/allowlister" ]; then BIN="$ROOT/target/release/allowlister"
  elif [ -x "target/debug/allowlister" ]; then BIN="$ROOT/target/debug/allowlister"
  else
    echo "building allowlister (debug) …"
    cargo build --locked --quiet
    BIN="$ROOT/target/debug/allowlister"
  fi
fi
[ -x "$BIN" ] || fail "allowlister binary not found or not executable: $BIN"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# --- 1. Install the skill via gh, from local source (offline, as users do) ---

gh skill install "$ROOT" "$SKILL_NAME" --from-local --dir "$TMP/installed" -f >/dev/null 2>&1 \
  || fail "gh skill install --from-local failed."
INSTALLED="$TMP/installed/${SKILL_NAME}/SKILL.md"
[ -f "$INSTALLED" ] || fail "installed SKILL.md not found at $INSTALLED"
ok "gh skill install --from-local placed ${SKILL_NAME}/SKILL.md"

# --- 2. Validate the source skill's frontmatter against the spec -------------

python3 - "$ROOT/$SKILL_DIR/SKILL.md" "$SKILL_NAME" <<'PY' || fail "frontmatter validation failed."
import re, sys
path, expected = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
m = re.match(r"^---\n(.*?)\n---\n", text, re.S)
assert m, "missing YAML frontmatter"
fm = m.group(1)
def field(name):
    mm = re.search(rf"^{name}:\s*(.+?)\s*$", fm, re.M)
    return mm.group(1).strip() if mm else None
name = field("name")
desc = field("description")
assert name == expected, f"name '{name}' must equal directory name '{expected}'"
assert re.fullmatch(r"[a-z0-9]+(-[a-z0-9]+)*", name), f"name '{name}' breaks agentskills naming rules"
assert len(name) <= 64, "name exceeds 64 chars"
assert desc and len(desc) >= 1, "description is required and non-empty"
assert len(desc) <= 1024, "description exceeds 1024 chars"
print("  name + description conform to the Agent Skills spec")
PY
ok "source frontmatter conforms to the Agent Skills spec"

# --- 3. Spec validation via gh skill publish --dry-run -----------------------

PUB_OUT="$(gh skill publish --dry-run 2>&1)" || fail "gh skill publish --dry-run errored:
$PUB_OUT"
if printf '%s\n' "$PUB_OUT" | grep -qiE '^[[:space:]]*error'; then
  fail "gh skill publish --dry-run reported errors:
$PUB_OUT"
fi
ok "gh skill publish --dry-run validates the skill (no errors)"

# --- 4. Binary contract: the CLI surface the skill drives --------------------
# Isolated HOME/config so the real user history and config are never touched.
export HOME="$TMP/home"
export XDG_CONFIG_HOME="$TMP/home/.config"
mkdir -p "$XDG_CONFIG_HOME" "$TMP/empty"

# history summary --json → object with events_total and the recency anchor as_of
"$BIN" history --json > "$TMP/hist.json" 2>/dev/null || fail "history --json exited non-zero"
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert isinstance(d,dict) and "events_total" in d and "as_of" in d' "$TMP/hist.json" \
  || fail "history --json is not the expected summary object (events_total + as_of)"
ok "history --json returns the summary the skill reads"

# history fragments/defer --json → object with a rows array (the skill's candidates)
"$BIN" history --view fragments --verdict defer --top 5 --json > "$TMP/defer.json" 2>/dev/null \
  || fail "history --view fragments --verdict defer --json exited non-zero"
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); assert isinstance(d.get("rows"),list)' "$TMP/defer.json" \
  || fail "history defer --json has no rows array"
ok "history --verdict defer --json returns rows the skill classifies"

# history path → prints a location
"$BIN" history path >/dev/null 2>&1 || fail "history path exited non-zero"

# install --output → merges a ruleset file (the skill's apply path)
cat > "$TMP/rules.json" <<'JSON'
{ "rules": [
  { "name": "verify-allow-ls",  "match": "ls*",     "action": "allow" },
  { "name": "verify-deny-rmrf", "match": "rm -rf*", "action": "deny"  }
] }
JSON
"$BIN" install "$TMP/rules.json" --output "$TMP/merged.json" >/dev/null 2>&1 \
  || fail "install --output exited non-zero"
grep -q "verify-allow-ls" "$TMP/merged.json" || fail "install did not merge the ruleset"
ok "install --output merges an approved ruleset"

# explain + check → the verify step, with real verdicts and exit codes
PROJ="$TMP/proj"; mkdir -p "$PROJ"; cp "$TMP/rules.json" "$PROJ/.allowlister.json"
"$BIN" explain "rm -rf /tmp/x" --cwd "$PROJ" >/dev/null 2>&1 || fail "explain exited non-zero"

"$BIN" check "ls -la" --cwd "$PROJ" >/dev/null 2>&1 \
  || fail "check on an allowed command should exit 0"
if "$BIN" check "rm -rf /tmp/x" --cwd "$PROJ" >/dev/null 2>&1; then
  fail "check on a denied command should exit non-zero (2)"
fi
"$BIN" check --json "ls -la" --cwd "$PROJ" > "$TMP/check.json" 2>/dev/null \
  || fail "check --json on an allowed command should exit 0"
python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$TMP/check.json" \
  || fail "check --json did not emit valid JSON"
ok "explain/check report verdicts and exit codes the skill verifies against"

printf '\n✓ verify-skill passed — %s is installable and its CLI contract holds\n' "$SKILL_NAME"

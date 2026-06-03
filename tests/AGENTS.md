# AGENTS — tests

- `golden.rs` is the rule-table source of truth; add a row for each new motivating
  case. Load rules from explicit example files so cases never depend on ambient
  user/project config.
- Keep fixtures deterministic and local; no network.

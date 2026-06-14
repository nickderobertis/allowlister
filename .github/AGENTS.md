# AGENTS — .github

- CI mirrors `just full-check`: a check that gates locally must gate in CI too.
- Pin actions to a stable major version (or a SHA) and grant least-privilege
  `permissions`.
- Release artifacts publish only after the gate passes; never publish untested
  binaries.
- Informational jobs (the live harness check, performance) run outside the gate:
  they report results and must never be required to merge. A step that needs
  write scope (e.g. posting PR comments) runs only for same-repo, non-fork
  events, since fork pull requests get a read-only token.
- Each live harness e2e runs the full Linux/macOS/Windows matrix (`fail-fast:
  false`, every cell strict). The checks are bash scripts, so force `bash` on
  every OS and keep harness installs per-OS (`$RUNNER_OS` case) — Windows uses
  Git Bash and has no Unix `curl | bash` installer.

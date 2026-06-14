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
- Each live harness e2e runs the Linux/macOS/Windows matrix (`fail-fast: false`,
  every cell strict). The checks are bash scripts, so force `bash` on every OS and
  keep harness installs per-OS (`$RUNNER_OS` case) — Windows uses Git Bash and has
  no Unix `curl | bash` installer.
- A harness that genuinely cannot load the hook on a platform is dropped from that
  OS in its matrix (with a comment), never weakened to a soft pass: Copilot and
  Codex are Linux/macOS only because their hooks don't fire under Windows. Keep
  the README platform-support matrix in sync with what the matrices actually run.

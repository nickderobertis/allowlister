# AGENTS — .github

- Required context names are fixed. The Mac context is a Linux `always()`
  carrier; only the semantic classifier authorizes omitting its prerequisite.
  Failed, cancelled or missing required work must fail the carrier.
- Automatic Mac work belongs only in CI's ordinary-PR lane and release's
  pre-upload Darwin lane. One job runs each lane's assertions sequentially.
  Manual Mac diagnostics opt in through CI, default off.
- Pin actions to a stable major version (or a SHA) and grant least-privilege
  `permissions`.
- Release artifacts publish only after the gate passes; never publish untested
  binaries.
- Release Linux targets are static, non-PIE `-musl` (per-target tuning lives in
  `.cargo/config.toml`; Windows there links the CRT static). The published asset
  names embed the triple, so the release matrix, `scripts/install.sh`,
  `scripts/dist.sh`, and `rust-toolchain.toml` must list the same triples — and
  the external `asdf-allowlister` plugin downloads them by name too.
- Informational jobs (the live harness check, performance) run outside the gate:
  they report results and must never be required to merge. A step that needs
  write scope (e.g. posting PR comments) runs only for same-repo, non-fork
  events, since fork pull requests get a read-only token.
- Live harness workflows keep Linux/Windows coverage; CI owns their formerly
  automatic Mac proofs and optional Mac diagnostics. Keep credential validation
  strict and fork guards intact. `scripts/check-e2e-matrix.sh` checks the matrix
  contract; `.github/tests/` resolves workflow routing and reusable calls.
- Mac policy compares parsed manifest/lockfile non-version fields; author login
  is not authority because release-plz uses a personal token. Only a canonical
  repository release branch with semantic release-only content may omit Mac.
- Workflow test telemetry is uncached and tied to its source digest. Budgets
  consume it without re-running scenarios or embedding limits in graph tests.
- Every matrix cell that runs is strict (`fail-fast: false`, no soft pass). The
  checks are bash scripts, so force `bash` on every OS and keep harness installs
  per-OS (`$RUNNER_OS` case) — Windows uses Git Bash and has no Unix
  `curl | bash` installer.
- A harness that genuinely cannot load the hook on a platform is dropped from that
  OS in its matrix (with a comment), never weakened to a soft pass: Copilot and
  Codex are Linux/macOS only because their hooks don't fire under Windows. Keep
  the README platform-support matrix in sync with what the matrices actually run.

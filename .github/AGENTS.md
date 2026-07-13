# AGENTS — .github

- CI mirrors `just full-check`: a check that gates locally must gate in CI too.
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
- Live e2e (`e2e-*.yml`) triggers on `pull_request` + `workflow_dispatch` only,
  never `push: main` (the release-plz release PR re-runs the suite as the
  pre-release gate). The PR matrix is slim: claude and codex run the full matrix,
  every other harness Linux-only on PR, with a `workflow_dispatch` `os` input
  widening the rest on demand. This contract is duplicated across the `e2e-*.yml`
  files (GitHub Actions can't centralize it); `scripts/check-e2e-matrix.sh` (the
  `lint-workflows` phase of `just check`) is its drift gate — add a row there when
  you add a harness, Linux-only on PR unless it is a primary.
- Every matrix cell that runs is strict (`fail-fast: false`, no soft pass). The
  checks are bash scripts, so force `bash` on every OS and keep harness installs
  per-OS (`$RUNNER_OS` case) — Windows uses Git Bash and has no Unix
  `curl | bash` installer.
- A harness that genuinely cannot load the hook on a platform is dropped from that
  OS in its matrix (with a comment), never weakened to a soft pass: Copilot and
  Codex are Linux/macOS only because their hooks don't fire under Windows. Keep
  the README platform-support matrix in sync with what the matrices actually run.

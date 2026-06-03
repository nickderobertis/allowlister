# AGENTS — .github

- CI mirrors `just full-check`: a check that gates locally must gate in CI too.
- Pin actions to a stable major version (or a SHA) and grant least-privilege
  `permissions`.
- Release artifacts publish only after the gate passes; never publish untested
  binaries.

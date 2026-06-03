# AGENTS — tests/e2e

- These run the compiled binary; assert exit code, stdout, stderr, and file
  effects — not merely that it starts.
- Build a hermetic config environment (temp dirs for user and project config) so
  the host machine's configuration never leaks in.

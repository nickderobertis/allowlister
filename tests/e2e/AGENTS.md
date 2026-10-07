# AGENTS — tests/e2e

- These run the compiled binary; assert exit code, stdout, stderr, and file
  effects — not merely that it starts.
- Build a hermetic config environment (temp dirs for user and project config) so
  the host machine's configuration never leaks in.
- This directory is the `allowlister-e2e` workspace member. `assert_cmd` finds
  the binary beside the test executable (`target/debug`, or the instrumented copy
  in `target/llvm-cov-target/debug`), so run it through `just test-e2e` — its
  `test` target builds the binary first — never against a stale build.
- Resolve repo files through `repo_root()`; this crate's own
  `CARGO_PKG_VERSION` is a placeholder, so compare against `allowlister_version()`.

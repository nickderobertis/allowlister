# AGENTS — commands

- One module per CLI verb. Each returns the process exit code; `main` maps it.
- Keep human-readable output separate from machine-readable (`--json`) output,
  and keep the machine output stable for tests and automation.
- The hook verb is fail-open: a malformed payload writes a stderr note and a
  non-blocking exit, never a deny.

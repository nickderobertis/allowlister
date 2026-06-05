# AGENTS — io

- The only place that touches the filesystem and process stdin/stdout. Inject
  environment inputs (home, XDG, streams) so behavior is testable without the
  real environment.
- A malformed or missing config file is skipped with a recorded warning; loading
  never fails the caller.
- Harness adapters translate one product's request/response envelope into the
  shared decision pipeline; only the I/O shape differs between them.
- A harness without an undecided/defer state maps defer to its safest escalation
  (ask the user), never to allow.
- When a harness's pre-execution hook honors only a deny (rejecting a bare allow)
  or reads a blocking exit code, express every non-deny verdict — and any internal
  read/parse failure — through its no-decision fall-through, so an internal error
  can never become a deny.

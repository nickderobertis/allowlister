# AGENTS — io

- The only place that touches the filesystem and process stdin/stdout. Inject
  environment inputs (home, XDG, streams) so behavior is testable without the
  real environment.
- A malformed or missing config file is skipped with a recorded warning; loading
  never fails the caller.
- Harness adapters translate one product's request/response envelope into the
  shared decision pipeline; only the I/O shape differs between them.
- Map each verdict to the harness's native shape: deny blocks; ask surfaces the
  command for approval where the harness has a confirm/ask state; allow and defer
  follow the rules below.
- A harness without an ask or undecided state maps both ask and defer to its
  safest escalation (the harness's own permission prompt), never to allow.
- When a harness's pre-execution hook honors only a deny (rejecting a bare allow)
  or reads a blocking exit code, express every non-deny verdict — ask and defer
  alike — and any internal read/parse failure through its no-decision
  fall-through, so an internal error can never become a deny.
- Every adapter routes its decision through the shared `gate` (engine call plus
  history recording), never the engine directly, so recording is defined once;
  `check`/`explain` call the engine directly and do not record.
- Usage history is opt-in and best-effort: recording runs inside the hook hot
  path, so it must never block, slow, or alter a decision — every error is
  swallowed. The hot path only appends (atomic per line); the read-modify-write
  fold into the durable summary is serialized by an exclusive-create lock so
  concurrent hook processes can never double-count. Keep the store bounded: raw
  events are folded and cleared, and the summary's per-key maps are capped with
  an overflow bucket, so disk use tracks distinct commands, not call volume.
- Per-key time information stays fixed-size: first/latest timestamps plus decayed
  recency weights, never a per-event timeline or time-bucket series. The decay
  math must be order-independent, since folds can see events out of order.

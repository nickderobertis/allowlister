# AGENTS — io

- The only place that touches the filesystem and process stdin/stdout. Inject
  environment inputs (home, XDG, streams) so behavior is testable without the
  real environment.
- A malformed or missing config file is skipped with a recorded warning; loading
  never fails the caller.
- Harness adapters translate one product's request/response envelope into the
  shared decision pipeline; only the I/O shape differs between them.
- A tool call's `path` is scoped to the working directory in `gate` (and mirrored
  in `check`) before the engine sees it: inside the dir → `./`-relative,
  forward-slash, `.`/`..` resolved; outside → verbatim. This makes a portable
  `./**` profile rule match any harness/path style and is why the domain still
  matches raw strings. Decide inside-vs-outside from the *normalized* path so
  traversal can't disguise an external file as in-project; keep it purely lexical
  (no fs, no symlink resolution).
- Each adapter normalizes its harness's native session field to one `session_id`
  threaded through `gate` to plugins (Cursor's is `conversation_id`, not the
  per-message `generation_id`; Copilot's is `sessionId`; OpenCode's arrives from
  the shim's `input.sessionID`). It is passed to plugins but deliberately **not**
  recorded in usage history — a per-session key would make the store grow with
  session count instead of distinct commands.
- Hook *installation* is delegated to the shared cross-harness installer
  (`oneharness-core`): `hooks` only maps each harness to its gate policy (the
  matcher dialect and timeout) and aggregates the result. Do not re-add
  per-harness file/merge/path logic here. Claude Code's `permissions.deny`
  backstop stays local — it is a permissions merge, not part of the hook.
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
- A recorded event's project tag is repository identity, not the raw cwd: a cwd
  inside a git repo tags by its remote URL (normalized so clones over any
  transport agree), or the repo root when there is no remote, so one user-global
  store aggregates a repo across clones and subdirectories. A non-git cwd keeps
  the folder tag. Resolution is best-effort and runs after the enabled check.

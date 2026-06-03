# AGENTS — src

- `main.rs` stays thin; all behavior lives in the library so it is directly
  testable.
- `lib.rs` re-exports define the public API surface — keep it intentional and
  documented.
- Cross-cutting types (roles, verdicts, fragments) are owned by `domain`; other
  layers import them rather than redefining them.

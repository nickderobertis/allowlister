# AGENTS — live-e2e

- One project per harness (`<harness>/project.json`, `type:live`): its only
  target, `live`, runs `scripts/e2e-<harness>.sh` against the real agent CLI
  (network, a model call, a credential). No gate target, so neither tier reaches
  it; `.github/workflows/e2e-<harness>.yml` runs it through `just test-<harness>`.
- The scripts themselves stay in `scripts/` (the `scripts` project owns them);
  each project lists its script, `e2e-lib.sh` and `e2e-mcp-server.py` as inputs,
  so a change to them marks exactly the live projects that run them.
- A new harness gets a `<harness>/project.json` here, its script, a workflow, a
  `just test-<harness>` recipe, and a row in `scripts/check-e2e-matrix.sh`.

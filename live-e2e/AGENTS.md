# AGENTS — live-e2e

- Each `<harness>/e2e-<harness>.sh` drives a real agent CLI (network, a model
  call, a credential) against the release binary. These suites never join the
  gate: their projects declare only a `live` target, and `.github/workflows/
  e2e-<harness>.yml` runs them through `just test-<harness>`.
- A missing harness CLI or `oneharness` is a skip, not a failure; a missing
  credential fails fast in the workflow.
- Shared helpers live in `e2e-lib.sh` and the MCP fixture in `e2e-mcp-server.py`
  (project `live-e2e-lib`); scripts resolve the repo root as `../..` from their
  own directory.
- A new harness gets a `<harness>/` directory (script + `project.json` tagged
  `type:live`, depending on `allowlister` and `live-e2e-lib`), a workflow, a
  `just test-<harness>` recipe, and a row in `.github/scripts/check-e2e-matrix.sh`.

# AGENTS — schema (config-schema)

- `allowlister.schema.json` is a published contract: GitHub Pages serves it at
  its `$id` (`pages.yml` publishes only it and `index.html`). Never change the
  `$id` or file name; evolve the schema without breaking configs it accepts.
- A config vocabulary change updates the schema in the same change.
- `type:contract`: depends on nothing it serves. It reads its validator,
  `examples/` and the dogfood `.allowlister.jsonc` as `{workspaceRoot}` inputs,
  never through an edge to the crate.
- Read files as UTF-8 explicitly; the sweep runs this on Windows too.

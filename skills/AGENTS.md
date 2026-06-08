# AGENTS — skills

- Skills follow the Agent Skills spec (agentskills.io): a directory with a `SKILL.md`
  (YAML frontmatter + Markdown body). `name` is lowercase-and-hyphens and MUST equal the
  directory name; `description` says what the skill does and when to use it.
- Keep `SKILL.md` under 500 lines; move detail into `references/`, one level deep.
- A skill here drives only allowlister's public CLI contract — the documented subcommands
  and their `--json` — never internal files or private behavior, so it stays valid as the
  engine evolves.
- Validate with `just verify-skill`: it installs the skill the way users do and exercises
  the CLI the skill depends on. Run it after changing a skill or that CLI surface.

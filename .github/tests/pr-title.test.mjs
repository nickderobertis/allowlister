// The pr-title check: its workflow shape, its type list against release-plz's
// changelog parsers (the two must not drift), and the rules themselves run
// locally the way amannn/action-semantic-pull-request@v6 runs them — the
// conventionalcommits preset's parser options through conventional-commits-parser
// (the same packages and majors the action depends on), then the action's checks:
// a type, a subject, and a type from the configured list.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import conventionalCommitsPreset from "conventional-changelog-conventionalcommits";
import { CommitParser } from "conventional-commits-parser";
import { parse } from "yaml";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const workflow = parse(readFileSync(join(repo, ".github/workflows/pr-title.yml"), "utf8"));
const job = workflow.jobs["pr-title"];
const step = job?.steps?.find((s) => s.uses?.startsWith("amannn/action-semantic-pull-request@"));
// The action splits its `types` input on newlines and trims each entry.
const types = (step?.with?.types ?? "")
  .split("\n")
  .map((t) => t.trim())
  .filter(Boolean);

/** The action's validation of a title against `types`; returns the failure, or null. */
async function validate(title) {
  const { parser } = await conventionalCommitsPreset();
  const result = new CommitParser(parser).parse(title);
  if (!result.type) return "no release type";
  if (!result.subject) return "no subject";
  if (!types.some((t) => new RegExp(`^${t}$`).test(result.type))) return `unknown type ${result.type}`;
  return null;
}

test("job pr-title runs the v6 action on the PR events that change a title, with only pull-requests: read", () => {
  assert.ok(job, "pr-title.yml must define job pr-title");
  assert.equal(job.name ?? "pr-title", "pr-title");
  assert.equal(step?.uses, "amannn/action-semantic-pull-request@v6");
  assert.deepEqual(workflow.on, {
    pull_request: { types: ["opened", "edited", "synchronize", "reopened", "ready_for_review"] },
  });
  assert.deepEqual(workflow.permissions, { "pull-requests": "read" });
  assert.equal(job.permissions, undefined, "the job must not widen the workflow's permissions");
  assert.equal(job.if, undefined, "the check must report on every pull request");
});

test("the admitted types are exactly the ones release-plz's commit_parsers name", () => {
  const toml = readFileSync(join(repo, "release-plz.toml"), "utf8");
  const parsers = [...toml.matchAll(/\{\s*message\s*=\s*"\^([a-z]+)"/g)].map((m) => m[1]);
  assert.ok(parsers.length > 0, "release-plz.toml must declare commit_parsers");
  assert.deepEqual([...types].sort(), [...new Set(parsers)].sort());
});

test("a releasable Conventional Commit title passes", async () => {
  for (const title of ["feat: add x", "fix(hook): keep the exit code", "feat!: drop the v0 config", "chore: release v0.5.11"]) {
    assert.equal(await validate(title), null, title);
  }
});

test("a non-conventional or unknown-type title fails", async () => {
  assert.equal(await validate("Add x"), "no release type");
  assert.equal(await validate("feat:no space"), "no release type");
  assert.equal(await validate("feature: add x"), "unknown type feature");
  assert.equal(await validate("wip: try something"), "unknown type wip");
});

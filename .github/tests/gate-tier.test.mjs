// The CI tier router (.github/scripts/gate-tier.mjs) fed synthetic GitHub events
// against a throwaway git history: release-plz's release PR gets the full sweep,
// every other pull request and every push to main the affected tier keyed off an
// explicit base, and the real entrypoint writes that decision where the
// workflow's gate step reads it.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { RoutingError, decide, releaseBranchPrefix } from "../scripts/gate-tier.mjs";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const router = join(repo, ".github/scripts/gate-tier.mjs");
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

let git; // a repo with main (and origin/main) at M2 and a feature branch F1 forked from M1
let sha;
before(() => {
  const dir = mkdtempSync(join(tmpdir(), "gate-tier-"));
  scratch.push(dir);
  const run = (...args) =>
    execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.com", ...args], { cwd: dir, encoding: "utf8" }).trim();
  run("init", "-q", "-b", "main");
  const commit = (msg) => (run("commit", "-q", "--allow-empty", "-m", msg), run("rev-parse", "HEAD"));
  const m1 = commit("m1");
  const m2 = commit("m2");
  run("update-ref", "refs/remotes/origin/main", m2);
  run("switch", "-q", "-c", "feature", m1);
  const f1 = commit("f1");
  git = { dir, run };
  sha = { m1, m2, f1 };
});

const pr = (headRef, { fork = false, baseRef = "main" } = {}) => ({
  pull_request: {
    head: { ref: headRef, repo: { full_name: fork ? "someone/allowlister" : "nickderobertis/allowlister" } },
    base: { ref: baseRef, repo: { full_name: "nickderobertis/allowlister" } },
  },
});

test("release-plz's release PR runs the full sweep", () => {
  const d = decide("pull_request", pr("release-plz-2026-09-20T14-29-27Z"), { cwd: git.dir });
  assert.equal(d.tier, "all");
  assert.equal(d.base, "");
});

test("an ordinary pull request runs the affected tier from its merge base with the base branch", () => {
  const d = decide("pull_request", pr("feature"), { cwd: git.dir });
  assert.deepEqual([d.tier, d.base], ["affected", sha.m1]);
});

test("a fork's branch named like the release branch is still an ordinary pull request", () => {
  const d = decide("pull_request", pr("release-plz-2026-09-20T14-29-27Z", { fork: true }), { cwd: git.dir });
  assert.deepEqual([d.tier, d.base], ["affected", sha.m1]);
});

test("a push to main runs the affected tier from the previous tip", () => {
  git.run("switch", "-q", "main");
  try {
    const d = decide("push", { before: sha.m1 }, { cwd: git.dir });
    assert.deepEqual([d.tier, d.base], ["affected", sha.m1]);
    // A push with no usable `before` (a new branch: all zeros) falls back to HEAD~1.
    const z = decide("push", { before: "0".repeat(40) }, { cwd: git.dir });
    assert.deepEqual([z.tier, z.base], ["affected", sha.m1]);
  } finally {
    git.run("switch", "-q", "feature");
  }
});

test("any other event (a manual dispatch) runs the full sweep", () => {
  assert.equal(decide("workflow_dispatch", {}, { cwd: git.dir }).tier, "all");
});

test("a base ref that is not a plain branch name is refused, running nothing", () => {
  assert.throws(() => decide("pull_request", pr("feature", { baseRef: "main;rm -rf ~" }), { cwd: git.dir }), RoutingError);
  assert.throws(() => decide("pull_request", pr("feature", { baseRef: "a..b" }), { cwd: git.dir }), RoutingError);
});

test("the release branch prefix is read from release-plz.toml's [workspace] table, and nowhere else", () => {
  assert.equal(releaseBranchPrefix(repo), "release-plz-");
  const dir = mkdtempSync(join(tmpdir(), "gate-tier-prefix-"));
  scratch.push(dir);
  const write = (toml) => writeFileSync(join(dir, "release-plz.toml"), toml);
  write('[workspace]\npr_branch_prefix = "rel-"\n\n[changelog]\n');
  assert.equal(releaseBranchPrefix(dir), "rel-");
  assert.equal(decide("pull_request", pr("rel-1"), { cwd: git.dir, root: dir }).tier, "all");
  // Unset, set only in another table, single-quoted, unsafe, or set twice: refused.
  for (const toml of [
    "[workspace]\npublish = false\n",
    '[workspace]\n\n[[package]]\npr_branch_prefix = "rel-"\n',
    "[workspace]\npr_branch_prefix = 'rel-'\n",
    '[workspace]\npr_branch_prefix = "rel-$(id)"\n',
    '[workspace]\npr_branch_prefix = "a-"\npr_branch_prefix = "b-"\n',
  ]) {
    write(toml);
    assert.throws(() => releaseBranchPrefix(dir), RoutingError, toml);
  }
});

test("release-plz.yml's auto-merge selects the release PR by the same prefix", () => {
  const workflow = readFileSync(join(repo, ".github/workflows/release-plz.yml"), "utf8");
  assert.ok(workflow.includes(`startswith("${releaseBranchPrefix(repo)}")`), "the auto-merge step must select the configured prefix");
});

test("the entrypoint writes tier and base to GITHUB_OUTPUT from the event payload", () => {
  const dir = mkdtempSync(join(tmpdir(), "gate-tier-cli-"));
  scratch.push(dir);
  const cases = [
    ["pull_request", pr("release-plz-2026-10-01T00-00-00Z"), "tier=all\nbase=\n"],
    ["pull_request", pr("feature"), `tier=affected\nbase=${sha.m1}\n`],
    ["push", { before: sha.m1 }, `tier=affected\nbase=${sha.m1}\n`],
  ];
  for (const [name, payload, expected] of cases) {
    const event = join(dir, "event.json");
    const output = join(dir, `out-${Math.random()}`);
    writeFileSync(event, JSON.stringify(payload));
    writeFileSync(output, "");
    const out = spawnSync("node", [router], {
      cwd: git.dir,
      encoding: "utf8",
      env: { ...process.env, GITHUB_EVENT_NAME: name, GITHUB_EVENT_PATH: event, GITHUB_OUTPUT: output },
    });
    assert.equal(out.status, 0, out.stderr);
    assert.equal(readFileSync(output, "utf8"), expected);
    assert.match(out.stderr, /^gate-tier: (all|affected) — /);
  }
  const bad = spawnSync("node", [router], {
    cwd: git.dir,
    encoding: "utf8",
    env: { ...process.env, GITHUB_EVENT_NAME: "pull_request", GITHUB_EVENT_PATH: join(dir, "missing.json") },
  });
  assert.equal(bad.status, 1);
  assert.match(bad.stderr, /gate-tier: next: /);
});

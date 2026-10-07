// The CI workflow contract, read from the parsed workflow files:
//
//   * the fixed status-check contexts branch protection names are each reported
//     by exactly one job, in ci.yml, on every pull request — no `if`, `needs`,
//     path filter or skippable step can leave one unreported;
//   * each of those jobs runs its `just` recipe at the tier the router chose, and
//     the step's real script, run the way Actions runs it, propagates a failing
//     sweep as the job's failure;
//   * the review artifact and the informational workflows (notignored, the live
//     harness suites, skill-install) report none of those contexts;
//   * release.yml still re-gates clippy, the unit tests and e2e over the crate;
//   * every setup-node `node-version-file` resolves to the pinned Node version.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { parse } from "yaml";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const workflowsDir = join(repo, ".github/workflows");
const load = (name) => parse(readFileSync(join(workflowsDir, name), "utf8"));
const workflows = Object.fromEntries(
  readdirSync(workflowsDir)
    .filter((f) => f.endsWith(".yml"))
    .map((f) => [f, load(f)]),
);
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

const CONTRACT = ["test (ubuntu-latest)", "test (macos-latest)", "test (windows-latest)", "coverage", "deps & security"];
const RECIPES = { test: "check", coverage: "test-cov", deps: "supply-chain" };

/** The status-check contexts a job reports: its name (or id), expanded over a literal `matrix.os`. */
function contexts(id, job) {
  const name = job.name ?? id;
  if (!name.includes("${{ matrix.os }}")) return [name];
  const os = job.strategy?.matrix?.os;
  // A matrix built by an expression (the live suites' fromJSON) can expand to any
  // runner; report the name with each runner GitHub offers, so a clash is caught.
  const values = Array.isArray(os) ? os : ["ubuntu-latest", "macos-latest", "windows-latest"];
  return values.map((v) => name.replaceAll("${{ matrix.os }}", v));
}

test("each fixed context is reported by exactly one job, in ci.yml", () => {
  const reporters = new Map();
  for (const [file, wf] of Object.entries(workflows)) {
    for (const [id, job] of Object.entries(wf.jobs ?? {})) {
      for (const ctx of contexts(id, job)) {
        if (!CONTRACT.includes(ctx)) continue;
        reporters.set(ctx, [...(reporters.get(ctx) ?? []), `${file}:${id}`]);
      }
    }
  }
  for (const ctx of CONTRACT) {
    const jobs = reporters.get(ctx) ?? [];
    assert.equal(jobs.length, 1, `${ctx} must be reported by exactly one job (found: ${jobs.join(", ") || "none"})`);
    assert.match(jobs[0], /^ci\.yml:/);
  }
});

test("ci.yml runs on every pull request and every push to main, with no filter", () => {
  const on = workflows["ci.yml"].on;
  assert.ok("pull_request" in on, "ci.yml must trigger on pull_request");
  assert.equal(on.pull_request, null, "pull_request must carry no paths/branches/types filter");
  assert.deepEqual(on.push, { branches: ["main"] });
});

test("no condition, dependency or skippable step can leave a contract job unreported or green", () => {
  const jobs = workflows["ci.yml"].jobs;
  for (const id of Object.keys(RECIPES)) {
    const job = jobs[id];
    assert.ok(job, `ci.yml must define job ${id}`);
    for (const key of ["if", "needs", "continue-on-error"]) {
      assert.equal(job[key], undefined, `job ${id} must not set \`${key}\``);
    }
    if (job.strategy) assert.equal(job.strategy["fail-fast"], false, `job ${id}'s matrix must not cancel siblings`);
    for (const step of job.steps) {
      assert.equal(step.if, undefined, `job ${id} step ${step.name ?? step.uses} must not be conditional`);
      assert.equal(step["continue-on-error"], undefined, `job ${id} step ${step.name ?? step.uses} must not soft-fail`);
    }
  }
});

test("each contract job derives the tier explicitly and runs its recipe at it", () => {
  const jobs = workflows["ci.yml"].jobs;
  for (const [id, recipe] of Object.entries(RECIPES)) {
    const steps = jobs[id].steps;
    const checkout = steps.find((s) => s.uses?.startsWith("actions/checkout@"));
    assert.equal(checkout?.with?.["fetch-depth"], 0, `job ${id} needs full history for the merge base`);
    const router = steps.find((s) => s.id === "tier");
    assert.equal(router?.run, "node .github/scripts/gate-tier.mjs", `job ${id} must route through gate-tier.mjs`);
    const gate = steps.at(-1);
    assert.equal(gate.run, `just ${recipe} "$TIER"`, `job ${id}'s last step must be the gate`);
    assert.deepEqual(gate.env, { TIER: "${{ steps.tier.outputs.tier }}", NX_BASE: "${{ steps.tier.outputs.base }}" });
    assert.ok(steps.indexOf(router) < steps.indexOf(gate));
  }
});

test("the tiered recipes key the affected tier off an explicit base and sweep with `all`", () => {
  const justfile = readFileSync(join(repo, "justfile"), "utf8");
  // Every tiered dispatch (the shared nx-tier helper and the gate's own body):
  // affected keys off nx-base.sh, `all` is run-many, and both exclude the live tier.
  const affected = [...justfile.matchAll(/affected\) base="\$\(bash scripts\/nx-base\.sh\)"; exec bash scripts\/nx affected --base="\$base" --exclude=tag:type:live /g)];
  const sweep = [...justfile.matchAll(/all\) exec bash scripts\/nx run-many --exclude=tag:type:live /g)];
  assert.equal(affected.length, 2);
  assert.equal(sweep.length, 2);
  assert.doesNotMatch(justfile, /--uncommitted|--untracked/);
  for (const recipe of Object.values(RECIPES)) {
    assert.match(justfile, new RegExp(`^${recipe} tier="affected":`, "m"), `recipe ${recipe} must take the tier`);
  }
});

/** Run a gate step's real script the way Actions runs `shell: bash`, with a stub `just`. */
function runStep(step, env, justExit) {
  const dir = mkdtempSync(join(tmpdir(), "gate-step-"));
  scratch.push(dir);
  mkdirSync(join(dir, "bin"));
  const record = join(dir, "argv");
  writeFileSync(
    join(dir, "bin/just"),
    `#!/usr/bin/env bash\nprintf '%s|NX_BASE=%s\\n' "$*" "\${NX_BASE-unset}" >> ${JSON.stringify(record)}\nexit ${justExit}\n`,
  );
  chmodSync(join(dir, "bin/just"), 0o755);
  const script = join(dir, "step.sh");
  writeFileSync(script, step.run);
  const out = spawnSync("bash", ["--noprofile", "--norc", "-eo", "pipefail", script], {
    encoding: "utf8",
    env: { ...process.env, PATH: `${join(dir, "bin")}:${process.env.PATH}`, ...env },
  });
  return { status: out.status, calls: readFileSync(record, "utf8").trim().split("\n") };
}

// Which tier each event routes to is gate-tier.test.mjs's: this feeds the gate
// step each decision the router can write and checks what reaches the recipe.
test("the gate step hands each routed tier and base to its recipe, and a failing sweep fails the job", { skip: process.platform === "win32" && "bash stub" }, () => {
  const jobs = workflows["ci.yml"].jobs;
  for (const [id, recipe] of Object.entries(RECIPES)) {
    const gate = jobs[id].steps.at(-1);
    // Release PR: the router writes tier=all, base empty.
    const sweep = runStep(gate, { TIER: "all", NX_BASE: "" }, 0);
    assert.deepEqual([sweep.status, sweep.calls], [0, [`${recipe} all|NX_BASE=`]]);
    // Ordinary PR / push to main: the router writes affected and the derived base.
    const affected = runStep(gate, { TIER: "affected", NX_BASE: "0123abc" }, 0);
    assert.deepEqual([affected.status, affected.calls], [0, [`${recipe} affected|NX_BASE=0123abc`]]);
    // A red sweep: the recipe's non-zero exit is the step's, hence the job's.
    const red = runStep(gate, { TIER: "all", NX_BASE: "" }, 3);
    assert.equal(red.status, 3, `job ${id} must fail when \`just ${recipe} all\` fails`);
  }
});

test("notignored is its own workflow and job, reports no contract context, and skips forks", () => {
  const wf = workflows["notignored.yml"];
  assert.ok("pull_request" in wf.on);
  assert.deepEqual(wf.permissions, { contents: "read", "pull-requests": "write" });
  const [[id, job], ...rest] = Object.entries(wf.jobs);
  assert.equal(rest.length, 0);
  assert.ok(!contexts(id, job).some((c) => CONTRACT.includes(c)));
  assert.equal(job.if, "github.event.pull_request.head.repo.full_name == github.repository");
  assert.equal(job.steps.find((s) => s.uses?.startsWith("actions/checkout@"))?.with?.["fetch-depth"], 0);
  assert.ok(job.steps.some((s) => s.uses === "nickderobertis/notignored@v0"));
  for (const ciJob of Object.values(workflows["ci.yml"].jobs)) assert.equal(ciJob.needs, undefined);
});

test("the live harness suites stay informational, fork-guarded, and outside the contract", () => {
  const live = Object.keys(workflows).filter((f) => /^e2e-.+\.yml$/.test(f));
  assert.equal(live.length, 8);
  for (const file of [...live, "skill-install.yml", "bench.yml"]) {
    for (const [id, job] of Object.entries(workflows[file].jobs)) {
      assert.ok(!contexts(id, job).some((c) => CONTRACT.includes(c)), `${file}:${id} must not report a contract context`);
    }
  }
  for (const file of live) {
    const wf = workflows[file];
    assert.deepEqual(Object.keys(wf.on).sort(), ["pull_request", "workflow_dispatch"], `${file} triggers`);
    const job = Object.values(wf.jobs)[0];
    assert.match(job.if, /github\.event\.pull_request\.head\.repo\.full_name == github\.repository/, `${file} fork guard`);
    assert.ok(job.steps.some((s) => /^\s*just test-[a-z]+\s*$/m.test(s.run ?? "")), `${file} still runs its suite`);
  }
});

test("release.yml still re-gates clippy, the unit tests and e2e over the whole crate", () => {
  const job = workflows["release.yml"].jobs.test;
  const runs = job.steps.map((s) => s.run).filter(Boolean);
  for (const cmd of ["just clippy all", "just test all", "just test-e2e all"]) assert.ok(runs.includes(cmd), `release.yml test runs ${cmd}`);
  assert.deepEqual(workflows["release.yml"].jobs.upload.needs, "test");
});

/**
 * The Node version actions/setup-node@v4 reads from a non-JSON version file: the
 * first line matching its pattern. A bare token — a lone `#` comment line — matches
 * too, so it would be resolved as the version and fail every job that sets up Node.
 */
const setupNodeVersion = (text) => text.match(/^(?:node(js)?\s+)?v?(?<version>[^\s]+)$/m)?.groups?.version ?? text.trim();

test("every setup-node version file resolves to the pinned Node version", () => {
  const steps = Object.entries(workflows).flatMap(([file, wf]) =>
    Object.values(wf.jobs ?? {}).flatMap((job) =>
      (job.steps ?? []).filter((s) => s.uses?.startsWith("actions/setup-node@") && s.with?.["node-version-file"]).map((s) => [file, s]),
    ),
  );
  assert.ok(steps.length > 0, "the gate's jobs set up Node from a version file");
  for (const [file, step] of steps) {
    const versionFile = step.with["node-version-file"];
    const text = readFileSync(join(repo, versionFile), "utf8");
    const pinned = text.match(/^nodejs\s+(\S+)\s*$/m)?.[1];
    assert.ok(pinned, `${versionFile} pins nodejs`);
    assert.equal(setupNodeVersion(text), pinned, `${file}: setup-node resolves ${versionFile} to the nodejs pin`);
  }
});

test("pages.yml stages only the published schema files, never schema/'s project files", { skip: process.platform === "win32" && "bash step" }, () => {
  const step = workflows["pages.yml"].jobs.publish.steps.find((s) => (s.run ?? "").includes("staging="));
  // Run the step's own staging lines (everything before it touches git) against
  // a copy of the real schema/ directory.
  const staging = step.run.split("\n").slice(0, step.run.split("\n").findIndex((l) => l.includes("git config")));
  const dir = mkdtempSync(join(tmpdir(), "pages-"));
  scratch.push(dir);
  spawnSync("cp", ["-R", join(repo, "schema"), join(dir, "schema")]);
  const out = spawnSync("bash", ["-eo", "pipefail", "-c", `${staging.join("\n")}\nls -A "$staging"`], { cwd: dir, encoding: "utf8" });
  assert.equal(out.status, 0, out.stderr);
  assert.deepEqual(out.stdout.trim().split("\n").sort(), [".nojekyll", "allowlister.schema.json", "index.html"]);
  assert.ok(readdirSync(join(repo, "schema")).includes("project.json"), "the fixture must contain a file that is not published");
});

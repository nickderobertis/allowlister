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
//   * run through the real justfile (Nx stubbed to record its calls), the
//     pull-request jobs together run `supply-chain` once while a local `just
//     check` still runs it, and release.yml's one gate recipe covers every target
//     its former clippy / unit / e2e steps ran;
//   * every pinned Node setup reads the .tool-versions `nodejs` pin through the
//     workflow's own step, run here, never through setup-node's version-file parser.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
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
// Each contract job's gate command after `just`, with the tier as `$TIER`. The
// test jobs leave `supply-chain` to `deps & security`, its single run.
const GATES = { test: 'check "$TIER" supply-chain', coverage: 'test-cov "$TIER"', deps: 'supply-chain "$TIER"' };
const RECIPES = Object.fromEntries(Object.entries(GATES).map(([id, gate]) => [id, gate.split(" ")[0]]));

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
  for (const [id, command] of Object.entries(GATES)) {
    const steps = jobs[id].steps;
    const checkout = steps.find((s) => s.uses?.startsWith("actions/checkout@"));
    assert.equal(checkout?.with?.["fetch-depth"], 0, `job ${id} needs full history for the merge base`);
    const router = steps.find((s) => s.id === "tier");
    assert.equal(router?.run, "node .github/scripts/gate-tier.mjs", `job ${id} must route through gate-tier.mjs`);
    const gate = steps.at(-1);
    assert.equal(gate.run, `just ${command}`, `job ${id}'s last step must be the gate`);
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
    assert.match(justfile, new RegExp(`^${recipe} tier="affected"[ :]`, "m"), `recipe ${recipe} must take the tier`);
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
  for (const [id, command] of Object.entries(GATES)) {
    const gate = jobs[id].steps.at(-1);
    const at = (tier) => command.replace('"$TIER"', tier);
    // Release PR: the router writes tier=all, base empty.
    const sweep = runStep(gate, { TIER: "all", NX_BASE: "" }, 0);
    assert.deepEqual([sweep.status, sweep.calls], [0, [`${at("all")}|NX_BASE=`]]);
    // Ordinary PR / push to main: the router writes affected and the derived base.
    const affected = runStep(gate, { TIER: "affected", NX_BASE: "0123abc" }, 0);
    assert.deepEqual([affected.status, affected.calls], [0, [`${at("affected")}|NX_BASE=0123abc`]]);
    // A red sweep: the recipe's non-zero exit is the step's, hence the job's.
    const red = runStep(gate, { TIER: "all", NX_BASE: "" }, 3);
    assert.equal(red.status, 3, `job ${id} must fail when \`just ${at("all")}\` fails`);
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

const realJust = spawnSync("bash", ["-c", "command -v just"], { encoding: "utf8" }).stdout.trim();
const journeySkip = (process.platform === "win32" && "bash stub") || (!realJust && "just is not on PATH");

/**
 * The real justfile in a scratch root whose scripts/nx records each Nx call
 * instead of running it, behind a `just` shim on PATH that runs that justfile —
 * so a workflow step's own script drives the real recipes.
 */
function gateSandbox() {
  const dir = mkdtempSync(join(tmpdir(), "gate-journey-"));
  scratch.push(dir);
  mkdirSync(join(dir, "scripts"));
  mkdirSync(join(dir, "bin"));
  writeFileSync(join(dir, "justfile"), readFileSync(join(repo, "justfile"), "utf8"));
  const record = join(dir, "nx-calls");
  // The stubs take their paths from the environment, never spliced into shell text.
  writeFileSync(join(dir, "scripts/nx"), `printf '%s\\n' "$*" >> "$GATE_NX_RECORD"\n`);
  writeFileSync(join(dir, "scripts/nx-base.sh"), "echo 0123abc\n");
  writeFileSync(
    join(dir, "bin/just"),
    '#!/usr/bin/env bash\nexec "$GATE_REAL_JUST" --justfile "$GATE_ROOT/justfile" --working-directory "$GATE_ROOT" "$@"\n',
  );
  chmodSync(join(dir, "bin/just"), 0o755);
  const sandboxEnv = { GATE_NX_RECORD: record, GATE_REAL_JUST: realJust, GATE_ROOT: dir };
  /** Run a step's script as Actions runs `shell: bash`; returns its status and each Nx call, parsed. */
  const run = (script, env = {}) => {
    rmSync(record, { force: true });
    const out = spawnSync("bash", ["--noprofile", "--norc", "-eo", "pipefail", "-c", script], {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, PATH: `${join(dir, "bin")}:${process.env.PATH}`, ...sandboxEnv, ...env },
    });
    const lines = existsSync(record) ? readFileSync(record, "utf8").trim().split("\n") : [];
    return { status: out.status, stderr: out.stderr, nx: lines.filter(Boolean).map(parseNx) };
  };
  return run;
}

/** An Nx call's mode, the targets after `-t`, and its project filters. */
function parseNx(line) {
  const [mode, ...args] = line.split(" ");
  const targets = [];
  const filters = [];
  let inTargets = false;
  for (const arg of args) {
    if (arg === "-t") inTargets = true;
    else if (arg.startsWith("--")) {
      inTargets = false;
      if (!arg.startsWith("--base=")) filters.push(arg);
    } else if (inTargets) targets.push(arg);
  }
  return { mode, targets, filters };
}

test("the pull-request jobs run supply-chain once, and a local `just check` still runs it", { skip: journeySkip }, () => {
  const run = gateSandbox();
  const jobs = workflows["ci.yml"].jobs;
  for (const [tier, mode] of [["affected", "affected"], ["all", "run-many"]]) {
    const local = run(`just check ${tier}`);
    assert.equal(local.status, 0, local.stderr);
    assert.equal(local.nx.length, 1);
    const gateTargets = local.nx[0].targets;
    assert.ok(gateTargets.includes("supply-chain"), `local \`just check ${tier}\` runs supply-chain`);
    let supplyChainRuns = 0;
    for (const id of Object.keys(GATES)) {
      const job = jobs[id];
      const cells = job.strategy?.matrix?.os?.length ?? 1;
      const { status, stderr, nx } = run(job.steps.at(-1).run, { TIER: tier, NX_BASE: "0123abc" });
      assert.equal(status, 0, stderr);
      for (const call of nx) {
        assert.equal(call.mode, mode, `${id} runs the ${tier} tier`);
        supplyChainRuns += cells * call.targets.filter((t) => t === "supply-chain").length;
      }
      if (id === "test") {
        assert.deepEqual(nx.map((c) => c.targets), [gateTargets.filter((t) => t !== "supply-chain")]);
      }
    }
    assert.equal(supplyChainRuns, 1, `the ${tier} tier's pull-request jobs run supply-chain exactly once`);
  }
  // A skip naming no gate target aborts before Nx runs, rather than skipping nothing.
  const typo = run("just check all supply-chian");
  assert.equal(typo.status, 2);
  assert.match(typo.stderr, /unknown gate target 'supply-chian'/);
  assert.deepEqual(typo.nx, []);
  const hidden = run("just check all $'supply-chain\\nsupply-chian'");
  assert.equal(hidden.status, 2, "a name on a later line of the skip list is checked too");
  assert.deepEqual(hidden.nx, []);
});

test("release.yml gates on the single gate recipe, covering every stage its old inline steps ran", { skip: journeySkip }, () => {
  const run = gateSandbox();
  const job = workflows["release.yml"].jobs.test;
  const gates = job.steps.map((s) => s.run).filter((r) => /\bjust\b/.test(r ?? ""));
  assert.deepEqual(gates, ["just check all"], "release.yml's test job runs exactly the gate recipe");
  const release = run(gates[0]);
  assert.equal(release.status, 0, release.stderr);
  assert.equal(release.nx.length, 1);
  const [sweep] = release.nx;
  assert.equal(sweep.mode, "run-many");
  assert.deepEqual(sweep.filters, ["--exclude=tag:type:live"], "the release gate narrows to no subset of projects");
  // What the release job ran before the single gate recipe replaced it.
  for (const old of ["just clippy all", "just test all", "just test-e2e all"]) {
    const { status, stderr, nx } = run(old);
    assert.equal(status, 0, stderr);
    for (const call of nx) {
      assert.equal(call.mode, "run-many");
      for (const t of call.targets) assert.ok(sweep.targets.includes(t), `\`just check all\` runs ${t}, which \`${old}\` ran`);
    }
  }
  assert.deepEqual(workflows["release.yml"].jobs.upload.needs, "test");
});

/** Run a Node-pin step's script in a directory holding `toolVersions`; returns its exit status and GITHUB_OUTPUT. */
function runPinStep(step, toolVersions) {
  const dir = mkdtempSync(join(tmpdir(), "node-pin-"));
  scratch.push(dir);
  writeFileSync(join(dir, ".tool-versions"), toolVersions);
  const output = join(dir, "output");
  writeFileSync(output, "");
  const out = spawnSync("bash", ["--noprofile", "--norc", "-eo", "pipefail", "-c", step.run], {
    cwd: dir,
    encoding: "utf8",
    env: { ...process.env, GITHUB_OUTPUT: output },
  });
  return { status: out.status, output: readFileSync(output, "utf8") };
}

test("every pinned Node setup reads the .tool-versions nodejs pin through its own step", { skip: process.platform === "win32" && "bash step" }, () => {
  const toolVersions = readFileSync(join(repo, ".tool-versions"), "utf8");
  const pinned = toolVersions.match(/^nodejs\s+(\S+)\s*$/m)?.[1];
  assert.ok(pinned, ".tool-versions pins nodejs");
  let pinnedSetups = 0;
  for (const [file, wf] of Object.entries(workflows)) {
    for (const [id, job] of Object.entries(wf.jobs ?? {})) {
      const steps = job.steps ?? [];
      steps.forEach((step, i) => {
        if (!step.uses?.startsWith("actions/setup-node@")) return;
        assert.equal(step.with?.["node-version-file"], undefined, `${file}:${id} must not hand a version file to setup-node's parser`);
        if (step.with?.["node-version"] !== "${{ steps.node.outputs.version }}") return;
        pinnedSetups += 1;
        const pin = steps.slice(0, i).find((s) => s.id === "node");
        assert.ok(pin, `${file}:${id} reads the pin in a step with id 'node' before setup-node`);
        // The committed file (its comment lines included) resolves to the pin.
        assert.deepEqual(runPinStep(pin, toolVersions), { status: 0, output: `version=${pinned}\n` }, `${file}:${id}`);
        // A file pinning no Node fails the step rather than letting setup-node fall back.
        assert.notEqual(runPinStep(pin, "#\njust 1.40.0\n").status, 0, `${file}:${id} fails without a nodejs pin`);
      });
    }
  }
  for (const file of ["ci.yml", "release.yml"]) {
    assert.ok(JSON.stringify(workflows[file]).includes("steps.node.outputs.version"), `${file} sets up the pinned Node`);
  }
  assert.ok(pinnedSetups >= 4, `the gate's jobs and the release re-gate set up the pinned Node (found ${pinnedSetups})`);
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

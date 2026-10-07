// scripts/nx (the wrapper every gate recipe runs Nx through) and
// scripts/nx-base.sh (the explicit base the affected tier keys off), driven as
// subprocesses: the wrapper installs from the lock exactly when it must, refuses
// clearly without Node or with a failing install, and hands its arguments to Nx;
// the base helper accepts only a plain ref or SHA and otherwise derives the merge
// base with origin/main.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const skip = process.platform === "win32" ? "bash wrapper with stub executables" : false;
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

/** A temp repo holding the wrapper, a lock, and a stub `npm` whose `ci` installs a stub `nx`. */
function stageWrapper({ npmExit = 0 } = {}) {
  const dir = mkdtempSync(join(tmpdir(), "nx-wrapper-"));
  scratch.push(dir);
  mkdirSync(join(dir, "scripts"));
  mkdirSync(join(dir, "bin"));
  copyFileSync(join(repo, "scripts/nx"), join(dir, "scripts/nx"));
  writeFileSync(join(dir, "package.json"), "{}\n");
  writeFileSync(join(dir, "package-lock.json"), "{}\n");
  writeFileSync(
    join(dir, "bin/npm"),
    `#!/usr/bin/env bash
echo "npm $*" >> "${join(dir, "npm.calls")}"
[ ${npmExit} -eq 0 ] || { echo "npm ERR! simulated" >&2; exit ${npmExit}; }
mkdir -p node_modules/.bin
printf '#!/usr/bin/env bash\\necho "nx $* daemon=$NX_DAEMON tui=$NX_TUI"\\n' > node_modules/.bin/nx
chmod +x node_modules/.bin/nx
`,
  );
  chmodSync(join(dir, "bin/npm"), 0o755);
  return dir;
}

const runWrapper = (dir, args, path = `${join(dir, "bin")}:${process.env.PATH}`) =>
  spawnSync("bash", [join(dir, "scripts/nx"), ...args], { cwd: dir, encoding: "utf8", env: { ...process.env, PATH: path } });
const npmCalls = (dir) => (existsSync(join(dir, "npm.calls")) ? readFileSync(join(dir, "npm.calls"), "utf8").trim().split("\n") : []);

test("the wrapper installs from the lock once, then reuses it, and passes arguments through", { skip }, () => {
  const dir = stageWrapper();
  const first = runWrapper(dir, ["show", "projects"]);
  assert.equal(first.status, 0, first.stderr);
  assert.equal(first.stdout.trim(), "nx show projects daemon=false tui=false");
  assert.deepEqual(npmCalls(dir), ["npm ci --no-audit --no-fund"]);
  const second = runWrapper(dir, ["--version"]);
  assert.equal(second.status, 0, second.stderr);
  assert.equal(npmCalls(dir).length, 1, "an up-to-date install must not reinstall");
});

test("a lock newer than the install reinstalls", { skip }, () => {
  const dir = stageWrapper();
  assert.equal(runWrapper(dir, ["--version"]).status, 0);
  const later = new Date(Date.now() + 60_000);
  utimesSync(join(dir, "package-lock.json"), later, later);
  assert.equal(runWrapper(dir, ["--version"]).status, 0);
  assert.equal(npmCalls(dir).length, 2);
});

test("a failing install stops before Nx, says why, and is retried next time", { skip }, () => {
  const dir = stageWrapper({ npmExit: 1 });
  const out = runWrapper(dir, ["show", "projects"]);
  assert.equal(out.status, 1);
  assert.match(out.stderr, /npm ERR! simulated/);
  assert.match(out.stderr, /nx: 'npm ci' failed \(above\)/);
  assert.equal(existsSync(join(dir, "node_modules/.npm-ci-stamp")), false);
  runWrapper(dir, ["show", "projects"]);
  assert.equal(npmCalls(dir).length, 2);
});

test("without Node the wrapper refuses with the next step", { skip }, (t) => {
  // Only bash and the coreutils the wrapper needs; no node or npm.
  const path = "/usr/bin:/bin";
  if (spawnSync("bash", ["-c", "command -v node || command -v npm"], { env: { PATH: path } }).status === 0) {
    t.skip(`node is installed system-wide (${path}) here, so it cannot be made missing`);
    return;
  }
  const out = runWrapper(stageWrapper(), ["--version"], path);
  assert.equal(out.status, 1);
  assert.match(out.stderr, /Nx runs on Node, and node\/npm are not on PATH/);
});

/** A repo with origin/main at M2 and HEAD on a branch forked from M1. */
function stageGit() {
  const dir = mkdtempSync(join(tmpdir(), "nx-base-"));
  scratch.push(dir);
  const git = (...a) => execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.com", ...a], { cwd: dir, encoding: "utf8" }).trim();
  git("init", "-q", "-b", "main");
  git("commit", "-q", "--allow-empty", "-m", "m1");
  const m1 = git("rev-parse", "HEAD");
  git("commit", "-q", "--allow-empty", "-m", "m2");
  git("update-ref", "refs/remotes/origin/main", "HEAD");
  git("switch", "-q", "-c", "feature", m1);
  git("commit", "-q", "--allow-empty", "-m", "f1");
  return { dir, git, m1 };
}

const base = (dir, env) =>
  spawnSync("bash", [join(repo, "scripts/nx-base.sh")], { cwd: dir, encoding: "utf8", env: { PATH: process.env.PATH, ...env } });

test("the base is the merge base with origin/main unless NX_BASE names one", { skip }, () => {
  const { dir, git, m1 } = stageGit();
  const derived = base(dir, {});
  assert.deepEqual([derived.status, derived.stdout.trim()], [0, m1]);
  assert.match(derived.stderr, /merge-base with origin\/main/);
  const head = git("rev-parse", "HEAD");
  const given = base(dir, { NX_BASE: head });
  assert.deepEqual([given.status, given.stdout.trim()], [0, head]);
  assert.equal(base(dir, { NX_BASE: "origin/main" }).status, 0);
});

test("an NX_BASE that is not a plain, resolvable ref is refused before anything runs", { skip }, () => {
  const { dir } = stageGit();
  for (const bad of ["", "HEAD~1", "main;id", "$(id)", "a..b", "-x", "no-such-branch"]) {
    const out = base(dir, { NX_BASE: bad });
    assert.equal(out.status, 1, `NX_BASE=${JSON.stringify(bad)} must be refused`);
    assert.equal(out.stdout, "");
    assert.match(out.stderr, /^nx-base: NX_BASE/);
  }
});

test("with no origin/main and no NX_BASE there is no implicit fallback", { skip }, () => {
  const { dir, git } = stageGit();
  git("update-ref", "-d", "refs/remotes/origin/main");
  const out = base(dir, {});
  assert.equal(out.status, 1);
  assert.match(out.stderr, /no origin\/main in this clone/);
});

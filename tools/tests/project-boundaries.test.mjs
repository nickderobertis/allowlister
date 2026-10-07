// The module-boundary check (tools/check-project-boundaries.mjs) run as its
// `workspace:lint` target runs it: clean on the committed graph (including the
// drift check against the graph `nx graph` resolves), and refusing each kind of
// forbidden edge when one is introduced into a copy of the real tree.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const checker = join(repo, "tools/check-project-boundaries.mjs");
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

const check = (root) => spawnSync("node", [checker, "--root", root], { encoding: "utf8" });

/** A copy of the tracked tree (no node_modules, so the Nx drift check is skipped there). */
function copyTree() {
  const dir = mkdtempSync(join(tmpdir(), "boundaries-"));
  scratch.push(dir);
  const files = execFileSync("git", ["ls-files", "--cached", "--others", "--exclude-standard"], { cwd: repo, encoding: "utf8" })
    .split("\n")
    .filter(Boolean);
  for (const f of files) {
    let stat;
    try {
      stat = lstatSync(join(repo, f));
    } catch (err) {
      if (err.code === "ENOENT") continue; // deleted in the working tree
      throw err;
    }
    mkdirSync(dirname(join(dir, f)), { recursive: true });
    if (stat.isSymbolicLink()) symlinkSync(readlinkSync(join(repo, f)), join(dir, f));
    else copyFileSync(join(repo, f), join(dir, f));
  }
  execFileSync("git", ["init", "-q"], { cwd: dir });
  execFileSync("git", ["add", "-A"], { cwd: dir });
  return dir;
}

function edit(dir, file, change) {
  const path = join(dir, file);
  const json = JSON.parse(readFileSync(path, "utf8"));
  change(json);
  writeFileSync(path, JSON.stringify(json, null, 2));
}

test("the committed graph passes, and agrees with the graph Nx resolves", () => {
  const out = check(repo);
  assert.equal(out.status, 0, out.stderr);
  assert.equal(out.stderr, "");
});

test("the contract may not depend on the crate it serves", () => {
  const dir = copyTree();
  edit(dir, "schema/project.json", (p) => (p.implicitDependencies = ["allowlister"]));
  const out = check(dir);
  assert.equal(out.status, 1);
  assert.match(out.stderr, /config-schema \(type:contract\) may not depend on allowlister \(type:app\)/);
});

test("nothing outside the live tier may depend on a live suite", () => {
  const dir = copyTree();
  edit(dir, "project.json", (p) => (p.implicitDependencies = ["live-e2e-claude"]));
  edit(dir, "tests/e2e/project.json", (p) => p.implicitDependencies.push("live-e2e-codex"));
  const out = check(dir);
  assert.equal(out.status, 1);
  assert.match(out.stderr, /allowlister \(type:app\) may not depend on live-e2e-claude \(type:live\)/);
  assert.match(out.stderr, /allowlister-e2e \(type:e2e\) may not depend on live-e2e-codex \(type:live\)/);
});

test("the crate may not depend on its e2e suite through Cargo either", () => {
  const dir = copyTree();
  const manifest = join(dir, "Cargo.toml");
  writeFileSync(
    manifest,
    readFileSync(manifest, "utf8").replace("[dev-dependencies]\n", '[dev-dependencies]\nallowlister-e2e = { path = "tests/e2e" }\n'),
  );
  const out = check(dir);
  assert.equal(out.status, 1);
  assert.match(out.stderr, /allowlister \(type:app\) may not depend on allowlister-e2e \(type:e2e\) — found Cargo dev dependency/);
});

test("every project carries one known type tag, and every Cargo member a project.json", () => {
  const dir = copyTree();
  edit(dir, ".github/project.json", (p) => (p.tags = ["scope:ci"]));
  execFileSync("git", ["rm", "-q", "-f", "tests/e2e/project.json"], { cwd: dir });
  const out = check(dir);
  assert.equal(out.status, 1);
  assert.match(out.stderr, /ci-workflows must carry exactly one type:\* tag/);
  assert.match(out.stderr, /Cargo member allowlister-e2e \(tests\/e2e\/Cargo.toml\) has no project.json beside it/);
});

test("a Cargo edge Nx is not told about is refused", () => {
  const dir = copyTree();
  edit(dir, "tests/e2e/project.json", (p) => (p.implicitDependencies = []));
  const out = check(dir);
  assert.equal(out.status, 1);
  assert.match(out.stderr, /allowlister-e2e has a Cargo dev dependency on allowlister that its project.json does not declare/);
});

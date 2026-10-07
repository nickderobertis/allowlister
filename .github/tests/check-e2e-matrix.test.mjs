// The live-e2e CI matrix drift gate (.github/scripts/check-e2e-matrix.sh) is the
// single source of the contract every .github/workflows/e2e-*.yml must match.
// These tests drive its two observable outcomes — pass on the committed
// workflows, fail on a drifted copy — so a regression in the gate (or an
// unnoticed contract change) fails here rather than only in CI. Skipped on
// Windows: the gate is a bash script, and it is exercised the way it runs.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const skip = process.platform === "win32" ? "the gate is a bash script" : false;
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

/** Stage the gate plus the real e2e workflows into a temp tree it checks instead of the repo. */
function stage() {
  const tmp = mkdtempSync(join(tmpdir(), "e2e-matrix-"));
  scratch.push(tmp);
  mkdirSync(join(tmp, ".github/scripts"), { recursive: true });
  mkdirSync(join(tmp, ".github/workflows"), { recursive: true });
  copyFileSync(join(repo, ".github/scripts/check-e2e-matrix.sh"), join(tmp, ".github/scripts/check-e2e-matrix.sh"));
  for (const name of readdirSync(join(repo, ".github/workflows"))) {
    if (name.startsWith("e2e-") && name.endsWith(".yml")) {
      copyFileSync(join(repo, ".github/workflows", name), join(tmp, ".github/workflows", name));
    }
  }
  return tmp;
}

const run = (tmp) => spawnSync("bash", [join(tmp, ".github/scripts/check-e2e-matrix.sh")], { encoding: "utf8" });

function mutate(tmp, file, from, to) {
  const path = join(tmp, ".github/workflows", file);
  const text = readFileSync(path, "utf8");
  const drifted = text.replace(from, to);
  assert.notEqual(drifted, text, `fixture mutation for ${file} changed nothing`);
  writeFileSync(path, drifted);
}

test("the gate passes on the committed workflows", { skip }, () => {
  const out = run(stage());
  assert.equal(out.status, 0, `gate should pass.\nstdout: ${out.stdout}\nstderr: ${out.stderr}`);
  assert.match(out.stdout, /all e2e workflows match the matrix contract/);
});

test("the gate fails when a workflow re-adds the push trigger", { skip }, () => {
  const tmp = stage();
  mutate(tmp, "e2e-goose.yml", "on:\n  pull_request:", "on:\n  push:\n    branches: [main]\n  pull_request:");
  const out = run(tmp);
  assert.notEqual(out.status, 0, "gate should fail on a push trigger");
  assert.match(out.stderr, /still triggers on push/);
});

test("the gate fails when a dispatch option is dropped", { skip }, () => {
  const tmp = stage();
  mutate(tmp, "e2e-goose.yml", "          - macos-latest\n", "");
  const out = run(tmp);
  assert.notEqual(out.status, 0, "gate should fail on a missing option");
  assert.match(out.stderr, /missing option/);
});

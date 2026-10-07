// scripts/lint.sh (the scripts project's lint target) and
// scripts/validate-schema.py (config-schema's test), driven as subprocesses:
// lint.sh checks each script with the shell that runs it and fails naming the
// broken file and the next step; the validator reports a config the schema
// rejects on stderr, with the fix, and exits non-zero.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const skip = process.platform === "win32" ? "bash scripts" : false;
const realBash = process.platform === "win32" ? "bash" : execFileSync("bash", ["-c", "command -v bash"], { encoding: "utf8" }).trim();
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

/** A scripts/ directory holding lint.sh, the files given, and the stub `nx` it always checks. */
function stageScripts(files) {
  const dir = mkdtempSync(join(tmpdir(), "lint-"));
  scratch.push(dir);
  mkdirSync(join(dir, "scripts"));
  copyFileSync(join(repo, "scripts/lint.sh"), join(dir, "scripts/lint.sh"));
  writeFileSync(join(dir, "scripts/nx"), "#!/usr/bin/env bash\nexec true\n");
  for (const [name, body] of Object.entries(files)) writeFileSync(join(dir, "scripts", name), body);
  return dir;
}

test("each script is checked by the shell that runs it", { skip }, () => {
  const dir = stageScripts({
    "install.sh": "#!/bin/sh\necho posix\n",
    "tool.sh": "#!/usr/bin/env bash\necho bash\n",
    "lib.sh": "# sourced library: no shebang\nhelper() { :; }\n",
  });
  // Recording stand-ins for the two checkers, found on PATH by lint.sh itself
  // (which runs under the real bash, invoked by path).
  mkdirSync(join(dir, "bin"));
  for (const shell of ["sh", "bash"]) {
    writeFileSync(join(dir, "bin", shell), `#!/bin/sh\necho "${shell} $*" >> "${join(dir, "calls")}"\n`);
    chmodSync(join(dir, "bin", shell), 0o755);
  }
  const out = spawnSync(realBash, [join(dir, "scripts/lint.sh")], { encoding: "utf8", env: { ...process.env, PATH: `${join(dir, "bin")}:${process.env.PATH}` } });
  assert.deepEqual([out.status, out.stdout, out.stderr], [0, "", ""]);
  assert.deepEqual(readFileSync(join(dir, "calls"), "utf8").trim().split("\n").sort(), [
    "bash -n lib.sh",
    "bash -n lint.sh",
    "bash -n nx",
    "bash -n tool.sh",
    "sh -n install.sh",
  ]);
});

test("a syntax error anywhere fails lint, naming the file and line and the next step", { skip }, () => {
  const dir = stageScripts({
    "good.sh": "#!/usr/bin/env bash\necho fine\n",
    "broken.sh": "#!/usr/bin/env bash\nif true; then\n  echo unterminated\n",
    "also-broken.sh": "#!/bin/sh\ncase x in\n",
  });
  const out = spawnSync(realBash, [join(dir, "scripts/lint.sh")], { encoding: "utf8" });
  assert.equal(out.status, 1);
  assert.equal(out.stdout, "");
  assert.match(out.stderr, /broken\.sh: line \d+: syntax error/);
  assert.match(out.stderr, /also-broken\.sh/, "every broken script is reported, not just the first");
  assert.match(out.stderr, /lint: fix the shell syntax errors above \(file:line\), then re-run 'just lint'\./);
});

test("a config the schema rejects is reported on stderr with the fix, and fails", { skip }, (t) => {
  if (spawnSync("bash", ["-c", "command -v uv"]).status !== 0) {
    t.skip("uv is not on PATH (the validator runs through `uv run --script`)");
    return;
  }
  const dir = mkdtempSync(join(tmpdir(), "validate-schema-"));
  scratch.push(dir);
  mkdirSync(join(dir, "scripts"));
  mkdirSync(join(dir, "schema"));
  mkdirSync(join(dir, "examples"));
  copyFileSync(join(repo, "scripts/validate-schema.py"), join(dir, "scripts/validate-schema.py"));
  copyFileSync(join(repo, "schema/allowlister.schema.json"), join(dir, "schema/allowlister.schema.json"));
  copyFileSync(join(repo, "examples/user-config.json"), join(dir, "examples/good.json"));
  writeFileSync(join(dir, "examples/bad.jsonc"), '// an unknown top-level key\n{ "rulez": [] }\n');
  const run = () => spawnSync("uv", ["run", "--quiet", "--script", join(dir, "scripts/validate-schema.py")], { encoding: "utf8" });
  const out = run();
  assert.equal(out.status, 1, out.stdout + out.stderr);
  assert.match(out.stdout, /^ok {3}examples\/good\.json$/m);
  assert.doesNotMatch(out.stdout, /FAIL/);
  assert.match(out.stderr, /^FAIL examples\/bad\.jsonc$/m);
  assert.match(out.stderr, /at <root>: Additional properties are not allowed \('rulez' was unexpected\)/);
  assert.match(out.stderr, /fix the config or widen schema\/allowlister\.schema\.json/);
  rmSync(join(dir, "examples/bad.jsonc"));
  assert.equal(run().status, 0);
});

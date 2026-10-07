// scripts/msrv.sh (the workspace:msrv target) derives the toolchain from
// Cargo.toml's rust-version and refuses when clippy.toml restates a different
// one. A stub `cargo` records the invocation, so no MSRV toolchain is needed.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const skip = process.platform === "win32" ? "bash script with a stub cargo" : false;
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

function stage({ cargoToml, clippyToml }) {
  const dir = mkdtempSync(join(tmpdir(), "msrv-"));
  scratch.push(dir);
  mkdirSync(join(dir, "scripts"));
  mkdirSync(join(dir, "bin"));
  copyFileSync(join(repo, "scripts/msrv.sh"), join(dir, "scripts/msrv.sh"));
  writeFileSync(join(dir, "Cargo.toml"), cargoToml);
  writeFileSync(join(dir, "clippy.toml"), clippyToml);
  writeFileSync(join(dir, "bin/cargo"), `#!/bin/sh\necho "$@" > "${join(dir, "cargo.args")}"\n`);
  chmodSync(join(dir, "bin/cargo"), 0o755);
  return dir;
}

const run = (dir) =>
  spawnSync("bash", [join(dir, "scripts/msrv.sh")], { encoding: "utf8", env: { ...process.env, PATH: `${join(dir, "bin")}:${process.env.PATH}` } });

test("the real manifests agree, and the check runs under the declared MSRV", { skip }, () => {
  const dir = stage({ cargoToml: readFileSync(join(repo, "Cargo.toml"), "utf8"), clippyToml: readFileSync(join(repo, "clippy.toml"), "utf8") });
  const out = run(dir);
  assert.equal(out.status, 0, out.stderr);
  const msrv = readFileSync(join(repo, "Cargo.toml"), "utf8").match(/^\[workspace\.package\][^[]*?^rust-version = "([^"]+)"/ms)[1];
  assert.equal(readFileSync(join(dir, "cargo.args"), "utf8").trim(), `+${msrv}.0 check --locked --workspace --all-targets --all-features`);
});

test("a clippy.toml msrv that differs from rust-version fails before any build", { skip }, () => {
  const dir = stage({ cargoToml: '[workspace.package]\nrust-version = "1.88"\n', clippyToml: 'msrv = "1.87"\n' });
  const out = run(dir);
  assert.equal(out.status, 1);
  assert.match(out.stderr, /clippy.toml msrv '1.87' differs from Cargo.toml rust-version '1.88'/);
  assert.throws(() => readFileSync(join(dir, "cargo.args")));
});

test("a missing or malformed rust-version is refused", { skip }, () => {
  for (const cargoToml of ["[package]\nname = \"x\"\n", '[workspace.package]\nrust-version = "latest"\n']) {
    const out = run(stage({ cargoToml, clippyToml: 'msrv = "1.88"\n' }));
    assert.equal(out.status, 1);
    assert.match(out.stderr, /must declare rust-version as MAJOR.MINOR/);
  }
});

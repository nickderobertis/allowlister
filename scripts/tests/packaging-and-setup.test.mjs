// scripts/dist.sh plan and scripts/setup-check.sh, driven as subprocesses:
//   * the release plan reads each package field whether the manifest states it
//     literally or inherits it from [workspace.package], fails naming a missing
//     one, and prints the real crate's asset names unchanged;
//   * setup readiness requires Node (the orchestrator's runtime) alongside the
//     rest of the toolchain.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const skip = process.platform === "win32" ? "bash scripts" : false;
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

/** A one-crate cargo package whose manifest is `manifest`, with dist.sh beside it. */
function stagePackage(manifest) {
  const dir = mkdtempSync(join(tmpdir(), "dist-plan-"));
  scratch.push(dir);
  mkdirSync(join(dir, "scripts"));
  mkdirSync(join(dir, "src"));
  copyFileSync(join(repo, "scripts/dist.sh"), join(dir, "scripts/dist.sh"));
  writeFileSync(join(dir, "src/main.rs"), "fn main() {}\n");
  writeFileSync(join(dir, "Cargo.toml"), manifest);
  return dir;
}

const plan = (dir) => spawnSync("bash", ["scripts/dist.sh", "plan"], { cwd: dir, encoding: "utf8" });

const PACKAGE = (license) => `[package]
name = "allowlister"
version = "1.2.3"
edition = "2021"
${license}
repository = "https://example.com/r"
description = "d"

[[bin]]
name = "allowlister"
path = "src/main.rs"
`;

test("the plan reads literal and workspace-inherited package fields alike", { skip }, () => {
  const literal = plan(stagePackage(PACKAGE('license = "MIT"')));
  assert.equal(literal.status, 0, literal.stderr);
  assert.match(literal.stdout, /^license: {4}MIT$/m);
  const inherited = plan(stagePackage(`${PACKAGE("license.workspace = true")}\n[workspace]\n\n[workspace.package]\nlicense = "Apache-2.0"\n`));
  assert.equal(inherited.status, 0, inherited.stderr);
  assert.match(inherited.stdout, /^package: {4}allowlister 1\.2\.3$/m);
  assert.match(inherited.stdout, /^license: {4}Apache-2\.0$/m);
});

test("the plan fails naming a field the manifest does not provide", { skip }, () => {
  const out = plan(stagePackage(PACKAGE("")));
  assert.equal(out.status, 1);
  assert.match(out.stderr, /missing required package field: license/);
});

test("the real crate's plan keeps every release asset name", { skip }, () => {
  const out = spawnSync("bash", ["scripts/dist.sh", "plan"], { cwd: repo, encoding: "utf8" });
  assert.equal(out.status, 0, out.stderr);
  const version = readFileSync(join(repo, "Cargo.toml"), "utf8").match(/^\[package\][^[]*?^version = "([^"]+)"/ms)[1];
  for (const asset of [
    `allowlister-${version}-x86_64-unknown-linux-musl.tar.gz`,
    `allowlister-${version}-aarch64-unknown-linux-musl.tar.gz`,
    `allowlister-${version}-x86_64-apple-darwin.tar.gz`,
    `allowlister-${version}-aarch64-apple-darwin.tar.gz`,
    `allowlister-${version}-x86_64-pc-windows-msvc.zip`,
  ]) {
    assert.ok(out.stdout.includes(asset), `plan must still name ${asset}`);
  }
  assert.match(out.stdout, /^license: {4}MIT$/m);
});

/** A copy of the setup scripts plus a bin dir of stand-ins for every required tool except `missing`. */
function stageSetup(missing) {
  const dir = mkdtempSync(join(tmpdir(), "setup-check-"));
  scratch.push(dir);
  cpSync(join(repo, "scripts"), join(dir, "repo/scripts"), { recursive: true });
  for (const f of ["rust-toolchain.toml", ".tool-versions", "justfile"]) copyFileSync(join(repo, f), join(dir, "repo", f));
  const required = readFileSync(join(repo, "scripts/setup-lib.sh"), "utf8").match(/^REQUIRED_BINS="([^"]+)"/m)[1].split(" ");
  mkdirSync(join(dir, "bin"));
  for (const b of required.filter((b) => !missing.includes(b))) {
    writeFileSync(join(dir, "bin", b), "#!/bin/sh\nexit 0\n");
    chmodSync(join(dir, "bin", b), 0o755);
  }
  const env = { PATH: `${join(dir, "bin")}:/usr/bin:/bin`, HOME: join(dir, "home") };
  spawnSync("bash", ["-c", ". scripts/setup-lib.sh; _write_stamp"], { cwd: join(dir, "repo"), env });
  return { dir, env, required };
}

const setupCheck = ({ dir, env }) => spawnSync("bash", [join(dir, "repo/scripts/setup-check.sh")], { encoding: "utf8", env });

test("readiness requires node and npm beside the rest of the toolchain", { skip }, (t) => {
  for (const tool of ["node", "npm"]) {
    if (spawnSync("bash", ["-c", `command -v ${tool}`], { env: { PATH: "/usr/bin:/bin" } }).status === 0) {
      t.skip(`${tool} is installed system-wide here, so it cannot be made missing`);
      return;
    }
  }
  const ready = stageSetup([]);
  assert.ok(ready.required.includes("node") && ready.required.includes("npm"));
  assert.equal(setupCheck(ready).status, 0, setupCheck(ready).stdout);
  const out = setupCheck(stageSetup(["node", "npm"]));
  assert.equal(out.status, 1);
  assert.match(out.stdout + out.stderr, /missing tools: node npm/);
});

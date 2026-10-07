// The SessionStart hook (scripts/session-setup.sh) run as Claude Code runs it — a
// subprocess whose stdout becomes session context — down every path it can exit
// by. Each path must hand off to setup-llmlint.sh, return at once with exit 0
// whether that installer succeeds, fails, or hangs, and keep the installer's
// output out of the session. Copies of the scripts run in a temp tree with an
// isolated HOME, so nothing touches the real machine.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const skip = process.platform === "win32" ? "the hook is a bash script" : false;
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

/** A temp tree holding the hook's scripts, with setup-llmlint.sh replaced by `installer` when given. */
function stage(installer) {
  const dir = mkdtempSync(join(tmpdir(), "session-setup-"));
  scratch.push(dir);
  cpSync(join(repo, "scripts"), join(dir, "repo/scripts"), { recursive: true });
  for (const f of ["rust-toolchain.toml", ".tool-versions", "justfile"]) cpSync(join(repo, f), join(dir, "repo", f));
  // The opt-in auto path launches setup.sh; a stub keeps it from provisioning.
  writeFileSync(join(dir, "repo/scripts/setup.sh"), "#!/usr/bin/env bash\necho stub-setup\n");
  if (installer) {
    writeFileSync(join(dir, "repo/scripts/setup-llmlint.sh"), `#!/usr/bin/env bash\n${installer}\n`);
    chmodSync(join(dir, "repo/scripts/setup-llmlint.sh"), 0o755);
  }
  mkdirSync(join(dir, "home"));
  return dir;
}

const RECORD = 'echo "ran $$" > "$(dirname "$0")/../handoff.ran"';
const OUTCOMES = {
  succeeds: `${RECORD}\necho installer-noise; exit 0`,
  fails: `${RECORD}\necho installer-noise >&2; exit 1`,
  hangs: `${RECORD}\nsleep 20`,
};

function runHook(dir, env) {
  const started = Date.now();
  const out = spawnSync("bash", [join(dir, "repo/scripts/session-setup.sh")], {
    encoding: "utf8",
    timeout: 15_000,
    env: { PATH: process.env.PATH, HOME: join(dir, "home"), CLAUDE_PROJECT_DIR: join(dir, "repo"), ...env },
  });
  return { ...out, ms: Date.now() - started };
}

async function waitFor(path, ms = 5_000) {
  for (const end = Date.now() + ms; Date.now() < end; ) {
    if (existsSync(path)) return true;
    await new Promise((r) => setTimeout(r, 50));
  }
  return existsSync(path);
}

const PATHS = {
  "this repo's CI": { GITHUB_ACTIONS: "true" },
  "the skip escape hatch": { ALLOWLISTER_SKIP_SETUP: "1" },
  "opt-in auto setup": { ALLOWLISTER_AUTO_SETUP: "1" },
  "advice (not set up)": {},
};

for (const [path, env] of Object.entries(PATHS)) {
  for (const [outcome, installer] of Object.entries(OUTCOMES)) {
    test(`${path}: hands off and returns at once when the installer ${outcome}`, { skip }, async () => {
      const dir = stage(installer);
      const out = runHook(dir, env);
      assert.equal(out.status, 0, out.stderr);
      assert.ok(out.ms < 5_000, `hook took ${out.ms}ms; the hand-off must not block`);
      assert.ok(await waitFor(join(dir, "repo/handoff.ran")), "setup-llmlint.sh was never launched");
      assert.doesNotMatch(out.stdout, /installer-noise/, "installer output must stay out of the session context");
      if (outcome !== "hangs") {
        const log = join(dir, "repo/.dev/setup-llmlint.log");
        for (const end = Date.now() + 5_000; Date.now() < end && !readFileSync(log, "utf8").includes("installer-noise"); ) {
          await new Promise((r) => setTimeout(r, 50));
        }
        assert.match(readFileSync(log, "utf8"), /installer-noise/, "the installer's output must reach .dev/setup-llmlint.log");
      }
      if (path === "advice (not set up)") assert.match(out.stdout, /ACTION: run 'just setup'/);
      if (path === "this repo's CI" || path === "the skip escape hatch") assert.equal(out.stdout, "");
    });
  }
}

test("a ready environment stays silent and still hands off", { skip }, async () => {
  const dir = stage(OUTCOMES.succeeds);
  // Stand-ins for every binary readiness requires, plus a matching stamp.
  const bin = join(dir, "bin");
  mkdirSync(bin);
  for (const b of readFileSync(join(repo, "scripts/setup-lib.sh"), "utf8").match(/^REQUIRED_BINS="([^"]+)"/m)[1].split(" ")) {
    writeFileSync(join(bin, b), "#!/bin/sh\nexit 0\n");
    chmodSync(join(bin, b), 0o755);
  }
  const path = `${bin}:/usr/bin:/bin`;
  const stamp = spawnSync("bash", ["-c", ". scripts/setup-lib.sh; _write_stamp"], { cwd: join(dir, "repo"), env: { PATH: path } });
  assert.equal(stamp.status, 0);
  const out = runHook(dir, { PATH: path });
  assert.equal(out.status, 0, out.stderr);
  assert.equal(out.stdout, "");
  assert.ok(await waitFor(join(dir, "repo/handoff.ran")));
});

test("the real installer, with uv missing from PATH, logs why and the hook still exits 0", { skip }, async (t) => {
  const path = "/usr/bin:/bin";
  if (spawnSync("bash", ["-c", "command -v uv"], { env: { PATH: path } }).status === 0) {
    t.skip(`uv is on ${path} here, so it cannot be made missing`);
    return;
  }
  const dir = stage(null);
  const out = runHook(dir, { PATH: path, GITHUB_ACTIONS: "true" });
  assert.equal(out.status, 0, out.stderr);
  const log = join(dir, "repo/.dev/setup-llmlint.log");
  assert.ok(await waitFor(log));
  for (const end = Date.now() + 5_000; Date.now() < end && !/llmlint not installed/.test(readFileSync(log, "utf8")); ) {
    await new Promise((r) => setTimeout(r, 50));
  }
  assert.match(readFileSync(log, "utf8"), /uv not found; cannot install llmlint/);
});

test("without setsid the hand-off falls back to nohup and still detaches", { skip }, async (t) => {
  // A PATH holding every system tool except setsid.
  const dir = stage(OUTCOMES.hangs);
  const bin = join(dir, "no-setsid");
  mkdirSync(bin);
  for (const sys of ["/usr/bin", "/bin"]) {
    for (const name of readdirSync(sys)) {
      if (name === "setsid" || existsSync(join(bin, name))) continue;
      symlinkSync(join(sys, name), join(bin, name));
    }
  }
  if (spawnSync("bash", ["-c", "command -v setsid"], { env: { PATH: bin } }).status === 0) {
    t.skip("could not hide setsid");
    return;
  }
  const out = runHook(dir, { PATH: bin, GITHUB_ACTIONS: "true" });
  assert.equal(out.status, 0, out.stderr);
  assert.ok(out.ms < 5_000, `hook took ${out.ms}ms`);
  assert.ok(await waitFor(join(dir, "repo/handoff.ran")), "the nohup branch never launched the installer");
});

test("a missing installer is skipped quietly", { skip }, () => {
  const dir = stage(null);
  rmSync(join(dir, "repo/scripts/setup-llmlint.sh"));
  const out = runHook(dir, { GITHUB_ACTIONS: "true" });
  assert.deepEqual([out.status, out.stdout, out.stderr], [0, "", ""]);
});

for (const [what, block] of [
  [".dev/ is not a directory", (dir) => writeFileSync(join(dir, "repo/.dev"), "a file where the log directory should be\n")],
  ["the log cannot be opened", (dir) => mkdirSync(join(dir, "repo/.dev/setup-llmlint.log"), { recursive: true })],
]) {
  test(`when ${what}, the hook skips the install, says why and what to run, and still exits 0`, { skip }, async () => {
    const dir = stage(OUTCOMES.succeeds);
    block(dir);
    const out = runHook(dir, { GITHUB_ACTIONS: "true" });
    assert.equal(out.status, 0);
    assert.equal(out.stdout, "");
    assert.match(out.stderr, /^session-setup: llmlint not provisioned: cannot write .*setup-llmlint\.log \(.+\); fix that, then run just setup-llmlint$/m);
    assert.equal(await waitFor(join(dir, "repo/handoff.ran"), 1_000), false);
  });
}

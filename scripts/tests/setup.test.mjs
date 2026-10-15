// scripts/setup.sh and scripts/setup-llmlint.sh driven end to end, with stand-ins
// only at the external boundary (asdf, uv, rustup, just and direnv record what
// they were asked to do): setup provisions the asdf `nodejs` plugin and the
// pinned tools before bootstrap and leaves a stamp setup-check accepts; the
// llmlint installer asks uv for the release floor it declares.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const skip = process.platform === "win32" ? "bash scripts with stub executables" : false;
const scratch = [];
after(() => scratch.forEach((d) => rmSync(d, { recursive: true, force: true })));

/** A copy of the repo's setup inputs, an isolated HOME, and recording stand-ins for `tools`. */
function stage(tools) {
  const dir = mkdtempSync(join(tmpdir(), "setup-"));
  scratch.push(dir);
  cpSync(join(repo, "scripts"), join(dir, "repo/scripts"), { recursive: true });
  for (const f of ["rust-toolchain.toml", ".tool-versions", "justfile"]) copyFileSync(join(repo, f), join(dir, "repo", f));
  mkdirSync(join(dir, "home"));
  mkdirSync(join(dir, "bin"));
  const log = join(dir, "calls");
  for (const [name, body] of Object.entries(tools)) {
    writeFileSync(join(dir, "bin", name), `#!/usr/bin/env bash\necho "${name} $*" >> "${log}"\n${body}\n`);
    chmodSync(join(dir, "bin", name), 0o755);
  }
  const calls = () => (existsSync(log) ? readFileSync(log, "utf8").trim().split("\n") : []);
  const env = { PATH: `${join(dir, "bin")}:/usr/bin:/bin`, HOME: join(dir, "home"), SHELL: "/bin/bash" };
  return { dir, calls, env };
}

const ok = "exit 0";
const SETUP_TOOLS = {
  // `plugin list` reports none installed, so setup must add each one; `list
  // direnv` reports a version, so direnv itself is not reinstalled.
  asdf: 'case "$1 $2" in "plugin list") ;; "list direnv") echo "  2.35.0" ;; "--version ") echo v0.14.1 ;; esac',
  direnv: ok,
  just: ok,
  rustup: ok,
  rustc: 'echo "rustc 1.96.0"',
  cargo: ok,
  "cargo-nextest": ok,
  node: ok,
  npm: ok,
};

test("setup adds the nodejs plugin and installs the pinned tools before bootstrap, then stamps readiness", { skip }, () => {
  const { dir, calls, env } = stage(SETUP_TOOLS);
  const out = spawnSync("bash", [join(dir, "repo/scripts/setup.sh")], { cwd: join(dir, "repo"), encoding: "utf8", env });
  assert.equal(out.status, 0, out.stdout + out.stderr);
  const log = calls();
  const at = (call) => log.indexOf(call);
  for (const call of ["asdf plugin add just", "asdf plugin add nodejs", "asdf install", "just bootstrap"]) {
    assert.ok(at(call) >= 0, `setup must run \`${call}\` (ran: ${log.join(" | ")})`);
  }
  assert.ok(at("asdf plugin add nodejs") < at("asdf install"), "the plugin must exist before `asdf install` reads .tool-versions");
  assert.ok(at("asdf install") < at("just bootstrap"), "Node must be installed before bootstrap runs `npm ci`");
  assert.match(readFileSync(join(dir, "repo/.tool-versions"), "utf8"), /^nodejs \d+\.\d+\.\d+$/m);
  const check = spawnSync("bash", [join(dir, "repo/scripts/setup-check.sh")], { encoding: "utf8", env });
  assert.equal(check.status, 0, check.stdout + check.stderr);
});

test("setup-llmlint asks uv for the declared llmlint floor and never fails the session", { skip }, () => {
  const floor = readFileSync(join(repo, "scripts/setup-llmlint.sh"), "utf8").match(/^readonly LLMLINT_MIN="([^"]+)"/m)[1];
  assert.match(floor, /^\d+\.\d+\.\d+$/);
  const [major, minor, patch] = floor.split(".").map(Number);
  assert.ok(major > 0 || minor > 3 || (minor === 3 && patch >= 23), `the floor ${floor} must be at least 0.3.23`);
  for (const uv of [ok, 'echo "simulated resolver failure" >&2; exit 1']) {
    const { dir, calls, env } = stage({ uv });
    const out = spawnSync("bash", [join(dir, "repo/scripts/setup-llmlint.sh")], { encoding: "utf8", env });
    assert.equal(out.status, 0, out.stderr);
    assert.deepEqual(calls(), [`uv tool install --upgrade llmlint-cli>=${floor}`]);
  }
});

test("bootstrap installs the pinned pruner with its own compiler once and propagates install failures", { skip }, () => {
  const { dir, calls } = stage({
    "cargo-binstall": ok,
    lefthook: ok,
    rustup: ok,
    cargo: `case "$*" in
      *"install --locked cargo-prune-superseded"*)
        [ "\${FAIL_INSTALL:-0}" = 0 ] || exit 42
        [[ "$1" == +1.97.1 ]] || exit 43
        touch "\$PRUNER_MARKER" ;;
    esac`,
    "cargo-prune-superseded": '[ -f "$PRUNER_MARKER" ] && echo "cargo-prune-superseded 0.1.1"',
  });
  writeFileSync(join(dir, "repo/scripts/nx"), "exit 0\n");
  writeFileSync(join(dir, "repo/scripts/hooks-install.sh"), "exit 0\n");
  const env = { ...process.env, PATH: `${join(dir, "bin")}:${process.env.PATH}`, PRUNER_MARKER: join(dir, "installed") };
  const run = extra => spawnSync("just", ["bootstrap"], { cwd: join(dir, "repo"), env: { ...env, ...extra }, encoding: "utf8" });
  const failed = run({ FAIL_INSTALL: "1" });
  assert.equal(failed.status, 42, failed.stdout + failed.stderr);
  const first = run({});
  assert.equal(first.status, 0, first.stdout + first.stderr);
  assert.ok(calls().includes("rustup toolchain install 1.97.1 --profile minimal --no-self-update"));
  const installs = () => calls().filter(call => call.includes("install --locked cargo-prune-superseded"));
  assert.equal(installs().length, 2);
  const second = run({});
  assert.equal(second.status, 0, second.stdout + second.stderr);
  assert.equal(installs().length, 2, "the installed release must not be reinstalled");
});

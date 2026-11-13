// Drives the real scripts/e2e-lib.sh `al_run` in bash against the real
// `oneharness` the workflows pin. Only the paid provider process is replaced,
// by that release's own `--mock-harness` responder, so oneharness's argv
// rendering, `--output-dir` transcript files and JSON report are the shipped
// ones rather than a fixture's copy of them.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";

const repo = resolve(import.meta.dirname, "../..");
// The pin the workflows install; .github/tests/release-contract.test.mjs holds
// every workflow to this one version.
const pinned = readFileSync(join(repo, ".github/workflows/e2e-cursor.yml"), "utf8")
  .match(/cargo install oneharness --version (\S+) --locked/)[1];
const installed = spawnSync("oneharness", ["--version"], { encoding: "utf8" }).stdout?.trim();
const skip = process.platform === "win32"
  ? "requires bash"
  : installed !== `oneharness ${pinned}`
    ? `requires the pinned oneharness ${pinned} on PATH (cargo install oneharness --version ${pinned} --locked); found ${installed || "none"}`
    : false;

function alRun(root, args) {
  mkdirSync(join(root, "proj"), { recursive: true });
  mkdirSync(join(root, "home"), { recursive: true });
  const stream = join(root, "run.stream");
  const argvFile = join(root, "argv");
  const out = spawnSync("bash", ["-c", `. scripts/e2e-lib.sh && al_run cursor 'run it' "$STREAM" "$@"`, "al_run",
    "--mock-harness", "cursor", "--cwd", join(root, "proj"), "--timeout", "60", ...args], {
    cwd: repo,
    encoding: "utf8",
    env: {
      ...process.env,
      // No ambient user config: CI runners have none.
      HOME: join(root, "home"),
      XDG_CONFIG_HOME: join(root, "home/.config"),
      STREAM: stream,
      MOCK_ARGV_FILE: argvFile,
      MOCK_STDOUT: '{"type":"result","result":"allowlister: denied"}',
      MOCK_STDERR: "harness stderr",
      MOCK_EXIT: "3",
    },
  });
  assert.equal(out.status, 0, out.stderr);
  return {
    argv: readFileSync(argvFile, "utf8").trimEnd().split("\n"),
    stdout: readFileSync(stream, "utf8"),
    stderr: readFileSync(`${stream}.err`, "utf8"),
  };
}

test("al_run drives the pinned oneharness bypass-by-default and captures the transcript", { skip }, () => {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "e2e-lib-"));
  try {
    // No mode chosen: the headless run bypasses Cursor's own confirmation.
    const plain = alRun(join(root, "plain"), []);
    assert.ok(plain.argv.includes("--force"), plain.argv.join(" "));
    // A failed harness run is not fatal: its transcript and the report's status
    // fields still reach the files the e2e assertions read.
    assert.match(plain.stdout, /"result":"allowlister: denied"/);
    assert.match(plain.stderr, /harness stderr/);
    assert.match(plain.stderr, /oneharness-report: "exit_code":3/);

    // A caller's mode choice replaces the default (oneharness refuses --bypass
    // beside --no-bypass, so a doubled flag would fail the run outright).
    for (const [chosen, wants, refuses] of [
      [["--no-bypass"], "--trust", "--force"],
      [["--mode", "read-only"], "ask", "--force"],
      [["--mode=read-only"], "ask", "--force"],
      [["--bypass"], "--force", "--trust"],
    ]) {
      const run = alRun(join(root, chosen.join("")), chosen);
      assert.ok(run.argv.includes(wants), `${chosen}: ${run.argv.join(" ")}`);
      assert.ok(!run.argv.includes(refuses), `${chosen}: ${run.argv.join(" ")}`);
    }

    // A verbatim harness argument after `--` is not an oneharness mode choice.
    const verbatim = alRun(join(root, "verbatim"), ["--", "--no-bypass"]);
    assert.ok(verbatim.argv.includes("--force"), verbatim.argv.join(" "));
    assert.equal(verbatim.argv.at(-1), "--no-bypass");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// Drives the real scripts/e2e-lib.sh `al_run` in bash. Only the `oneharness`
// executable is a stand-in: it records its argv and writes the per-harness
// transcript files a real `--output-dir` run leaves, so the approval-mode
// request and the stream capture the live e2e scripts rely on are observable
// without a paid harness.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, chmodSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";

const repo = resolve(import.meta.dirname, "../..");
const skip = process.platform === "win32" ? "requires bash with a POSIX PATH for the oneharness stand-in" : false;

function alRun(root, args) {
  const bin = join(root, "bin");
  mkdirSync(bin, { recursive: true });
  writeFileSync(join(bin, "oneharness"), `#!/usr/bin/env bash
printf '%s\\n' "$@" > "$ARGV_FILE"
while [ $# -gt 0 ]; do
  case "$1" in --output-dir) od="$2"; shift ;; --harness) id="$2"; shift ;; --) break ;; esac
  shift
done
printf 'transcript for %s\\n' "$id" > "$od/$id.stdout"
printf 'harness stderr\\n' > "$od/$id.stderr"
echo 'driver warning' >&2
echo '{"results":[{"status":"failed","exit_code":3}]}'
exit 1
`);
  chmodSync(join(bin, "oneharness"), 0o755);
  const stream = join(root, "run.stream");
  const out = spawnSync("bash", ["-c", `. scripts/e2e-lib.sh && al_run cursor 'run it' "$STREAM" "$@"`, "al_run", ...args], {
    cwd: repo,
    encoding: "utf8",
    env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, ARGV_FILE: join(root, "argv"), STREAM: stream },
  });
  assert.equal(out.status, 0, out.stderr);
  return {
    argv: readFileSync(join(root, "argv"), "utf8").trimEnd().split("\n"),
    stdout: readFileSync(stream, "utf8"),
    stderr: readFileSync(`${stream}.err`, "utf8"),
  };
}

test("al_run requests bypass unless the caller chose an approval mode", { skip }, () => {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "e2e-lib-"));
  try {
    const plain = alRun(join(root, "plain"), ["--cwd", "/proj", "--timeout", "180"]);
    assert.deepEqual(plain.argv.slice(0, 2), ["run", "--harness"]);
    assert.equal(plain.argv.filter((a) => a === "--bypass").length, 1, plain.argv.join(" "));
    assert.ok(plain.argv.indexOf("--bypass") < plain.argv.indexOf("--cwd"));
    // A harness's failed run is not fatal: its transcript, the driver's own
    // diagnostics and the report's status fields still reach the assertions.
    assert.equal(plain.stdout, "transcript for cursor\n");
    assert.match(plain.stderr, /^harness stderr\ndriver warning\n/);
    assert.match(plain.stderr, /oneharness-report: "status":"failed"/);
    assert.match(plain.stderr, /oneharness-report: "exit_code":3/);

    for (const chosen of [["--no-bypass"], ["--mode", "default"], ["--mode=read-only"], ["--bypass"]]) {
      const run = alRun(join(root, chosen.join("")), ["--cwd", "/proj", ...chosen]);
      assert.deepEqual(run.argv.slice(-chosen.length), chosen);
      assert.equal(run.argv.filter((a) => a === "--bypass").length, chosen[0] === "--bypass" ? 1 : 0, run.argv.join(" "));
    }

    // A verbatim harness argument after `--` is not an oneharness mode choice.
    const verbatim = alRun(join(root, "verbatim"), ["--cwd", "/proj", "--", "--no-bypass"]);
    assert.ok(verbatim.argv.includes("--bypass"), verbatim.argv.join(" "));
    assert.deepEqual(verbatim.argv.slice(-2), ["--", "--no-bypass"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

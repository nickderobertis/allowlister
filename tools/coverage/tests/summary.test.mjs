// tools/coverage/summary.mjs, run the way coverage.sh runs it (the export piped
// on stdin): it reads the named totals of llvm's JSON export and refuses
// anything that is not one, rather than printing a misleading summary.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const script = join(dirname(fileURLToPath(import.meta.url)), "..", "summary.mjs");
const run = (input) => spawnSync("node", [script, "95"], { input: typeof input === "string" ? input : JSON.stringify(input), encoding: "utf8" });

// The shape `cargo llvm-cov report --json --summary-only` (0.8.x) emits.
const exported = (totals) => ({
  type: "llvm.coverage.json.export",
  version: "3.1.0",
  cargo_llvm_cov: { version: "0.8.7" },
  data: [{ totals }],
});
const TOTALS = {
  functions: { count: 1036, covered: 1002, percent: 96.71814671814671 },
  lines: { count: 9990, covered: 9746, percent: 97.55755755755756 },
  regions: { count: 17364, covered: 16900, notcovered: 464, percent: 97.32780465330569 },
};

test("a merged report summarizes as lines, functions and regions against the floor", () => {
  const out = run(exported(TOTALS));
  assert.equal(out.status, 0, out.stderr);
  assert.equal(out.stdout.trim(), "coverage: lines 97.56%, functions 96.72%, regions 97.33% (floor 95%)");
});

test("anything other than one llvm coverage export is refused with the reason", () => {
  for (const [input, reason] of [
    ["not json", /Unexpected token|JSON/],
    [{ ...exported(TOTALS), type: "something.else" }, /not an llvm coverage export/],
    [{ ...exported(TOTALS), version: "9.0.0" }, /unsupported export version/],
    [{ ...exported(TOTALS), data: [] }, /exactly one data entry/],
    [exported({ ...TOTALS, lines: { count: 1 } }), /totals.lines.percent is not a percentage/],
    [exported({ ...TOTALS, regions: { percent: 140 } }), /totals.regions.percent is not a percentage/],
  ]) {
    const out = run(input);
    assert.equal(out.status, 1, JSON.stringify(input).slice(0, 80));
    assert.equal(out.stdout, "");
    assert.match(out.stderr, reason);
    assert.match(out.stderr, /check the cargo-llvm-cov version/);
  }
});

test("the floor argument must be one percentage", () => {
  for (const args of [[], ["ninety"], ["101"], ["95", "extra"], ["-5"]]) {
    const out = spawnSync("node", [script, ...args], { input: JSON.stringify(exported(TOTALS)), encoding: "utf8" });
    assert.equal(out.status, 2, JSON.stringify(args));
    assert.match(out.stderr, /^usage: node tools\/coverage\/summary.mjs <floor percent, 0-100>/);
  }
});

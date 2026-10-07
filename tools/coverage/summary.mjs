// Print the one-line coverage summary from `cargo llvm-cov report --json
// --summary-only` (llvm's structured `llvm.coverage.json.export`), reading the
// named totals rather than the text report's column positions.
//
// Usage: cargo llvm-cov report --json --summary-only ... | node tools/coverage/summary.mjs <floor>
// Exit status: 0 with the summary on stdout; 1 when the input is not a coverage
// export this script understands (the message names what was expected).
import { readFileSync } from "node:fs";

/** The summary line for an export, or a thrown Error naming what is wrong with it. */
export function summarize(exported, floor) {
  if (exported?.type !== "llvm.coverage.json.export") throw new Error(`not an llvm coverage export (type: ${JSON.stringify(exported?.type)})`);
  if (!/^[23]\./.test(String(exported.version))) throw new Error(`unsupported export version ${JSON.stringify(exported.version)} (expected 2.x or 3.x)`);
  if (!Array.isArray(exported.data) || exported.data.length !== 1) throw new Error("expected exactly one data entry (one merged report)");
  const totals = exported.data[0]?.totals;
  const pct = (metric) => {
    const p = totals?.[metric]?.percent;
    if (typeof p !== "number" || !Number.isFinite(p) || p < 0 || p > 100) throw new Error(`totals.${metric}.percent is not a percentage`);
    return `${p.toFixed(2)}%`;
  };
  return `coverage: lines ${pct("lines")}, functions ${pct("functions")}, regions ${pct("regions")} (floor ${floor}%)`;
}

if (import.meta.main) {
  try {
    console.log(summarize(JSON.parse(readFileSync(0, "utf8")), process.argv[2]));
  } catch (err) {
    console.error(`coverage: cannot summarize the report: ${err.message}; check the cargo-llvm-cov version against tools/coverage/summary.mjs.`);
    process.exit(1);
  }
}

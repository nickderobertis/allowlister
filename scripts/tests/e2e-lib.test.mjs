// Drives the real scripts/e2e-lib.sh `al_run` in bash against the real
// `oneharness` the workflows pin. Only the paid provider process is replaced,
// by that release's own `--mock-harness` responder, so oneharness's argv
// rendering, `--output-dir` transcript files and JSON report are the shipped
// ones rather than a fixture's copy of them.
//
// No expectation here restates what oneharness renders for a harness: each one
// is the pinned binary's own output for the equivalent direct `oneharness run`,
// so the release stays the one source of its own contract.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";

const repo = resolve(import.meta.dirname, "../..");
// The pin the workflows install; .github/tests/release-contract.test.mjs holds
// every workflow to this one version.
const pinned = readFileSync(join(repo, ".github/workflows/e2e-cursor.yml"), "utf8")
  .match(/cargo install oneharness --version (\S+) --locked/)[1];
const installed = spawnSync("oneharness", ["--version"], { encoding: "utf8" }).stdout?.trim();
// Where the pin is absent these checks skip, and nothing goes stale meanwhile:
// every expectation below is computed from that binary at run time, never
// written down here.
const skip = process.platform === "win32"
  ? "requires bash"
  : installed !== `oneharness ${pinned}`
    ? `requires the pinned oneharness ${pinned} on PATH (cargo install oneharness --version ${pinned} --locked); found ${installed || "none"}`
    : false;

const SOURCES = Object.fromEntries(readdirSync(join(repo, "scripts"))
  .filter((f) => /^e2e-.+\.sh$/.test(f) && f !== "e2e-lib.sh")
  .map((f) => [f.slice(4, -3), readFileSync(join(repo, "scripts", f), "utf8")]));
const HARNESSES = Object.fromEntries(Object.entries(SOURCES)
  .map(([script, source]) => [script, source.match(/^\s*al_run (\S+) "\$prompt"/m)?.[1]])
  .filter(([, id]) => id));

const PROVIDER = {
  MOCK_STDOUT: '{"type":"result","result":"allowlister: denied"}',
  MOCK_STDERR: "harness stderr",
  MOCK_EXIT: "3",
};

function sandbox(root) {
  mkdirSync(join(root, "proj"), { recursive: true });
  mkdirSync(join(root, "home"), { recursive: true });
  // No ambient user config or mode default: CI runners have neither.
  const env = { ...process.env, HOME: join(root, "home"), XDG_CONFIG_HOME: join(root, "home/.config") };
  delete env.ONEHARNESS_MODE;
  return env;
}

const lines = (file) => readFileSync(file, "utf8").trimEnd().split("\n");
// The provider stand-in re-serializes the JSON it is given, so a transcript is
// compared as data.
const json = (text) => JSON.parse(text);

// The project directory a pair of compared runs share.
function project(dir) {
  mkdirSync(join(dir, "proj"), { recursive: true });
  return join(dir, "proj");
}

// llmlint: ignore-block[e2e_not_mocked] Only the paid model provider is replaced, by the pinned release's own --mock-harness responder, in both the al_run run and the direct run it is compared with, because this test holds no harness credential; al_run and the oneharness binary under test run for real. The unmocked journey is the live e2e scripts, which drive the real harness CLIs in CI.
// The pinned release called directly, with no al_run in between: what it hands
// the harness, and its own JSON report.
function direct(root, harness, args, extraEnv = {}) {
  const env = sandbox(root);
  const argvFile = join(root, "argv");
  const out = spawnSync("oneharness", ["run", "--harness", harness, "--prompt", "run it",
    "--output-dir", join(root, "out"), "--compact", "--mock-harness", harness, ...args], {
    encoding: "utf8",
    env: { ...env, ...PROVIDER, MOCK_ARGV_FILE: argvFile, ...extraEnv },
  });
  return { argv: lines(argvFile), report: out.stdout };
}

function alRun(root, harness, args, mock = {}) {
  const env = sandbox(root);
  const stream = join(root, "run.stream");
  const argvFile = join(root, "argv");
  const out = spawnSync("bash", ["-c", `. scripts/e2e-lib.sh && al_run "$HARNESS" 'run it' "$STREAM" "$@"`, "al_run",
    "--mock-harness", harness, ...args], {
    cwd: repo,
    encoding: "utf8",
    env: { ...env, ...PROVIDER, HARNESS: harness, STREAM: stream, MOCK_ARGV_FILE: argvFile, ...mock },
  });
  assert.equal(out.status, 0, out.stderr);
  return {
    argv: lines(argvFile),
    stdout: readFileSync(stream, "utf8"),
    stderr: readFileSync(`${stream}.err`, "utf8"),
  };
}
// llmlint: ignore-end[e2e_not_mocked]

function scratch() {
  return mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "e2e-lib-"));
}

test("al_run runs every live script's harness in the pinned release's bypass mode by default", { skip }, () => {
  const root = scratch();
  try {
    assert.ok(Object.keys(HARNESSES).length >= 7, JSON.stringify(HARNESSES));
    let distinguished = 0;
    for (const harness of new Set(Object.values(HARNESSES))) {
      const common = ["--cwd", project(join(root, harness))];
      const run = alRun(join(root, harness, "al_run"), harness, common);
      const bypass = direct(join(root, harness, "bypass"), harness, [...common, "--mode", "bypass"]);
      assert.deepEqual(run.argv, bypass.argv, harness);
      const fallback = direct(join(root, harness, "default"), harness, common);
      if (fallback.argv.join("\n") !== bypass.argv.join("\n")) distinguished += 1;
    }
    // Some harness renders bypass and the release's own default differently, so
    // the equality above cannot hold merely because no mode reached any harness.
    assert.ok(distinguished > 0, "no harness distinguishes bypass from the default mode");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a caller's own mode choice and verbatim harness arguments reach oneharness unaltered", { skip }, () => {
  const root = scratch();
  try {
    for (const [i, [chosen, equivalent]] of [
      [["--no-bypass"], ["--no-bypass"]],
      [["--mode", "read-only"], ["--mode", "read-only"]],
      [["--mode=read-only"], ["--mode=read-only"]],
      [["--bypass"], ["--bypass"]],
      // A verbatim argument after `--` is the harness's, not an oneharness mode
      // choice, so the bypass default still applies.
      [["--", "--no-bypass"], ["--mode", "bypass", "--", "--no-bypass"]],
    ].entries()) {
      const common = ["--cwd", project(join(root, `${i}`))];
      const run = alRun(join(root, `${i}`, "al_run"), "cursor", [...common, ...chosen]);
      const wants = direct(join(root, `${i}`, "direct"), "cursor", [...common, ...equivalent]);
      assert.deepEqual(run.argv, wants.argv, chosen.join(" "));
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("al_run captures the transcript and surfaces the release's own report for failed and successful runs", { skip }, () => {
  const root = scratch();
  try {
    for (const [name, mock] of [
      ["failed", {}],
      ["ok", { MOCK_STDOUT: '{"type":"result","result":"all done"}', MOCK_STDERR: "harness note", MOCK_EXIT: "0" }],
    ]) {
      const common = ["--cwd", project(join(root, name))];
      // A failed harness run is not fatal (alRun asserts al_run exits 0): its
      // transcript and the report's status fields still reach the stream files.
      const run = alRun(join(root, name, "al_run"), "cursor", common, mock);
      const { report } = direct(join(root, name, "direct"), "cursor", [...common, "--mode", "bypass"], mock);
      assert.deepEqual(json(run.stdout), json(mock.MOCK_STDOUT ?? PROVIDER.MOCK_STDOUT), name);
      assert.ok(run.stderr.startsWith(mock.MOCK_STDERR ?? PROVIDER.MOCK_STDERR), `${name}: ${run.stderr}`);
      const surfaced = run.stderr.split("\n").filter((l) => l.startsWith("oneharness-report: "))
        .map((l) => l.slice("oneharness-report: ".length));
      assert.ok(surfaced.length > 0, `${name}: ${run.stderr}`);
      for (const field of surfaced) assert.ok(report.includes(field), `${name}: ${field} not in ${report}`);
      const exit = mock.MOCK_EXIT ?? PROVIDER.MOCK_EXIT;
      assert.ok(surfaced.some((field) => field.endsWith(`:${exit}`)), `${name}: exit ${exit} in ${surfaced}`);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// Each live script's own run function, read from its source rather than restated
// here, with the call mode its cases use.
const SCRIPTS = [
  ["claude", "run_claude", ""],
  ["copilot", "run_agent", ""],
  ["crush", "run_agent", ""],
  ["cursor", "run_agent", ""],
  ["cursor", "run_agent", "trust"],
  ["goose", "run_agent", ""],
  ["opencode", "run_agent", ""],
  ["qwen", "run_agent", ""],
];

// The script's last assignment to a variable (Goose's populated extension list
// follows its empty default).
function scriptLine(source, name) {
  return source.split("\n").findLast((line) => new RegExp(`^\\s*${name}=`).test(line)) ?? "";
}

// llmlint: ignore-block[e2e_not_mocked, tests_mirror_real_usage] Each script's own run function is replayed, not its whole entry point, because the script's assertions need a real model to attempt tool calls and that needs a harness credential this test does not hold; the al_run wrapper only records its arguments, appends the pinned release's --mock-harness and calls the real al_run and oneharness. The whole scripts run unmocked in the live e2e jobs (`just test-<harness>`), the Claude one in the required macOS job.
test("each live script's run function reaches the pinned oneharness with its exact arguments", { skip }, () => {
  const root = scratch();
  try {
    const rendered = {};
    for (const [script, fn, callMode] of SCRIPTS) {
      const source = SOURCES[script];
      const harness = HARNESSES[script];
      const body = source.match(new RegExp(`^${fn}\\(\\) \\{\\n[\\s\\S]*?^\\}$`, "m"))?.[0];
      assert.ok(harness && body, `${script}: ${fn} not found`);
      const label = `${script} ${callMode || "bypass"}`;
      const dir = join(root, `${script}-${callMode || "bypass"}`);
      const env = sandbox(dir);
      const proj = join(dir, "proj");
      const mcpConfig = join(proj, ".mcp.json");
      writeFileSync(mcpConfig, "{}");
      const stream = join(dir, "run.stream");
      const argvFile = join(dir, "argv");
      const argsFile = join(dir, "al_run.args");
      const defs = [scriptLine(source, "model"), "mcp_server=/srv/mcp.py",
        scriptLine(source, "goose_ext_args").trim(), body].join("\n");
      const out = spawnSync("bash", ["-c", `. scripts/e2e-lib.sh
eval "$(declare -f al_run | sed '1s/^al_run/al_run_real/')"
al_run() { printf '%s\\0' "$@" >"$AL_RUN_ARGS"; local id="$1" p="$2" s="$3"; shift 3; al_run_real "$id" "$p" "$s" --mock-harness "$id" "$@"; }
${defs}
${fn} 'run it' "$STREAM" ${callMode}`], {
        cwd: repo,
        encoding: "utf8",
        env: {
          ...env,
          ALLOWLISTER_E2E_MODEL: "",
          OPENAI_MODEL: "",
          STREAM: stream,
          AL_RUN_ARGS: argsFile,
          proj,
          sandbox: dir,
          mcp_config: mcpConfig,
          agent_bin: `/opt/${harness}`,
          claude_bin: "/opt/claude",
          mcp_sentinel: "/tmp/s",
          mcp_token: "TOKEN",
          mcp_log: "/tmp/l",
          MOCK_ARGV_FILE: argvFile,
          MOCK_STDOUT: PROVIDER.MOCK_STDOUT,
          MOCK_EXIT: "0",
        },
      });
      assert.equal(out.status, 0, `${label}: ${out.stderr}`);
      assert.deepEqual(json(readFileSync(stream, "utf8")), json(PROVIDER.MOCK_STDOUT), label);
      const [id, prompt, , ...args] = readFileSync(argsFile, "utf8").split("\0").slice(0, -1);
      assert.deepEqual([id, prompt], [harness, "run it"], label);
      const argv = lines(argvFile);
      // The same arguments given straight to the release, under the bypass
      // default al_run applies (the first test holds that default to the
      // release's own --mode bypass), render the identical harness argv.
      const wants = direct(join(dir, "direct"), harness, args, { ONEHARNESS_MODE: "bypass", MOCK_EXIT: "0" });
      assert.deepEqual(argv, wants.argv, label);
      // Every verbatim harness argument the script passes after `--` (Claude's
      // MCP flags, Goose's --with-extension, Qwen's --auth-type) arrives as given.
      const sep = args.indexOf("--");
      const verbatim = sep < 0 ? [] : args.slice(sep + 1);
      if (verbatim.length) assert.deepEqual(argv.slice(-verbatim.length), verbatim, label);
      rendered[label] = { argv: argv.join(" "), verbatim };
    }
    // Goose's run function took its populated extension list, which names the
    // MCP server this test defined, not the empty default before it.
    assert.ok(rendered["goose bypass"].verbatim.some((a) => a.includes("/srv/mcp.py")),
      JSON.stringify(rendered["goose bypass"]));
    assert.notEqual(rendered["cursor trust"].argv, rendered["cursor bypass"].argv);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
// llmlint: ignore-end[e2e_not_mocked, tests_mirror_real_usage]

// Drives the real scripts/e2e-lib.sh `al_run` in bash against the real
// `oneharness` the workflows pin. Only the paid provider process is replaced,
// by that release's own `--mock-harness` responder, so oneharness's argv
// rendering, `--output-dir` transcript files and JSON report are the shipped
// ones rather than a fixture's copy of them.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";

const repo = resolve(import.meta.dirname, "../..");
// The pin the workflows install; .github/tests/release-contract.test.mjs holds
// every workflow to this one version.
const pinned = readFileSync(join(repo, ".github/workflows/e2e-cursor.yml"), "utf8")
  .match(/cargo install oneharness --version (\S+) --locked/)[1];
const installed = spawnSync("oneharness", ["--version"], { encoding: "utf8" }).stdout?.trim();
// llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] The authority for oneharness compatibility is the live e2e jobs, which install this same pin and run al_run against the real harness — the required macOS job does so on same-repo pull requests — so a drift fails a required check; this test is the credential-free local pre-check and runs wherever the pin is installed.
const skip = process.platform === "win32"
  ? "requires bash"
  : installed !== `oneharness ${pinned}`
    ? `requires the pinned oneharness ${pinned} on PATH (cargo install oneharness --version ${pinned} --locked); found ${installed || "none"}`
    : false;
// llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate]

// llmlint: ignore-block[e2e_not_mocked] Only the paid model provider is replaced, by the pinned release's own --mock-harness responder, because this test holds no harness credential; al_run and the oneharness binary under test run for real. The unmocked journey is the live e2e scripts, which drive the real harness CLIs in CI.
function alRun(root, args, harness = "cursor", mock = {}) {
  mkdirSync(join(root, "proj"), { recursive: true });
  mkdirSync(join(root, "home"), { recursive: true });
  const stream = join(root, "run.stream");
  const argvFile = join(root, "argv");
  const out = spawnSync("bash", ["-c", `. scripts/e2e-lib.sh && al_run "$HARNESS" 'run it' "$STREAM" "$@"`, "al_run",
    "--mock-harness", harness, "--cwd", join(root, "proj"), "--timeout", "60", ...args], {
    cwd: repo,
    encoding: "utf8",
    env: {
      ...process.env,
      // No ambient user config: CI runners have none.
      HOME: join(root, "home"),
      XDG_CONFIG_HOME: join(root, "home/.config"),
      HARNESS: harness,
      STREAM: stream,
      MOCK_ARGV_FILE: argvFile,
      MOCK_STDOUT: '{"type":"result","result":"allowlister: denied"}',
      MOCK_STDERR: "harness stderr",
      MOCK_EXIT: "3",
      ...mock,
    },
  });
  assert.equal(out.status, 0, out.stderr);
  return {
    argv: readFileSync(argvFile, "utf8").trimEnd().split("\n"),
    stdout: readFileSync(stream, "utf8"),
    stderr: readFileSync(`${stream}.err`, "utf8"),
  };
}
// llmlint: ignore-end[e2e_not_mocked]

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
    assert.match(plain.stderr, /oneharness-report: "status":"nonzero"/);

    const ok = alRun(join(root, "ok"), [], "cursor",
      { MOCK_STDOUT: '{"type":"result","result":"all done"}', MOCK_STDERR: "harness note", MOCK_EXIT: "0" });
    assert.ok(ok.argv.includes("--force"), ok.argv.join(" "));
    assert.match(ok.stdout, /"result":"all done"/);
    assert.match(ok.stderr, /^harness note/);
    assert.match(ok.stderr, /oneharness-report: "status":"ok"/);
    assert.match(ok.stderr, /oneharness-report: "exit_code":0/);
    assert.match(ok.stderr, /oneharness-report: "error":null/);

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

// Every harness a scripts/e2e-*.sh drives through al_run, with the approval argv
// the pinned release renders for the bypass default. Crush and Goose render no
// approval flag in any mode, so for them the run itself is what is checked.
const BYPASS = {
  "claude-code": ["--permission-mode", "bypassPermissions"],
  copilot: ["--allow-all-tools"],
  crush: [],
  cursor: ["--force"],
  goose: [],
  opencode: ["--dangerously-skip-permissions"],
  qwen: ["--yolo"],
};

test("al_run's bypass default reaches every harness the live scripts drive", { skip }, () => {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "e2e-lib-"));
  try {
    for (const [harness, flags] of Object.entries(BYPASS)) {
      const run = alRun(join(root, harness), [], harness);
      for (const flag of flags) assert.ok(run.argv.includes(flag), `${harness}: ${run.argv.join(" ")}`);
      assert.match(run.stdout, /"result":"allowlister: denied"/, harness);
      assert.match(run.stderr, /oneharness-report: "exit_code":3/, harness);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// Each live script's own run function, read from its source rather than restated
// here, replayed through al_run against the pinned release with every argument it
// passes (verbatim harness flags after `--` included). The only addition is
// --mock-harness, so the provider stand-in records the argv the harness receives.
const SCRIPTS = [
  // [script, run function, harness id, call mode, argv the harness must receive]
  ["claude", "run_claude", "claude-code", "",
    ["--permission-mode", "bypassPermissions", "--model", "haiku", "--output-format", "stream-json",
      "--max-turns", "6", "--verbose", "--mcp-config", "MCP_CONFIG", "--strict-mcp-config"]],
  ["copilot", "run_agent", "copilot", "", ["--allow-all-tools"]],
  ["crush", "run_agent", "crush", "", ["run"]],
  ["cursor", "run_agent", "cursor", "", ["--force", "--output-format", "stream-json"]],
  ["cursor", "run_agent", "cursor", "trust", ["--trust", "--output-format", "stream-json"]],
  ["goose", "run_agent", "goose", "", ["--with-extension", "GOOSE_EXT"]],
  ["opencode", "run_agent", "opencode", "",
    ["--dangerously-skip-permissions", "-m", "anthropic/claude-haiku-4-5", "--format", "json"]],
  ["qwen", "run_agent", "qwen", "", ["--yolo", "-m", "gpt-4.1-mini", "--auth-type", "openai"]],
];

// The script's last assignment to a variable (Goose's populated extension list
// follows its empty default).
function scriptLine(source, name) {
  return source.split("\n").findLast((line) => new RegExp(`^\\s*${name}=`).test(line)) ?? "";
}

test("each live script's run function reaches the pinned oneharness with its exact arguments", { skip }, () => {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "e2e-lib-"));
  try {
    for (const [script, fn, harness, callMode, wants] of SCRIPTS) {
      const source = readFileSync(join(repo, `scripts/e2e-${script}.sh`), "utf8");
      const body = source.match(new RegExp(`^${fn}\\(\\) \\{\\n[\\s\\S]*?^\\}$`, "m"))?.[0];
      assert.ok(body, `${script}: ${fn} not found`);
      const dir = join(root, `${script}-${callMode || "bypass"}`);
      const proj = join(dir, "proj");
      mkdirSync(join(dir, "home"), { recursive: true });
      mkdirSync(proj, { recursive: true });
      const mcpConfig = join(proj, ".mcp.json");
      writeFileSync(mcpConfig, "{}");
      const stream = join(dir, "run.stream");
      const argvFile = join(dir, "argv");
      // The script's own definitions: its run function, and the top-level model
      // and Goose extension assignments that function reads.
      const defs = [scriptLine(source, "model"), "mcp_server=/srv/mcp.py",
        scriptLine(source, "goose_ext_args").trim(), body].join("\n");
      const out = spawnSync("bash", ["-c", `. scripts/e2e-lib.sh
eval "$(declare -f al_run | sed '1s/^al_run/al_run_real/')"
al_run() { local id="$1" p="$2" s="$3"; shift 3; al_run_real "$id" "$p" "$s" --mock-harness "$id" "$@"; }
${defs}
${fn} 'run it' "$STREAM" ${callMode}`], {
        cwd: repo,
        encoding: "utf8",
        env: {
          ...process.env,
          HOME: join(dir, "home"),
          XDG_CONFIG_HOME: join(dir, "home/.config"),
          ALLOWLISTER_E2E_MODEL: "",
          OPENAI_MODEL: "",
          STREAM: stream,
          proj,
          sandbox: dir,
          mcp_config: mcpConfig,
          agent_bin: `/opt/${harness}`,
          claude_bin: "/opt/claude",
          mcp_sentinel: "/tmp/s",
          mcp_token: "TOKEN",
          mcp_log: "/tmp/l",
          MOCK_ARGV_FILE: argvFile,
          MOCK_STDOUT: '{"type":"result","result":"allowlister: denied"}',
          MOCK_EXIT: "0",
        },
      });
      const label = `${script} ${callMode || "bypass"}`;
      assert.equal(out.status, 0, `${label}: ${out.stderr}`);
      const err = readFileSync(`${stream}.err`, "utf8");
      assert.match(err, /oneharness-report: "status":"ok"/, `${label}: ${err}`);
      assert.match(readFileSync(stream, "utf8"), /"result":"allowlister: denied"/, label);
      const argv = readFileSync(argvFile, "utf8").trimEnd().split("\n");
      for (const want of wants) {
        const value = { MCP_CONFIG: mcpConfig, GOOSE_EXT: "python3 /srv/mcp.py /tmp/s TOKEN /tmp/l" }[want] ?? want;
        assert.ok(argv.includes(value), `${label}: wants ${value} in ${argv.join(" ")}`);
      }
      if (callMode) assert.ok(!argv.some((a) => ["--force", "bypassPermissions"].includes(a)), `${label}: ${argv.join(" ")}`);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

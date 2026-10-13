// Real Git and Lefthook journeys; no installer or check-command doubles.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, copyFileSync, readFileSync, writeFileSync, existsSync, rmSync, mkdirSync, chmodSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { test } from "node:test";

const repo = resolve(import.meta.dirname, "../..");
const skip = process.platform === "win32" || spawnSync("lefthook", ["version"]).status !== 0
  ? "requires bash and the real optional Lefthook installation" : false;
test("setup retires its dispatcher and ordinary local pushes transfer refs", { skip }, () => {
  const root = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), "hooks-"));
  const cwd = join(root, "clone");
  const run = (command, args, dir = cwd, env = {}) => {
    const out = spawnSync(command, args, { cwd: dir, encoding: "utf8", env: { ...process.env, ...env } });
    assert.equal(out.status, 0, `${command} ${args.join(" ")}\n${out.stdout}${out.stderr}`);
    return out.stdout.trim();
  };
  try {
    run("git", ["init", "--bare", "remote"], root);
    run("git", ["clone", join(root, "remote"), "clone"], root);
    run("git", ["config", "user.name", "Hook Test"]);
    run("git", ["config", "user.email", "hook@example.invalid"]);
    // Recreate the previous pre-push checks with the real hook manager.
    writeFileSync(join(cwd, "lefthook.yml"), readFileSync(join(repo, "lefthook.yml"), "utf8") + `
pre-push:
  commands:
    clippy-all:
      run: cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    test:
      run: cargo nextest run --locked --status-level fail -p allowlister
    deny:
      run: cargo deny --locked check
    llmlint-validate:
      run: llmlint validate
`);
    run("lefthook", ["install"]);
    assert.ok(existsSync(join(cwd, ".git/hooks/pre-push")));
    const generated = readFileSync(join(cwd, ".git/hooks/pre-push"), "utf8");
    copyFileSync(join(repo, "lefthook.yml"), join(cwd, "lefthook.yml"));
    mkdirSync(join(cwd, "scripts"));
    copyFileSync(join(repo, "scripts/hooks-install.sh"), join(cwd, "scripts/hooks-install.sh"));
    copyFileSync(join(repo, "justfile"), join(cwd, "justfile"));
    const install = () => run("just", ["hooks-install"]);
    install();
    assert.ok(!existsSync(join(cwd, ".git/hooks/pre-push")));
    const retained = readFileSync(join(cwd, ".git/hooks/pre-commit"), "utf8");
    for (const [index, file] of ["README.md", "src/guarded.rs"].entries()) {
      if (index === 1) run("mkdir", ["-p", "src"]);
      writeFileSync(join(cwd, file), "fixture\n");
      run("git", ["add", "README.md", "lefthook.yml"]);
      if (index === 1) run("git", ["add", "src/guarded.rs"]);
      // Author the second commit with plumbing, preserving all installed hooks.
      const tree = run("git", ["write-tree"]);
      const parent = index ? ["-p", run("git", ["rev-parse", "HEAD"])] : [];
      const commit = run("git", ["commit-tree", tree, ...parent, "-m", `fixture ${index}`]);
      run("git", ["update-ref", "HEAD", commit]);
      const trace = join(root, `push-${index}.trace`);
      run("git", ["push", "origin", "HEAD:refs/heads/main"], cwd, { GIT_TRACE: trace });
      assert.equal(run("git", ["rev-parse", "refs/heads/main"], join(root, "remote")), commit);
      assert.doesNotMatch(readFileSync(trace, "utf8"), /pre-push|clippy|nextest|cargo deny|llmlint|capture/);
    }
    for (const rerun of [false, true]) {
      if (rerun) install();
      const trace = join(root, `up-to-date-${rerun}.trace`);
      run("git", ["push", "origin", "HEAD:refs/heads/main"], cwd, { GIT_TRACE: trace });
      assert.doesNotMatch(readFileSync(trace, "utf8"), /pre-push|clippy|nextest|cargo deny|llmlint|capture/);
      assert.equal(run("git", ["rev-parse", "refs/heads/main"], join(root, "remote")), run("git", ["rev-parse", "HEAD"]));
    }
    assert.equal(readFileSync(join(cwd, ".git/hooks/pre-commit"), "utf8"), retained);
    assert.ok(!existsSync(join(cwd, ".git/hooks/pre-push")));
    if (process.getuid?.() !== 0) {
      writeFileSync(join(cwd, ".git/hooks/pre-push"), generated);
      chmodSync(join(cwd, ".git/hooks"), 0o555);
      const removal = spawnSync("just", ["hooks-install"], { cwd, encoding: "utf8" });
      assert.notEqual(removal.status, 0);
      assert.match(removal.stderr, /could not replace the hook: remove .*pre-commit: permission denied/);
      assert.equal(readFileSync(join(cwd, ".git/hooks/pre-push"), "utf8"), generated);
      chmodSync(join(cwd, ".git/hooks"), 0o755);
      install();
      assert.ok(!existsSync(join(cwd, ".git/hooks/pre-push")));
      writeFileSync(join(cwd, ".git/hooks/pre-push"), generated);
      chmodSync(join(cwd, ".git/hooks/pre-push"), 0);
      const failure = spawnSync("just", ["hooks-install"], { cwd, encoding: "utf8" });
      assert.notEqual(failure.status, 0);
      assert.match(failure.stderr, /cannot compare .*pre-push; repair its permissions, then run just hooks-install/);
      chmodSync(join(cwd, ".git/hooks/pre-push"), 0o755);
      install();
      assert.ok(!existsSync(join(cwd, ".git/hooks/pre-push")));
    }
    writeFileSync(join(cwd, ".git/hooks/pre-push"), "#!/bin/sh\n# custom hook\n");
    install();
    assert.equal(readFileSync(join(cwd, ".git/hooks/pre-push"), "utf8"), "#!/bin/sh\n# custom hook\n");
    const shared = join(root, "shared-hooks");
    mkdirSync(shared);
    for (const hook of ["pre-commit", "pre-push", "post-commit"]) {
      writeFileSync(join(shared, hook), `#!/bin/sh\n# user-owned ${hook}\n`);
    }
    run("git", ["config", "core.hooksPath", shared]);
    const refusal = spawnSync("just", ["hooks-install"], { cwd, encoding: "utf8" });
    assert.notEqual(refusal.status, 0);
    assert.match(refusal.stdout + refusal.stderr, /Custom hooks paths are not supported by default/);
    assert.match(refusal.stdout + refusal.stderr, /Force installation into current hooks path/);
    assert.equal(run("git", ["config", "--get", "core.hooksPath"]), shared);
    for (const hook of ["pre-commit", "pre-push", "post-commit"]) {
      assert.equal(readFileSync(join(shared, hook), "utf8"), `#!/bin/sh\n# user-owned ${hook}\n`);
    }

  } finally {
    if (existsSync(join(cwd, ".git/hooks"))) chmodSync(join(cwd, ".git/hooks"), 0o755);
    rmSync(root, { recursive: true, force: true });
  }
});

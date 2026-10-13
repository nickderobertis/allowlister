import assert from 'node:assert/strict';
import { test } from 'node:test';
import { copyFileSync, mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { root } from '../workflow-graph.mjs';

test('invalid tooling commands fail before reading an install lock or provisioning dependencies', () => {
  const directory = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), 'tooling-entry-'));
  try {
    const entry = join(directory, 'tooling-run.mjs');
    copyFileSync(join(root, '.github/tooling-run.mjs'), entry);
    for (const args of [[], ['unsupported']]) {
      const out = spawnSync(process.execPath, [entry, ...args], { encoding: 'utf8' });
      assert.notEqual(out.status, 0);
      assert.match(out.stderr, /usage: tooling-run.mjs test\|lint\|budgets/);
      assert.equal(out.stdout, '');
      assert.deepEqual(readdirSync(directory), ['tooling-run.mjs']);
    }
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

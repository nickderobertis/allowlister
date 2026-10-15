// Explicit integration journey: builds twice; intentionally outside per-change targets.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';

const root = resolve(import.meta.dirname, '..');
const evidence = resolve(root, 'target/pool-maintenance-evidence');

test('declared measurement runs real builds, preserves ordinary hashes, and propagates build failure', () => {
  const report = resolve(evidence, 'journey-result.json');
  rmSync(report, { force: true });
  const run = env => spawnSync('./pool-maintenance/measure.sh', [], {
    cwd: root, env: { ...process.env, ONEBUDGETSPEC_RESULT: report, ...env }, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024,
  });
  const success = run({});
  process.stdout.write(success.stdout);
  process.stderr.write(success.stderr);
  assert.equal(success.status, 0);
  assert.match(success.stdout, /untimed backlog pass \(separate evidence\):/);
  assert.match(success.stdout, /maintenance proof: [1-9]\d* alternative entries removed/);
  const read = name => JSON.parse(readFileSync(resolve(evidence, `${name}.json`), 'utf8'));
  assert.deepEqual(read('ordinary-after'), read('ordinary-before'));
  assert.ok(read('alternative').length > 0);
  assert.ok(read('removed').length > 0);

  const measured = readFileSync(report, 'utf8');
  const result = JSON.parse(measured);
  assert.ok(Number.isFinite(result.value) && result.value >= 0);
  const budget = parse(readFileSync(resolve(import.meta.dirname, 'budgets.yaml'), 'utf8')).budgets[0];
  assert.equal(budget.unit, 'seconds');
  const failure = run({ RUSTFLAGS: '--deliberately-invalid-pool-journey-flag' });
  assert.notEqual(failure.status, 0);
  assert.match(failure.stdout, /untimed backlog pass/);
  assert.doesNotMatch(failure.stdout, /maintenance proof:/);
  assert.match(failure.stderr, /deliberately-invalid-pool-journey-flag/);
  assert.equal(readFileSync(report, 'utf8'), measured);
});

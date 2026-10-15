import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, statSync, symlinkSync, readFileSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { entries, backdate, verify } from './measure.mjs';

test('real pruner removes an old alternative and preserves the ordinary build', () => {
  const root = mkdtempSync(join(tmpdir(), 'pool-maintenance-'));
  try {
    const profile = join(root, 'target', 'debug');
    const make = hash => {
      const dir = join(profile, '.fingerprint', `sample-${hash}`);
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, 'lib-sample'), 'fingerprint');
      mkdirSync(join(profile, 'deps'), { recursive: true });
      writeFileSync(join(profile, 'deps', `libsample-${hash}.rlib`), 'artifact');
    };
    make('1111111111111111');
    const alternative = entries(root);
    backdate(alternative);
    assert.equal(statSync(join(profile, 'deps', 'libsample-1111111111111111.rlib')).mtimeMs, 1000);
    make('2222222222222222');
    make('3333333333333333');
    const before = entries(root);
    execFileSync('just', ['--justfile', resolve(import.meta.dirname, '../justfile'), '--working-directory', root, 'pool-maintenance']);
    const after = entries(root);
    assert.ok(verify(before, after, alternative).length);
    assert.ok(after.has(join(profile, 'deps', 'libsample-2222222222222222.rlib')));
    assert.ok(after.has(join(profile, 'deps', 'libsample-3333333333333333.rlib')));
    assert.throws(() => verify(before, after, new Set()), /ordinary entry/);
    assert.throws(() => verify(after, after, alternative), /no alternative/);
    symlinkSync(root, join(root, 'cycle'), 'junction');
    assert.ok(!entries(root).has(join(root, 'cycle')));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

// Gate compilation changes must deliberately update the steady-state workload.
test('measurement compilation agrees with the coverage gate', () => {
  const gate = readFileSync(resolve(import.meta.dirname, '../tools/coverage/coverage.sh'), 'utf8');
  const measurement = readFileSync(resolve(import.meta.dirname, 'measure.mjs'), 'utf8');
  const gateTests = gate.match(/exec cargo llvm-cov --no-report nextest -p "\$CRATE" (.+)/)[1];
  assert.equal(gateTests, '--locked --all-features --status-level fail');
  assert.match(measurement, /cargo nextest run --target-dir target\/llvm-cov-target --workspace --locked --all-features --no-run/);
  assert.match(gate, /target\/llvm-cov-target/);
  assert.match(gate, /cargo llvm-cov --no-report run -p allowlister --bin allowlister --locked -- --version/);
  assert.match(measurement, /cargo build --target-dir target\/llvm-cov-target -p allowlister --bin allowlister --locked/);
  assert.match(measurement, /CARGO_TARGET_DIR=target\/llvm-cov-target cargo llvm-cov show-env --sh/);
});

test('a failed maintenance subprocess aborts measurement without replacing its report', () => {
  const temporary = mkdtempSync(join(tmpdir(), 'pool-maintenance-failure-'));
  try {
    const command = join(temporary, 'just');
    writeFileSync(command, '#!/bin/sh\necho maintenance-unavailable >&2\nexit 17\n');
    chmodSync(command, 0o755);
    const report = join(temporary, 'result.json');
    writeFileSync(report, 'previous-result');
    const result = spawnSync(process.execPath, [resolve(import.meta.dirname, 'measure.mjs')], {
      cwd: resolve(import.meta.dirname, '..'), encoding: 'utf8',
      env: { ...process.env, PATH: `${temporary}:${process.env.PATH}`, ONEBUDGETSPEC_RESULT: report },
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /maintenance-unavailable/);
    assert.match(result.stderr, /Command failed: just pool-maintenance/);
    assert.doesNotMatch(result.stdout, /untimed backlog pass|maintenance proof/);
    assert.equal(readFileSync(report, 'utf8'), 'previous-result');
  } finally { rmSync(temporary, { recursive: true, force: true }); }
});

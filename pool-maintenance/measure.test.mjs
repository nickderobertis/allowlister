import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, statSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { entries, backdate, verify } from './measure.mjs';

test('real pruner removes an old alternative and preserves the ordinary build', () => {
  const root = mkdtempSync(join(tmpdir(), 'pool-maintenance-'));
  try {
    const profile = join(root, 'debug');
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
    const before = entries(root);
    execFileSync('cargo-prune-superseded', ['--keep-within', '2d', root]);
    const after = entries(root);
    assert.ok(verify(before, after, alternative).length);
    assert.ok(after.has(join(profile, 'deps', 'libsample-2222222222222222.rlib')));
    assert.throws(() => verify(before, after, new Set()), /ordinary entry/);
    assert.throws(() => verify(after, after, alternative), /no alternative/);
    symlinkSync(root, join(root, 'cycle'), 'junction');
    assert.ok(!entries(root).has(join(root, 'cycle')));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

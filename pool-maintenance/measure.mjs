// Real Cargo/pruner journey. Only the final recipe inherits the SDK result path.
import { execFileSync } from 'node:child_process';
import { existsSync, lstatSync, readdirSync, utimesSync, mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { pathToFileURL } from 'node:url';

export function entries(directory) {
  const result = new Set();
  function visit(path) {
    if (!existsSync(path)) return;
    const stat = lstatSync(path);
    if (stat.isSymbolicLink()) return;
    result.add(path);
    if (stat.isDirectory()) for (const name of readdirSync(path)) visit(join(path, name));
  }
  visit(directory);
  return result;
}

export function backdate(created) {
  // Epoch time also puts alternatives behind an already-warm ordinary build.
  for (const path of created) utimesSync(path, 1, 1);
}

export function verify(before, after, alternative) {
  const removed = [...before].filter(path => !after.has(path));
  if (!removed.length) throw new Error('maintenance removed no alternative build entries');
  for (const path of removed) {
    if (!alternative.has(path)) throw new Error(`maintenance removed an ordinary entry: ${path}`);
  }
  return removed;
}

export function main() {
  const root = resolve(import.meta.dirname, '..');
  if (process.cwd() !== root) throw new Error('run measure.sh from this repository');
  const evidence = join(root, 'target', 'pool-maintenance-evidence');
  mkdirSync(evidence, { recursive: true });
  const env = { ...process.env };
  delete env.ONEBUDGETSPEC_RESULT;
  const run = (command, args, environment = env) => execFileSync(command, args, { env: environment, stdio: 'inherit' });
  const backlogStart = performance.now();
  // llmlint: ignore[changed_behavior_has_e2e] The failed-maintenance subprocess test proves the shared execFileSync runner propagates failures without replacing the report; the real measurement journey proves the complete pass.
  run('just', ['pool-maintenance']);
  console.log(`untimed backlog pass (separate evidence): ${(performance.now() - backlogStart) / 1000} seconds`);
  const baseline = entries(join(root, 'target'));
  // The gate's test compilation uses llvm-cov + nextest for both workspace crates.
  // A distinct metadata salt creates one new configuration, without source edits.
  const alternativeEnv = { ...env, RUSTFLAGS: `${env.RUSTFLAGS ?? ''} -C metadata=pool-${randomUUID()}`.trim() };
  const build = environment => run('bash', ['-euo', 'pipefail', '-c', `
    coverage_env="$(CARGO_TARGET_DIR=target/llvm-cov-target cargo llvm-cov show-env --sh)"
    eval "$coverage_env"
    cargo build --target-dir target/llvm-cov-target -p allowlister --bin allowlister --locked
    cargo nextest run --target-dir target/llvm-cov-target --workspace --locked --all-features --no-run
  `], environment);
  build(alternativeEnv);
  const alternative = new Set([...entries(join(root, 'target'))].filter(path => !baseline.has(path)));
  // llmlint: ignore[changed_behavior_has_e2e] The subprocess test proves unhandled measurement failures leave the report unchanged, and the real journey proves alternative creation; this guard throws before report publication through the same process failure boundary.
  if (!alternative.size) throw new Error('alternative build created no entries');
  backdate(alternative);
  build(env);
  const before = entries(join(root, 'target'));
  const fingerprints = paths => [...paths].filter(path => /\/\.fingerprint\/[^/]+$/.test(path)).sort();
  const ordinary = fingerprints(before).filter(path => !alternative.has(path));
  writeFileSync(join(evidence, 'ordinary-before.json'), JSON.stringify(ordinary, null, 2));
  writeFileSync(join(evidence, 'alternative.json'), JSON.stringify(fingerprints(alternative), null, 2));
  // llmlint: ignore[changed_behavior_has_e2e] The failed-maintenance subprocess test proves failure propagation through this shared execFileSync runner; the real measurement journey proves final report publication.
  run('just', ['pool-maintenance'], process.env);
  const after = entries(join(root, 'target'));
  const removed = verify(before, after, alternative);
  writeFileSync(join(evidence, 'ordinary-after.json'), JSON.stringify(fingerprints(after).filter(path => !alternative.has(path)), null, 2));
  writeFileSync(join(evidence, 'removed.json'), JSON.stringify(removed, null, 2));
  console.log(`maintenance proof: ${removed.length} alternative entries removed; ${ordinary.length} ordinary metadata hashes retained; evidence: ${evidence}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();

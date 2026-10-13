import assert from 'node:assert/strict';
import { test } from 'node:test';
import { copyFileSync, mkdtempSync, readdirSync, rmSync, mkdirSync, writeFileSync, readFileSync, chmodSync, existsSync } from 'node:fs';
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

test('real tooling entry provisions the locked install, reuses it, and repairs changed or missing dependencies', () => {
  const directory=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(),'tooling-cache-'));
  const tooling=join(directory,'.github');const bin=join(directory,'bin');
  try {
    for(const path of [tooling,bin,join(tooling,'scripts'),join(tooling,'tests'),join(tooling,'workflows'),join(directory,'scripts')]) mkdirSync(path,{recursive:true});
    for(const file of ['tooling-run.mjs','package.json','package-lock.json']) copyFileSync(join(root,'.github',file),join(tooling,file));
    for(const file of readdirSync(join(root,'.github/workflows')).filter(file=>file.startsWith('e2e-'))) copyFileSync(join(root,'.github/workflows',file),join(tooling,'workflows',file));
    copyFileSync(join(root,'scripts/check-e2e-matrix.sh'),join(directory,'scripts/check-e2e-matrix.sh'));
    const record=join(directory,'npm-calls.jsonl');
    const installer=join(directory,'registry-transport.mjs');
    // npm is the third-party provisioning boundary. The entry, locked-version
    // decisions, and downstream matrix gate all run as their real processes.
    writeFileSync(installer, `import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
const args=process.argv.slice(2);appendFileSync(process.env.INSTALL_RECORD,JSON.stringify(args)+'\\n');
if(process.env.INSTALL_FAILURE) {console.error('registry unavailable');process.exit(9);}
const directory=join(args[args.indexOf('--prefix')+1],'node_modules/@actions/expressions/dist');
mkdirSync(directory,{recursive:true});writeFileSync(join(directory,'index.js'),'// registry transport fixture\\n');
`);
    if(process.platform==='win32') writeFileSync(join(bin,'npm.cmd'),'@echo off\n"%INSTALL_NODE%" "%INSTALL_SCRIPT%" %*\n');
    else {writeFileSync(join(bin,'npm'),'#!/bin/sh\nexec "$INSTALL_NODE" "$INSTALL_SCRIPT" "$@"\n');chmodSync(join(bin,'npm'),0o755);}
    const env={...process.env,PATH:bin+(process.platform==='win32'?';':':')+process.env.PATH,INSTALL_NODE:process.execPath,INSTALL_SCRIPT:installer,INSTALL_RECORD:record};
    const run=(extra={})=>spawnSync(process.execPath,[join(tooling,'tooling-run.mjs'),'lint'],{encoding:'utf8',env:{...env,...extra}});
    const calls=()=>existsSync(record)?readFileSync(record,'utf8').trim().split('\n').map(line=>JSON.parse(line)):[];
    const failed=run({INSTALL_FAILURE:'true'});assert.equal(failed.status,9);assert.match(failed.stderr,/registry unavailable/);
    assert.equal(existsSync(join(tooling,'node_modules/.locked-install')),false);
    const fresh=run();assert.equal(fresh.status,0,fresh.stderr);assert.equal(calls().length,2);
    assert.equal(calls()[1][0],'ci');assert.ok(calls()[1].includes('--ignore-scripts'));
    assert.ok(calls()[1].includes('--no-audit'));assert.ok(calls()[1].includes('--no-fund'));
    const reused=run();assert.equal(reused.status,0,reused.stderr);assert.equal(calls().length,2);
    const stamp=readFileSync(join(tooling,'node_modules/.locked-install'),'utf8');
    writeFileSync(join(tooling,'package-lock.json'),readFileSync(join(tooling,'package-lock.json'),'utf8')+'\n');
    const refreshed=run();assert.equal(refreshed.status,0,refreshed.stderr);assert.equal(calls().length,3);
    assert.notEqual(readFileSync(join(tooling,'node_modules/.locked-install'),'utf8'),stamp);
    rmSync(join(tooling,'node_modules/@actions/expressions/dist/index.js'));
    const repaired=run();assert.equal(repaired.status,0,repaired.stderr);assert.equal(calls().length,4);
    assert.equal(existsSync(join(tooling,'node_modules/@actions/expressions/dist/index.js')),true);
  } finally {rmSync(directory,{recursive:true,force:true});}
});

// Real production builder and installer, no compiled-binary doubles. Keep this
// behind the release-check edge rather than every workflow graph test run.
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, rmSync } from 'node:fs';
import { test } from 'node:test';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
const root=fileURLToPath(new URL('../../',import.meta.url));
test('production host archive builds, installs and runs, and rejects corrupted transport',()=>{
  const host=execFileSync('rustc',['-vV'],{encoding:'utf8'}).match(/^host: (.+)$/m)[1];
  const target=host.replace('-unknown-linux-gnu','-unknown-linux-musl');
  const version=readFileSync(join(root,'Cargo.toml'),'utf8').match(/^version = "([^"]+)"/m)[1];
  const tag='v'+version;
  const run=(script,args)=>spawnSync('bash',[script,...args],{cwd:root,encoding:'utf8',maxBuffer:8*1024*1024});
  const build=spawnSync('just',['dist-build',target,tag],{cwd:root,encoding:'utf8',maxBuffer:8*1024*1024});assert.equal(build.status,0,build.stderr);
  const basename=`allowlister-${tag}-${target}`;
  const checksum=join(root,'dist',basename+'.sha256');
  try {
    const installed=run('scripts/verify-release.sh',[target,tag]);assert.equal(installed.status,0,installed.stderr);
    const binary=join(root,'target',target,'release',target.includes('windows')?'allowlister.exe':'allowlister');
    const digest=createHash('sha256').update(readFileSync(binary)).digest('hex');
    assert.ok(installed.stdout.includes(`binary_sha256 ${target} ${digest}`));
    const ext=target.includes('windows')?'zip':'tar.gz';
    if(ext==='tar.gz') {
      const contents=execFileSync('tar',['-tzf',join(root,'dist',basename+'.'+ext)],{encoding:'utf8'}).trim().split('\n').sort();
      assert.deepEqual(contents,['CHANGELOG.md','LICENSE','README.md','allowlister']);
    }
    writeFileSync(checksum,'0'.repeat(64)+'  '+basename+'.tar.gz\n');
    const corrupt=run('scripts/verify-release.sh',[target,tag]);assert.notEqual(corrupt.status,0);assert.match(corrupt.stderr,/checksum mismatch/);
    const mismatch=spawnSync('just',['dist-build',target,'v999.0.0'],{cwd:root,encoding:'utf8'});assert.notEqual(mismatch.status,0);assert.match(mismatch.stderr,/does not match/);
  } finally {
    rmSync(checksum,{force:true});rmSync(join(root,'dist',basename+(target.includes('windows')?'.zip':'.tar.gz')),{force:true});
  }
});

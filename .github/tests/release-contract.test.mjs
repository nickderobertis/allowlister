import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFileSync, mkdirSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { loadWorkflows, root, evaluate, resolveRun } from '../workflow-graph.mjs';
const workflows=loadWorkflows();
const contract=JSON.parse(readFileSync(join(root,'.github/tests/fixtures/release-contract.json'),'utf8'));
// llmlint: ignore[changed_behavior_has_e2e] The production host-native journey builds, installs and executes a real artifact. Darwin/Windows compatibility is proved by the artifact contract here and native install/run commands before release upload; this Linux host cannot execute those binaries and runs no extra paid platform probes.
test('release graph preserves every platform and archive; one compiled target input precedes Linux publication',()=>{
  const release=workflows['release.yml'];const jobs=release.jobs;
  const darwin=jobs.darwin.steps.find(s=>s.run?.includes('for target in')).run;
  const targets=[...jobs.build.strategy.matrix.include.map(c=>c.target),...darwin.match(/for target in ([^;]+);/)[1].split(' ')].sort();
  assert.equal(contract.schemaVersion,1);
  assert.deepEqual(targets,contract.platforms.map(p=>p.target).sort());
  for(const platform of contract.platforms) assert.equal(platform.architecture,platform.target.split('-')[0]);
  assert.equal(contract.archiveName,'allowlister-{tag}-{target}.{archive}');
  assert.equal(contract.checksumName,'allowlister-{tag}-{target}.sha256');
  assert.match(darwin,/just dist-build "\$target" "\$TAG"\n\s+bash scripts\/verify-release.sh "\$target" "\$TAG"/);
  assert.equal(jobs.darwin['runs-on'],'macos-15');
  assert.ok(jobs.darwin.steps.some(s=>s.run==='rustup target add x86_64-apple-darwin aarch64-apple-darwin'));
  assert.ok(jobs.darwin.steps.some(s=>s.run==='softwareupdate --install-rosetta --agree-to-license'));
  assert.deepEqual(jobs.upload.needs,['build','darwin']);assert.equal(jobs.upload['runs-on'],'ubuntu-latest');
  const download=jobs.upload.steps.find(s=>s.uses?.startsWith('actions/download-artifact@'));
  assert.equal(download.with['run-id'],undefined,'download is scoped to this exact workflow run');
  assert.equal(download.with.pattern,'release-*');
  for(const job of [jobs.build,jobs.darwin]) {
    assert.equal(job.needs,'test');
    assert.ok(job.steps.find(s=>s.uses?.startsWith('actions/upload-artifact@')).with['if-no-files-found']==='error');
    assert.equal(job.permissions?.contents,undefined,'builders have read-only inherited permission');
  }
  const builder=readFileSync(join(root,'scripts/dist.sh'),'utf8');
  assert.match(builder,/cargo build --release --locked --target "\$target"/);
  assert.match(builder,/cp "target\/\$\{target\}\/release\/\$\{binary\}" "\$stage\/"/);
  assert.match(builder,/archive="\$\{bin_name\}-\$\{tag\}-\$\{target\}\.zip"/);
  assert.match(builder,/archive="\$\{bin_name\}-\$\{tag\}-\$\{target\}\.tar.gz"/);
  assert.match(builder,/\$outdir\/\$\{bin_name\}-\$\{tag\}-\$\{target\}\.sha256/);
  for(const file of contract.contents.filter(f=>f!=='allowlister')) assert.ok(builder.includes(file));
  const verify=readFileSync(join(root,'scripts/verify-release.sh'),'utf8');
  assert.match(verify,/sh scripts\/install.sh --version "\$tag" --to/);
  assert.match(verify,/cmp -s "target\/\$target\/release/);
  assert.match(verify,/arch -x86_64 "\$binary" --version/);
  assert.match(verify,/Darwin execution requires macOS/);
  for(const platform of contract.platforms) assert.ok(verify.includes(platform.target),platform.target);
  for(const identity of ['x86-64','arm64','Mach-O','ELF','PE32+']) assert.ok(verify.includes(identity));
  const offHost=spawnSync('bash',['scripts/verify-release.sh','x86_64-apple-darwin','v0.1.0'],{cwd:root,encoding:'utf8'});
  if(process.platform==='linux') {assert.notEqual(offHost.status,0);assert.match(offHost.stderr,/Darwin execution requires macOS/);}
  // Distribution is archive-only; both Darwin archives reuse a compiled input.
  assert.deepEqual(contract.packageForms,['archive']);
});
test('all former Mac harness commands keep credentials, installation paths, and strict failures',()=>{
  const mac=workflows['ci.yml'].jobs.macos;
  const sourcePins=new Set(Object.entries(workflows).filter(([file])=>file.startsWith('e2e-')).flatMap(([,wf])=>wf.jobs.live.steps.map(s=>s.run?.match(/cargo install --git https:\/\/github.com\/nickderobertis\/oneharness --tag (\S+) --locked/)?.[1]).filter(Boolean)));
  assert.equal(sourcePins.size,1,'sequential Mac installation requires one reconciled harness driver pin');
  const context={github:{repository:'nickderobertis/allowlister',event_name:'pull_request',event:{pull_request:{head:{repo:{full_name:'nickderobertis/allowlister'}}}}},inputs:{harness:'all'}};
  for(const harness of ['codex','claude','copilot','crush','cursor','goose','opencode','qwen']) {
    const old=workflows[`e2e-${harness}.yml`].jobs.live;
    const source=old.steps.find(s=>s.run?.includes(`just test-${harness}`));
    const moved=mac.steps.find(s=>s.run?.includes(`just test-${harness}`));
    assert.equal(moved.run,source.run,harness);assert.deepEqual(moved.env,source.env,harness);
    assert.equal(Boolean(evaluate(moved.if,context)),['codex','claude'].includes(harness),harness);
    const manual={...context,github:{...context.github,event_name:'workflow_dispatch'}};
    assert.equal(evaluate(moved.if,manual),true,harness);
    assert.equal(evaluate(moved.if,{...context,github:{...context.github,event:{pull_request:{head:{repo:{full_name:'someone/fork'}}}}}}),false,harness);
    // Run the copied production credential-validation shell script with the
    // credential absent. Only GitHub expression/secret injection is doubled.
    const validation=old.steps.find(s=>s.name?.startsWith('Verify'));
    const copied=mac.steps.find(s=>s.run===validation.run);assert.ok(copied,harness);
    const script=copied.run.replaceAll('${{ github.repository }}','nickderobertis/allowlister');
    const out=spawnSync('bash',['-eo','pipefail','-c',script],{encoding:'utf8',env:{...process.env,...Object.fromEntries(Object.keys(copied.env).map(k=>[k,'']))}});
    assert.notEqual(out.status,0,`${harness} must fail without its required credential`);
    const executable=text=>text?.split('\n').filter(line=>!line.trim().startsWith('#')).join('\n');
    for(const step of old.steps.filter(s=>s.name?.startsWith('Install ') || s.name?.startsWith('Show cursor'))) {
      if(step.run?.includes('cargo install')) {
        const install=step.run.split('\n').find(line=>line.includes('cargo install')).trim();
        assert.ok(mac.steps.some(m=>m.run?.includes(install)),`${harness}: shared oneharness pin`);
        continue;
      }
      assert.ok(mac.steps.some(m=>executable(m.run)===executable(step.run) && m.uses===step.uses),`${harness}: ${step.name}`);
    }
  }
});
test('real verifier rejects unsupported targets and unsafe/unversioned tags',()=>{
  for(const [target,tag,reason] of [['unsupported','v0.1.0',/unsupported target/],['x86_64-unknown-linux-musl','../bad',/invalid release tag/]]) {
    const out=spawnSync('bash',['scripts/verify-release.sh',target,tag],{cwd:root,encoding:'utf8'});
    assert.notEqual(out.status,0);assert.match(out.stderr,reason);
  }
});
test('actual Linux publication command sends only staged exact-tag assets to forge transport',()=>{
  const script=workflows['release.yml'].jobs.upload.steps.find(s=>s.run).run;
  const directory=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(),'release-upload-'));
  mkdirSync(join(directory,'dist'));
  const names=contract.platforms.flatMap(({target,archive})=>[contract.archiveName.replace('{tag}','v0.1.0').replace('{target}',target).replace('{archive}',archive),contract.checksumName.replace('{tag}','v0.1.0').replace('{target}',target)]).sort();
  for(const name of names) writeFileSync(join(directory,'dist',name),'staged forge transport fixture');
  try {
    const env={...process.env,TAG:'v0.1.0',GITHUB_REPOSITORY:'nickderobertis/allowlister'};
    const out=spawnSync('bash',['-eo','pipefail','-c',`function gh() { printf '%s\\n' "$@"; }; ${script}`],{cwd:directory,encoding:'utf8',env});
    assert.equal(out.status,0,out.stderr);const args=out.stdout.trim().split('\n');
    assert.deepEqual(args.slice(0,3),['release','upload','v0.1.0']);assert.deepEqual(args.slice(-2),['--repo','nickderobertis/allowlister']);
    assert.deepEqual(args.slice(3,-2),names.map(name=>'dist/'+name));
    const failed=spawnSync('bash',['-eo','pipefail','-c',`function gh() { return 9; }; ${script}`],{cwd:directory,encoding:'utf8',env});
    assert.equal(failed.status,9);
  } finally {rmSync(directory,{recursive:true,force:true});}
});

test('all production platform declarations reconcile exactly with release targets', () => {
  const targets = contract.platforms.map(platform => platform.target).sort();
  const builder = readFileSync(join(root, 'scripts/dist.sh'), 'utf8');
  const buildTargets = builder.match(/TARGETS=\(([\s\S]*?)\)/)[1].trim().split(/\s+/);
  assert.deepEqual(buildTargets.sort(), targets);
  const verifier = readFileSync(join(root, 'scripts/verify-release.sh'), 'utf8');
  const verifyTargets = verifier.match(/case "\$target" in\n\s*([^\n]+)\) ;;/)[1].split('|');
  assert.deepEqual(verifyTargets.sort(), targets);
  const toolchain = readFileSync(join(root, 'rust-toolchain.toml'), 'utf8');
  const toolchainTargets = [...toolchain.match(/targets\s*=\s*\[([\s\S]*?)\]/)[1].matchAll(/"([^"]+)"/g)].map(match => match[1]);
  assert.deepEqual(toolchainTargets.sort(), [...targets, 'x86_64-unknown-linux-gnu'].sort(), 'only the documented Linux dev host is outside the release set');
  const installer = readFileSync(join(root, 'scripts/install.sh'), 'utf8');
  const detect = installer.match(/detect_target\(\) \{[\s\S]*?\n\}/)[0];
  const operatingSystems = [...new Set([...detect.matchAll(/os_part="([^"]+)"/g)].map(match => match[1]))];
  const architectures = [...new Set([...detect.matchAll(/arch_part="([^"]+)"/g)].map(match => match[1]))];
  const names = new Map([['unknown-linux-musl', 'Linux'], ['apple-darwin', 'Darwin'], ['pc-windows-msvc', 'MINGW64_NT']]);
  const installedTargets = [];
  for (const os of operatingSystems) for (const architecture of architectures) {
    assert.ok(names.has(os), `unresolved installer operating system: ${os}`);
    const script = `err() { echo "$*" >&2; exit 1; }; uname() { if [ "$1" = -s ]; then echo "$INSTALL_OS"; else echo "$INSTALL_ARCH"; fi; }; BIN=allowlister; ${detect}; detect_target; printf '%s %s' "$TARGET" "$EXT"`;
    const out = spawnSync('bash', ['-eo', 'pipefail', '-c', script], { encoding: 'utf8', env: { ...process.env, INSTALL_OS: names.get(os), INSTALL_ARCH: architecture } });
    if (os === 'pc-windows-msvc' && architecture === 'aarch64') {
      assert.notEqual(out.status, 0); assert.match(out.stderr, /no prebuilt Windows binary/);
    } else {
      assert.equal(out.status, 0, out.stderr);
      const [target, archive] = out.stdout.split(' '); installedTargets.push(target);
      assert.equal(archive, contract.platforms.find(platform => platform.target === target)?.archive);
    }
  }
  assert.deepEqual(installedTargets.sort(), targets, 'actual installer target selection matches the release graph');
});

import assert from 'node:assert/strict';
import { test, after } from 'node:test';
import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync, readFileSync, rmSync, cpSync, copyFileSync, symlinkSync, mkdtempSync } from 'node:fs';
import { join, dirname, basename } from 'node:path';
import { tmpdir } from 'node:os';
import { journeys } from '../macos-journeys.mjs';
import { root, sourceDigest, loadWorkflows, resolveRun, evaluate, triggered } from '../workflow-graph.mjs';
import { runPolicy } from '../run-policy.mjs';
import { history } from './history.mjs';
const required=['coverage','deps & security','llmlint','pr-title','test (macos-latest)','test (ubuntu-latest)','test (windows-latest)'];
const telemetry=journeys();
const h=history();after(()=>rmSync(h.dir,{recursive:true,force:true}));
test('actual workflow event/matrix graph records the complete workload and required contexts', () => {
  const classes=new Set(telemetry.scenarios.map(s=>s.eventClass));assert.equal(classes.size,12);
  for(const scenario of telemetry.scenarios) {
    const mac=scenario.runs.flatMap(r=>r.jobs.filter(j=>j.macos).map(j=>({...j,workflow:r.workflow})));
    if(scenario.eventName==='pull_request') {
      const names=scenario.runs.flatMap(r=>r.jobs.map(j=>j.name)).filter(n=>required.includes(n));
      assert.deepEqual(names.sort(),[...required].sort(),scenario.id);
      if(scenario.classification.macosRequired) assert.deepEqual(mac.map(j=>`${j.workflow}:${j.job}`),['ci.yml:macos'],scenario.id);
      else assert.deepEqual(mac,[],scenario.id);
    } else if(scenario.eventName==='release') assert.deepEqual(mac.map(j=>`${j.workflow}:${j.job}`),['release.yml:darwin'],scenario.id);
    else if(scenario.eventClass==='manual-opt-in') assert.deepEqual(mac.map(j=>`${j.workflow}:${j.job}`),['ci.yml:macos'],scenario.id);
    else assert.deepEqual(mac,[],scenario.id);
  }
  // Routing assertions identify allowed locations, without duplicating budget
  // tolerance comparisons in tests. The SDK alone judges numeric measurements.
  for(const lifecycle of telemetry.lifecycles) assert.deepEqual(lifecycle.runs.filter(r=>r.jobs.some(j=>j.macos)).map(r=>r.workflow),['ci.yml','release.yml']);
  mkdirSync(join(root,'.github/.telemetry'),{recursive:true});writeFileSync(join(root,'.github/.telemetry/macos-policy.json'),JSON.stringify(telemetry,null,2)+'\n');
});
test('forged/mixed/unknown PRs request the real Mac lane, including forks', () => {
  const workflows=loadWorkflows();
  for(const [payload,head] of [[h.event(),h.heads.version],[h.event(false,'release-plz-test'),h.heads.mixed],[h.event(true,'release-plz-test'),h.heads.version],['{bad',h.heads.version]]) {
    const classification=h.classify(payload,head);
    const scenario={eventName:'pull_request',event:typeof payload==='object'?payload:h.event(),ref:'refs/pull/1/merge',head,classification,files:['Cargo.toml'],inputs:{}};
    scenario.event.action='synchronize';
    scenario.rawEvent=typeof payload==='string'?payload:undefined;
    scenario.event.pull_request.head.sha=head;
    scenario.policyOutputs=runPolicy(workflows['ci.yml'],scenario,h);
    assert.ok(resolveRun(workflows,'ci.yml',scenario).some(j=>j.job==='macos' && j.macos));
  }
});
test('carrier always runs on failure/cancellation and Mac lane depends on a successful classifier', () => {
  const jobs=loadWorkflows()['ci.yml'].jobs;
  assert.equal(jobs['macos-result'].if,'always()');
  assert.deepEqual(jobs['macos-result'].needs,['policy','macos']);
  for(const result of ['failure','cancelled','skipped','success']) assert.equal(evaluate(jobs['macos-result'].if,{needs:{policy:{result},macos:{result}}}),true);
  assert.equal(jobs.macos.needs,'policy');
  assert.equal(jobs.macos.if,"needs.policy.outputs.macosRequired == 'true'");
});
test('budget reporters consume recorded telemetry and reject missing/stale input', () => {
  // Recording is independent of test execution order; telemetry has no cost limits.
  const path=join(h.dir,'telemetry.json');writeFileSync(path,JSON.stringify({...telemetry,sourceDigest:sourceDigest()}));
  for(const metric of ['jobs-per-run','runs-per-change']) {
    const result=join(h.dir,`${metric}.json`);
    const out=spawnSync(process.execPath,[join(root,'.github/macos-budget.mjs'),'--metric',metric,'--telemetry',path],{encoding:'utf8',env:{...process.env,ONEBUDGETSPEC_RESULT:result}});
    assert.equal(out.status,0,out.stderr);
    const reported=JSON.parse(readFileSync(result,'utf8'));
    assert.equal(reported.value,telemetry.measurements[metric==='jobs-per-run'?'jobsPerRun':'runsPerChange']);
    const diagnostic=JSON.parse(reported.detail);assert.equal(diagnostic.sourceDigest,sourceDigest());
    if(metric==='jobs-per-run') {
      assert.deepEqual(diagnostic.breakdown.map(row=>row.event),telemetry.scenarios.map(scenario=>scenario.id));
      const published=diagnostic.breakdown.find(row=>row.event==='published:version:same:archives');
      const darwin=published.runs.find(run=>run.workflow==='release.yml').jobs.find(job=>job.job==='darwin');
      const declared=loadWorkflows()['release.yml'].jobs.darwin.steps.find(step=>step.run?.includes('for target in')).run.match(/for target in ([^;]+);/)[1].split(' ');
      assert.deepEqual(darwin.targets,declared);assert.equal(darwin.runner,'macos-15');
      const pr=diagnostic.breakdown.find(row=>row.event==='pr-synchronize:source:same:archives');
      assert.equal(pr.runs.find(run=>run.workflow==='ci.yml').jobs.find(job=>job.job==='macos').name,'Mac proofs');
    } else {
      assert.deepEqual(diagnostic.breakdown.map(row=>row.id),telemetry.lifecycles.map(lifecycle=>lifecycle.id));
      for(const lifecycle of diagnostic.breakdown) {
        assert.deepEqual(lifecycle.runs.map(run=>run.workflow),['ci.yml','release.yml']);
        assert.ok(lifecycle.runs.every(run=>typeof run.event==='string' && typeof run.head==='string'));
      }
    }
    const env={...process.env};delete env.ONEBUDGETSPEC_RESULT;
    const fallback=spawnSync(process.execPath,[join(root,'.github/macos-budget.mjs'),'--metric',metric,'--telemetry',path],{encoding:'utf8',env,maxBuffer:4*1024*1024});
    assert.equal(fallback.status,0,fallback.stderr);
    const printed=JSON.parse(fallback.stdout);assert.equal(printed.actual,reported.value);
    assert.deepEqual(printed.detail,diagnostic.breakdown);
  }
  for(const invalid of [{...telemetry,scenarios:null},{...telemetry,measurements:{jobsPerRun:-1,runsPerChange:0}},{...telemetry,measurements:{jobsPerRun:99,runsPerChange:99}}]) {
    writeFileSync(path,JSON.stringify({...invalid,sourceDigest:sourceDigest()}));
    const out=spawnSync(process.execPath,[join(root,'.github/macos-budget.mjs'),'--metric','jobs-per-run','--telemetry',path],{encoding:'utf8'});
    assert.notEqual(out.status,0);assert.match(out.stderr,/incomplete|inconsistent/);
  }
  for(const metric of ['constructor','__proto__','unsupported']) {
    writeFileSync(path,JSON.stringify({...telemetry,sourceDigest:sourceDigest()}));
    const out=spawnSync(process.execPath,[join(root,'.github/macos-budget.mjs'),'--metric',metric,'--telemetry',path],{encoding:'utf8'});
    assert.notEqual(out.status,0);assert.match(out.stderr,/invalid metric/);
  }
  writeFileSync(path,JSON.stringify({...telemetry,sourceDigest:'stale'}));
  const stale=spawnSync(process.execPath,[join(root,'.github/macos-budget.mjs'),'--metric','jobs-per-run','--telemetry',path],{encoding:'utf8'});assert.notEqual(stale.status,0);assert.match(stale.stderr,/stale/);
  assert.notEqual(spawnSync(process.execPath,[join(root,'.github/macos-budget.mjs'),'--metric','jobs-per-run','--telemetry',join(h.dir,'missing')]).status,0);
});

test('a real workflow input edit invalidates recorded telemetry through the production reporter', () => {
  const fixture=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(),'digest-input-'));
  try {
    cpSync(join(root,'.github'),join(fixture,'.github'),{recursive:true,filter:path=>!['node_modules','.telemetry'].includes(basename(path))});
    symlinkSync(join(root,'.github/node_modules'),join(fixture,'.github/node_modules'),process.platform==='win32'?'junction':'dir');
    for(const file of ['scripts/check-e2e-matrix.sh','scripts/dist.sh','scripts/verify-release.sh','release-plz.toml','rust-toolchain.toml','justfile','.tool-versions']) {
      mkdirSync(dirname(join(fixture,file)),{recursive:true});copyFileSync(join(root,file),join(fixture,file));
    }
    const digest=spawnSync(process.execPath,['--input-type=module','-e',"import {sourceDigest} from './.github/workflow-graph.mjs'; console.log(sourceDigest());"],{cwd:fixture,encoding:'utf8'});
    assert.equal(digest.status,0,digest.stderr);
    const recorded=join(fixture,'.github/.telemetry/macos-policy.json');mkdirSync(dirname(recorded),{recursive:true});
    writeFileSync(recorded,JSON.stringify({...telemetry,sourceDigest:digest.stdout.trim()}));
    const env={...process.env,ONEBUDGETSPEC_RESULT:join(fixture,'measurement.json')};
    const run=()=>spawnSync(process.execPath,['.github/macos-budget.mjs','--metric','jobs-per-run','--telemetry',recorded],{cwd:fixture,encoding:'utf8',env});
    const fresh=run();assert.equal(fresh.status,0,fresh.stderr);
    const workflow=join(fixture,'.github/workflows/ci.yml');
    writeFileSync(workflow,readFileSync(workflow,'utf8')+'\n# changed workflow source input\n');
    const changed=run();assert.notEqual(changed.status,0);assert.match(changed.stderr,/missing\/stale Mac telemetry/);
  } finally {rmSync(fixture,{recursive:true,force:true});}
});

test('production reporter rejects malformed records and inconsistent lifecycle totals', () => {
  const path=join(h.dir,'malformed-telemetry.json');
  const job=value=>value.scenarios[0].runs[0].jobs[0];
  const cases=[
    [value=>{value.scenarios[0].runs=null;},/invalid telemetry runs/],
    [value=>{value.scenarios[0].runs[0].workflow=null;},/invalid telemetry workflow/],
    [value=>{value.scenarios[0].runs[0].jobs=null;},/invalid telemetry workflow/],
    [value=>{job(value).job=null;},/invalid telemetry job/],
    [value=>{job(value).macos='true';},/invalid telemetry job/],
    [value=>{job(value).state='unknown';},/invalid telemetry job/],
    [value=>{job(value).state='skipped';job(value).macos=true;},/skipped telemetry job cannot spend Mac/],
    [value=>{job(value).runner='unresolved';},/inconsistent telemetry runner/],
    [value=>{job(value).runner=[];},/inconsistent telemetry runner/],
    [value=>{job(value).runner='macos-latest';job(value).macos=false;},/inconsistent telemetry runner/],
    [value=>{value.scenarios[0].id=null;},/invalid telemetry event/],
    [value=>{value.scenarios[0].eventName=null;},/invalid telemetry event/],
    [value=>{value.lifecycles[0].macosRuns+=1;},/inconsistent telemetry lifecycle/],
  ];
  for(const [mutate,message] of cases) {
    const value=structuredClone(telemetry);value.sourceDigest=sourceDigest();mutate(value);writeFileSync(path,JSON.stringify(value));
    const out=spawnSync(process.execPath,[join(root,'.github/macos-budget.mjs'),'--metric','jobs-per-run','--telemetry',path],{encoding:'utf8'});
    assert.notEqual(out.status,0);assert.match(out.stderr,message);assert.equal(out.stdout,'');
  }
});

test('actual workflow graphs reject unresolved interpolation, bad matrices, schedules, calls and dependencies', () => {
  const workflows=loadWorkflows();
  const event=h.event();event.action='synchronize';event.pull_request.head.sha=h.heads.source;
  const scenario={eventName:'pull_request',event,ref:'refs/pull/1/merge',head:h.heads.source,inputs:{}};
  scenario.policyOutputs=runPolicy(workflows['ci.yml'],scenario,h);
  const cases=[
    [graph=>{graph['ci.yml'].jobs.macos.name='Mac ${{ matrix.unknown }}';},/unresolved interpolation/],
    [graph=>{graph['ci.yml'].jobs.macos.strategy={matrix:{os:[]}};},/unresolved\/empty matrix axis/],
    [graph=>{graph['ci.yml'].jobs.macos.strategy={matrix:{os:'macos-latest'}};},/unresolved\/empty matrix axis/],
    [graph=>{graph['ci.yml'].jobs.macos={needs:'policy',uses:'./.github/workflows/ci.yml'};},/recursive workflow call/],
    [graph=>{graph['ci.yml'].jobs.macos.needs='missing';},/missing prerequisite/],
    [graph=>{graph['ci.yml'].jobs.policy.needs='macos';},/cyclic job dependencies/],
  ];
  for(const [mutate,message] of cases) {const graph=structuredClone(workflows);mutate(graph);assert.throws(()=>resolveRun(graph,'ci.yml',scenario),message);}
  for(const schedule of [{cron:'* * * * *'},[{cron:1}],[{cron:'* * * * *',unsupported:true}]]) {
    const graph=structuredClone(workflows);graph['ci.yml'].on.schedule=schedule;
    assert.throws(()=>triggered(graph['ci.yml'],{...scenario,eventName:'schedule'}),/unsupported schedule trigger/);
  }
});

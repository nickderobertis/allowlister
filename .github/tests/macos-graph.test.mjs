import assert from 'node:assert/strict';
import { test, after } from 'node:test';
import { spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { journeys } from '../macos-journeys.mjs';
import { root, sourceDigest, loadWorkflows, resolveRun, evaluate } from '../workflow-graph.mjs';
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
    assert.equal(out.status,0,out.stderr);assert.equal(JSON.parse(readFileSync(result,'utf8')).value,telemetry.measurements[metric==='jobs-per-run'?'jobsPerRun':'runsPerChange']);
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

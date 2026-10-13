import assert from 'node:assert/strict';
import { test, after } from 'node:test';
import { rmSync, writeFileSync, symlinkSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { history } from './history.mjs';
import { root, evaluate, expandMatrix, resolveRun, loadWorkflows, triggered, followDownstream } from '../workflow-graph.mjs';
const h = history();
symlinkSync(join(root, '.github'), join(h.dir, '.github'), process.platform === 'win32' ? 'junction' : 'dir');
after(() => rmSync(h.dir,{recursive:true,force:true}));
test('real classifier admits only a same-repository automation branch with parsed release-only content', () => {
  const release = h.event(false,'release-plz-2026');
  assert.deepEqual(h.classify(release),{isReleaseOnly:true,macosRequired:false,reason:'same-repository release-plz branch with semantic version/changelog-only diff'});
  for(const login of ['nickderobertis','github-actions[bot]','someone']) {
    release.pull_request.user.login=login;
    assert.equal(h.classify(release).isReleaseOnly,true,'author is not authority (release-plz uses a personal token)');
  }
  const botOnly=h.event();botOnly.pull_request.user.login='github-actions[bot]';
  assert.equal(h.classify(botOnly).macosRequired,true,'bot identity alone authorizes no omission');
  for(const payload of [h.event(),h.event(true,'release-plz-2026'),{},'{bad',null,[]]) assert.equal(h.classify(payload).macosRequired,true);
  for(const key of ['source','dependency','lockDependency','sameNameDependency','mixed','malformed','invalidVersion','changelog','added','deleted']) assert.equal(h.classify(release,h.heads[key]).macosRequired,true,key);
  assert.equal(h.classify(release,h.heads.version,'pull_request','missing-ref').macosRequired,true);
  assert.equal(h.classify(release,h.base).macosRequired,true,'empty diff');
  for(const event of ['push','release','schedule','workflow_run','repository_dispatch','workflow_dispatch']) assert.equal(h.classify('{bad',h.heads.version,event).macosRequired,false,event);
});
test('real Linux carrier fails every unapproved job outcome and reports intentional omission honestly', () => {
  const carrier = loadWorkflows()['ci.yml'].jobs['macos-result'];
  const command = carrier.steps.find(step => step.env?.NEEDS_JSON).run;
  for(const required of ['true','false']) for(const result of ['success','failure','cancelled','skipped',undefined]) for(const policyResult of ['success','failure','cancelled','skipped',undefined]) {
    const needs={policy:{result:policyResult,outputs:{macosRequired:required}},macos:{result}};
    const path=join(h.dir,'needs.json');writeFileSync(path,JSON.stringify(needs));
    const out=spawnSync('bash',['-eo','pipefail','-c',command],{cwd:h.dir,encoding:'utf8',env:{...process.env,NEEDS_JSON:JSON.stringify(needs)}});
    const good=policyResult==='success' && result===(required==='true'?'success':'skipped');
    assert.equal(out.status===0,good,JSON.stringify(needs));
    if(good && required==='false') assert.match(out.stdout,/no Mac tests ran/);
  }
  const path=join(h.dir,'needs.json');writeFileSync(path,JSON.stringify({policy:{result:'success',outputs:{}},macos:{result:'skipped'}}));
  assert.notEqual(spawnSync(process.execPath,[join(root,'.github/macos-result.mjs'),path]).status,0);
});
test('published Actions evaluator expands matrices and resolves reusable calls; unknowns fail explicitly', () => {
  assert.equal(evaluate("always() && needs.build.result == 'failure'",{needs:{build:{result:'failure'}}}),true);
  assert.deepEqual(expandMatrix({include:[{os:'macos-15',target:'a'},{os:'macos-15',target:'b'}]},{}),[{os:'macos-15',target:'a'},{os:'macos-15',target:'b'}]);
  assert.deepEqual(expandMatrix({os:['ubuntu-latest','macos-latest'],include:[{os:'macos-latest',target:'darwin'}],exclude:[{os:'ubuntu-latest'}]},{}),[{os:'macos-latest',target:'darwin'}]);
  const scenario={eventName:'workflow_dispatch',event:{},ref:'refs/heads/main',head:h.heads.source,classification:{macosRequired:false}};
  const call={jobs:{call:{uses:'./.github/workflows/nested.yml'}}};
  assert.equal(resolveRun({'caller.yml':call,'nested.yml':{jobs:{mac:{'runs-on':'macos-15'}}}},'caller.yml',scenario)[0].macos,true);
  assert.throws(()=>resolveRun({'caller.yml':call},'caller.yml',scenario),/unresolved workflow call/);
  assert.throws(()=>resolveRun({'caller.yml':{jobs:{call:{uses:'owner/repo/.github/workflows/x.yml@main'}}}},'caller.yml',scenario),/unresolved external/);
  assert.throws(()=>resolveRun({'caller.yml':{jobs:{mac:{'runs-on':'${{ matrix.unknown }}'}}}},'caller.yml',scenario),/unresolved runner/);
  assert.ok(loadWorkflows()['ci.yml']);
});

test('actual event filters honor tags, branch/path exclusions and reject unsupported fields', () => {
  const scenario={eventName:'push',event:{},ref:'refs/tags/v0.1.1',files:['src/lib.rs']};
  assert.equal(triggered({on:{push:{tags:['v*']}}},scenario),true);
  assert.equal(triggered({on:{push:{'tags-ignore':['v*']}}},scenario),false);
  assert.equal(triggered({on:{push:{branches:['main']}}},scenario),false);
  const push={...scenario,ref:'refs/heads/main'};
  assert.equal(triggered({on:{push:{'branches-ignore':['main']}}},push),false);
  assert.equal(triggered({on:{push:{paths:['docs/**']}}},push),false);
  assert.equal(triggered({on:{push:{'paths-ignore':['src/**']}}},push),false);
  assert.equal(triggered({on:{push:{branches:['main'],paths:['src/**']}}},push),true);
  assert.throws(()=>triggered({on:{push:{unknown:['main']}}},push),/unsupported trigger/);
});
test('actual downstream triggers preserve repeated runs and exact heads; cycles fail explicitly', () => {
  const workflows={
    'parent.yml':{name:'Build',jobs:{build:{'runs-on':'ubuntu-latest'}}},
    'smoke.yml':{name:'Smoke',on:{workflow_run:{workflows:['Build'],types:['completed']}},jobs:{smoke:{'runs-on':'ubuntu-latest',if:"github.event.workflow_run.head_sha == 'abc'"}}},
  };
  const parents=[{workflow:'parent.yml',head:'abc'},{workflow:'parent.yml',head:'abc'},{workflow:'parent.yml',head:'def'}];
  const rows=followDownstream(workflows,parents,{ref:'refs/heads/main',files:[]});
  assert.deepEqual(rows.map(r=>r.head),['abc','abc','def']);
  assert.deepEqual(rows.map(r=>r.jobs[0].state),['requested','requested','skipped']);
  workflows['parent.yml'].on={workflow_run:{workflows:['Smoke'],types:['completed']}};
  assert.throws(()=>followDownstream(workflows,parents,{ref:'refs/heads/main',files:[]}),/cyclic downstream/);
});

// Resolve the actual input choices, including single-harness diagnostics and
// per-harness Linux/Windows dispatches, rather than only the default workload.
test('every declared manual choice preserves its supported runner and harness routing', () => {
  const workflows = loadWorkflows();
  for (const [file, workflow] of Object.entries(workflows)) {
    const dispatch = workflow.on?.workflow_dispatch;
    if (!dispatch?.inputs?.os) continue;
    for (const os of dispatch.inputs.os.options) {
      const scenario = { eventName: 'workflow_dispatch', event: {}, ref: 'refs/heads/main', head: h.heads.source, inputs: { os } };
      const jobs = resolveRun(workflows, file, scenario);
      assert.ok(jobs.length, `${file}: ${os} resolves jobs`);
      assert.ok(jobs.every(job => !job.macos), `${file}: ${os} reserves Mac diagnostics for CI`);
      if (!['all', 'default'].includes(os)) assert.deepEqual(jobs.map(job => job.runner), [os], `${file}: single platform`);
    }
  }
  const mac = workflows['ci.yml'].jobs.macos;
  for (const harness of workflows['ci.yml'].on.workflow_dispatch.inputs.harness.options) {
    const context = { github: { repository: 'nickderobertis/allowlister', event_name: 'workflow_dispatch', event: {} }, inputs: { harness } };
    const commands = mac.steps.filter(step => step.run?.match(/just test-(codex|claude|copilot|crush|cursor|goose|opencode|qwen)\b/) && evaluate(step.if, context));
    const names = commands.map(step => step.run.match(/just test-(\w+)/)[1]);
    assert.deepEqual(names.sort(), (harness === 'all' ? ['codex','claude','copilot','crush','cursor','goose','opencode','qwen'] : [harness]).sort());
  }
});

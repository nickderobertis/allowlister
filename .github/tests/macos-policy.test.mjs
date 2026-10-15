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
  assert.equal(h.classify(release,h.heads.prerelease).isReleaseOnly,true,'valid prerelease and build identifiers remain version-only');
  for (const name of [1,'not-a-repository',' owner/repo','owner/repo/extra']) {
    const malformed=h.event(false,'release-plz-test');
    malformed.repository.full_name=name;malformed.pull_request.head.repo.full_name=name;malformed.pull_request.base.repo.full_name=name;
    assert.equal(h.classify(malformed).macosRequired,true,'malformed repository identity');
  }
  const other=h.event(false,'release-plz-test');
  other.repository.full_name='someone/another';other.pull_request.head.repo.full_name='someone/another';other.pull_request.base.repo.full_name='someone/another';
  assert.equal(h.classify(other).macosRequired,true,'payload repository must match Actions repository identity');
  const botOnly=h.event();botOnly.pull_request.user.login='github-actions[bot]';
  assert.equal(h.classify(botOnly).macosRequired,true,'bot identity alone authorizes no omission');
  for(const payload of [h.event(),h.event(true,'release-plz-2026'),{},'{bad',null,[]]) assert.equal(h.classify(payload).macosRequired,true);
  for(const key of ['source','dependency','lockDependency','sameNameDependency','mixed','malformed','invalidVersion','arrayManifestVersion','arrayLockVersion','invalidLockVersion','missingLocalPackage','duplicateLocalPackage','wrongPackageName','invalidSemver0','invalidSemver1','invalidSemver2','changelog','added','deleted']) assert.equal(h.classify(release,h.heads[key]).macosRequired,true,key);
  assert.equal(h.classify(release,h.heads.version,'pull_request','missing-ref').macosRequired,true);
  assert.equal(h.classify(release,h.base).macosRequired,true,'empty diff');
  for (const ownership of ['missing','duplicate','renamed']) {
    const classified=h.classifyOwnership(ownership);
    assert.equal(classified.macosRequired,true,ownership);assert.equal(classified.isReleaseOnly,false,ownership);
  }
  for (const [base,head] of [['',h.heads.version],[h.base,''],['-invalid',h.heads.version],[h.base,'-invalid']]) {
    const classified=h.classify(release,head,'pull_request',base);
    assert.equal(classified.macosRequired,true);assert.match(classified.reason,/missing or invalid revisions/);
  }
  for(const event of ['push','release','schedule','workflow_run','repository_dispatch','workflow_dispatch']) assert.equal(h.classify('{bad',h.heads.version,event).macosRequired,false,event);
  for(const event of ['unknown','pull_request_typo','','constructor']) {
    const classified=h.classify(release,h.heads.version,event);
    assert.equal(classified.macosRequired,true,event);assert.equal(classified.isReleaseOnly,false,event);assert.match(classified.reason,/unknown event name/);assert.ok(classified.reason.includes(JSON.stringify(event)),'reason names the unrecognised event');
  }
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
    if(good && required==='true') assert.match(out.stdout,/Mac lane job succeeded/);
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

// Authoritative Actions examples and event rules:
// https://docs.github.com/en/actions/using-jobs/using-a-matrix-for-your-jobs#expanding-or-adding-matrix-configurations
// https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#onpushpull_requestpull_request_targetpathspaths-ignore
// https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request
test('graph resolution reconciles matrix includes and event filters with the Actions contract', () => {
  const matrix = { fruit: ['apple', 'pear'], animal: ['cat', 'dog'], include: [
    { color: 'green' }, { color: 'pink', animal: 'cat' },
    { fruit: 'apple', shape: 'circle' }, { fruit: 'banana' }, { fruit: 'banana', animal: 'cat' },
  ] };
  assert.deepEqual(expandMatrix(matrix, {}), [
    { fruit: 'apple', animal: 'cat', color: 'pink', shape: 'circle' },
    { fruit: 'apple', animal: 'dog', color: 'green', shape: 'circle' },
    { fruit: 'pear', animal: 'cat', color: 'pink' }, { fruit: 'pear', animal: 'dog', color: 'green' },
    { fruit: 'banana' }, { fruit: 'banana', animal: 'cat' },
  ]);
  assert.deepEqual(expandMatrix({ include: [{ os: 'macos-15', target: 'a' }, { os: 'macos-15', target: 'b' }] }, {}), [{ os: 'macos-15', target: 'a' }, { os: 'macos-15', target: 'b' }]);
  assert.deepEqual(expandMatrix({ os: ['ubuntu-latest'], include: [{ os: 'macos-15', target: 'a' }, { os: 'macos-15', target: 'b' }] }, {}), [{ os: 'ubuntu-latest' }, { os: 'macos-15', target: 'a' }, { os: 'macos-15', target: 'b' }], 'new includes are separate jobs, never candidates for later merges');
  const event = h.event(); event.action = 'edited';
  const pr = { eventName: 'pull_request', event, ref: 'refs/pull/1/merge', files: ['src/lib.rs'] };
  for (const on of ['pull_request', ['pull_request'], { pull_request: null }]) assert.equal(triggered({ on }, pr), false);
  assert.equal(triggered({ on: { pull_request: { types: ['edited'] } } }, pr), true);
  for (const action of ['opened', 'synchronize', 'reopened']) assert.equal(triggered({ on: 'pull_request' }, { ...pr, event: { ...event, action } }), true);
  const tag = { ...pr, eventName: 'push', ref: 'refs/tags/v0.1.1' };
  assert.equal(triggered({ on: { push: { tags: ['v*'], paths: ['docs/**'] } } }, tag), true);
  assert.equal(triggered({ on: { push: { tags: ['v*'], 'paths-ignore': ['src/**'] } } }, tag), true);
  for (const pattern of ['!src/**', 'src/?.rs', 'v[0-9]+', '{main,master}']) {
    assert.throws(() => triggered({ on: { push: { tags: [pattern] } } }, tag), /unsupported Actions filter pattern/);
  }
});


test('real workflow graph refuses malformed matrix structures', () => {
  const scenario={eventName:'push',event:{},ref:'refs/heads/main',head:'test'};
  for(const matrix of [[],null,'bad',{include:'bad'},{exclude:'bad'},{include:[null]},{include:['bad']},{exclude:[null]}]) {
    assert.throws(()=>resolveRun({'fixture.yml':{jobs:{test:{'runs-on':'ubuntu-latest',strategy:{matrix}}}}},'fixture.yml',scenario),/invalid matrix/);
  }
});


test('actual graph refuses malformed job mappings and event filter declarations', () => {
  const scenario={eventName:'push',event:{},ref:'refs/heads/main',head:'test'};
  for(const jobs of [[],{},null,'bad',{test:null}]) assert.throws(()=>resolveRun({'fixture.yml':{jobs}},'fixture.yml',scenario),/invalid jobs/);
  for(const config of [false,'bad',{types:'published'},{workflows:'CI'},{branches:'main'},{paths:[false]}]) assert.throws(()=>triggered({on:{push:config}},scenario),/invalid trigger/);
});

// Exit 0: a recorded measurement was reported; 1: invalid/missing/stale telemetry.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { report } from '@onebudgetspec/sdk';
import { root, sourceDigest, loadWorkflows, expandMatrix, resolve as resolveDeclaration } from './workflow-graph.mjs';
const {values}=parseArgs({options:{metric:{type:'string'},telemetry:{type:'string'}}});
const path=resolve(process.cwd(),values.telemetry ?? '.github/.telemetry/macos-policy.json');
const telemetry=JSON.parse(readFileSync(path,'utf8'));
if(telemetry.schemaVersion!==1 || telemetry.sourceDigest!==sourceDigest()) throw new Error('missing/stale Mac telemetry; run github-workflows:test');
const key=new Map([['jobs-per-run','jobsPerRun'],['runs-per-change','runsPerChange']]).get(values.metric);
if(!key) throw new Error('invalid metric');
function validateRuns(runs) {
  if(!Array.isArray(runs)) throw new Error('invalid telemetry runs');
  for(const run of runs) {
    if(typeof run.workflow !== 'string' || !Array.isArray(run.jobs)) throw new Error('invalid telemetry workflow');
    for(const job of run.jobs) {
      if(typeof job.job !== 'string' || typeof job.macos !== 'boolean' || !['skipped','requested'].includes(job.state)) throw new Error('invalid telemetry job');
      if(job.state === 'skipped' && job.macos) throw new Error('skipped telemetry job cannot spend Mac');
      if(job.state === 'requested') {
        const labels=[job.runner].flat();
        if(!labels.length || labels.some(l=>typeof l !== 'string' || !/^(macos|ubuntu|windows)-[\w.-]+$/.test(l)) || job.macos !== labels.some(l=>l.startsWith('macos-'))) throw new Error('inconsistent telemetry runner');
      }
    }
  }
}
// llmlint: ignore[boundary_inputs_validated] Complete event/ownership/lifecycle composition is proved by graph journeys that record this telemetry; the telemetry-only reporter validates recorded coverage without rerunning scenarios or routing.
if(!Array.isArray(telemetry.scenarios) || !telemetry.scenarios.length || !Array.isArray(telemetry.lifecycles) || !telemetry.lifecycles.length) throw new Error('incomplete telemetry collections');
for(const scenario of telemetry.scenarios) {
  if(typeof scenario.id !== 'string' || typeof scenario.eventName !== 'string') throw new Error('invalid telemetry event');
  validateRuns(scenario.runs);
}
for(const lifecycle of telemetry.lifecycles) {
  validateRuns(lifecycle.runs);
  if(lifecycle.macosRuns !== lifecycle.runs.filter(r=>r.jobs.some(j=>j.macos)).length) throw new Error('inconsistent telemetry lifecycle');
}
// Source declarations are read only to reject gaps in recorded coverage. Costs
// below come exclusively from telemetry; no workflow routing or journey is run.
const workflows=loadWorkflows();
const covered=new Set();
function validateCoverage(runs, scenario) {
  for(const run of runs) {
    const workflow=workflows[run.workflow];
    if(!workflow) throw new Error(`unknown telemetry workflow ${run.workflow}`);
    covered.add(run.workflow);
    for(const [id,job] of Object.entries(workflow.jobs)) {
      const rows=run.jobs.filter(row=>row.job===id && (row.workflow===undefined || row.workflow===run.workflow));
      if(!rows.length) throw new Error(`missing telemetry coverage ${run.workflow}:${id}`);
      // llmlint: ignore[boundary_inputs_validated] Routing authorization is proved by graph journeys; under the telemetry-only agreement this reporter validates recorded coverage and does not rerun or re-evaluate conditional routing.
      if(rows.every(row=>row.state==='skipped')) {
        const deps=[job.needs ?? []].flat();
        const skippedDependency=deps.some(dep=>run.jobs.filter(row=>row.job===dep).every(row=>row.state==='skipped'));
        if((!job.if && !skippedDependency) || (run.workflow==='ci.yml' && id==='macos' && scenario?.policyOutputs?.macosRequired==='true')) {
          throw new Error(`unauthorized skipped telemetry job ${run.workflow}:${id}`);
        }
        continue;
      }
      if(job.uses) throw new Error(`unresolved telemetry coverage for reusable call ${run.workflow}:${id}`);
      if(!scenario && JSON.stringify(job.strategy?.matrix ?? {}).includes('${{')) continue;
      const context={github:{event_name:scenario?.eventName},inputs:scenario?.inputs ?? {},needs:{},vars:{}};
      for(const cell of expandMatrix(job.strategy?.matrix,context)) {
        const row=rows.find(row=>JSON.stringify(Object.entries(row.matrix ?? {}).sort())===JSON.stringify(Object.entries(cell).sort()));
        if(!row) {
          throw new Error(`missing telemetry coverage ${run.workflow}:${id} matrix ${JSON.stringify(cell)}`);
        }
        const runner=resolveDeclaration(job['runs-on'],{...context,matrix:cell});
        if(JSON.stringify([runner].flat())!==JSON.stringify([row.runner].flat())) throw new Error(`inconsistent declared telemetry runner ${run.workflow}:${id}`);
      }
    }
  }
}
for(const scenario of telemetry.scenarios) validateCoverage(scenario.runs,scenario);
for(const lifecycle of telemetry.lifecycles) validateCoverage(lifecycle.runs);
for(const file of Object.keys(workflows)) if(!covered.has(file)) throw new Error(`missing telemetry coverage workflow ${file}`);
const recorded={jobsPerRun:Math.max(...telemetry.scenarios.flatMap(s=>s.runs.map(r=>r.jobs.filter(j=>j.macos).length))),runsPerChange:Math.max(...telemetry.lifecycles.map(l=>l.macosRuns))};
for(const metric of Object.keys(recorded)) if(!Number.isInteger(telemetry.measurements?.[metric]) || telemetry.measurements[metric]<0 || telemetry.measurements[metric]!==recorded[metric]) throw new Error('inconsistent telemetry measurement');
const detail=values.metric==='jobs-per-run' ? telemetry.scenarios.map(s=>({event:s.id,runs:s.runs.map(r=>({workflow:r.workflow,jobs:r.jobs.filter(j=>j.macos)}))})) : telemetry.lifecycles.map(l=>({id:l.id,runs:l.runs.filter(r=>r.jobs.some(j=>j.macos))}));
const actual=telemetry.measurements[key];
if(!report(actual,JSON.stringify({sourceDigest:telemetry.sourceDigest,breakdown:detail}))) console.log(JSON.stringify({actual,detail}));

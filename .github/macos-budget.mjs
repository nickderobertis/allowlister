// Exit 0: a recorded measurement was reported; 1: invalid/missing/stale telemetry.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { report } from '@onebudgetspec/sdk';
import { root, sourceDigest } from './workflow-graph.mjs';
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
if(!Array.isArray(telemetry.scenarios) || !telemetry.scenarios.length || !Array.isArray(telemetry.lifecycles) || !telemetry.lifecycles.length) throw new Error('incomplete telemetry collections');
for(const scenario of telemetry.scenarios) {
  if(typeof scenario.id !== 'string' || typeof scenario.eventName !== 'string') throw new Error('invalid telemetry event');
  validateRuns(scenario.runs);
}
for(const lifecycle of telemetry.lifecycles) {
  validateRuns(lifecycle.runs);
  if(lifecycle.macosRuns !== lifecycle.runs.filter(r=>r.jobs.some(j=>j.macos)).length) throw new Error('inconsistent telemetry lifecycle');
}
const recorded={jobsPerRun:Math.max(...telemetry.scenarios.flatMap(s=>s.runs.map(r=>r.jobs.filter(j=>j.macos).length))),runsPerChange:Math.max(...telemetry.lifecycles.map(l=>l.macosRuns))};
for(const metric of Object.keys(recorded)) if(!Number.isInteger(telemetry.measurements?.[metric]) || telemetry.measurements[metric]<0 || telemetry.measurements[metric]!==recorded[metric]) throw new Error('inconsistent telemetry measurement');
const detail=values.metric==='jobs-per-run' ? telemetry.scenarios.map(s=>({event:s.id,runs:s.runs.map(r=>({workflow:r.workflow,jobs:r.jobs.filter(j=>j.macos)}))})) : telemetry.lifecycles.map(l=>({id:l.id,runs:l.runs.filter(r=>r.jobs.some(j=>j.macos))}));
const actual=telemetry.measurements[key];
if(!report(actual,JSON.stringify({sourceDigest:telemetry.sourceDigest,breakdown:detail}))) console.log(JSON.stringify({actual,detail}));

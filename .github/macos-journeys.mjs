import { mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { runPolicy } from './run-policy.mjs';
import { history } from './tests/history.mjs';
import { loadWorkflows, resolveRun, triggered, root, sourceDigest, followDownstream } from './workflow-graph.mjs';
export function journeys() {
  const h=history();
  try {
    const workflows=loadWorkflows();
    const scenarios=[];
    const classes=[['pr-opened','pull_request','opened'],['pr-synchronize','pull_request','synchronize'],['pr-reopened','pull_request','reopened'],['base-push','push'],['release-merge','push'],['tag-push','push'],['published','release','published'],['downstream','workflow_run','completed'],['dispatch','repository_dispatch'],['schedule','schedule'],['manual-default','workflow_dispatch'],['manual-opt-in','workflow_dispatch']];
    const proofs=new Map();
    const outputs=new Map();
    for(const diff of ['source','version','dependency']) for(const fork of [false,true]) {
      const payload=h.event(fork,diff==='version'?'release-plz-2026':'feature');
      const classification=h.classify(payload,h.heads[diff]);
      proofs.set(`${diff}:${fork}`,classification);
      for(const [eventClass,eventName,action] of classes) for(const publishCrate of [false,true]) {
        const event={...payload,action,release:{tag_name:'v0.1.1'},workflow_run:{name:'Release',head_sha:h.heads[diff],conclusion:'success'}};
        const scenario={id:`${eventClass}:${diff}:${fork?'fork':'same'}:${publishCrate?'crate':'archives'}`,eventClass,eventName,event,ref:eventClass==='tag-push'?'refs/tags/v0.1.1':'refs/heads/main',head:h.heads[diff],files:diff==='source'?['src/lib.rs']:diff==='version'?['Cargo.toml','Cargo.lock','CHANGELOG.md']:['Cargo.toml'],classification:eventName==='pull_request'?classification:{macosRequired:false,isReleaseOnly:false},inputs:{os:'default',macos_diagnostic:eventClass==='manual-opt-in',harness:'all'},publishCrate};
        event.pull_request.head.sha=h.heads[diff];
        const key=JSON.stringify([eventName,diff,fork,scenario.inputs.macos_diagnostic]);
        if(!outputs.has(key)) outputs.set(key,runPolicy(workflows['ci.yml'],scenario,h));
        scenario.policyOutputs=outputs.get(key);
        const runs=Object.entries(workflows).filter(([,wf])=>triggered(wf,scenario)).map(([file])=>({workflow:file,jobs:resolveRun(workflows,file,scenario)}));
        scenarios.push({...scenario,event:undefined,runs});
      }
    }
    // A lifecycle contains one ordinary push/PR update, its merge, the release
    // automation's version-only PR and merge, tag/publication, and every actual
    // workflow_run downstream trigger. Open/reopen are alternate event journeys.
    const lifecycles=[];
    for(const publishCrate of [false,true]) for(const fork of [false,true]) {
      const pick=(eventClass,diff)=>scenarios.find(s=>s.eventClass===eventClass && s.id.includes(`:${diff}:${fork?'fork':'same'}:`) && s.publishCrate===publishCrate);
      const selected=[pick('pr-synchronize','source'),pick('base-push','source'),pick('pr-opened','version'),pick('release-merge','version'),pick('tag-push','version'),pick('published','version')];
      // The release PR is always created in the canonical repository, even
      // when the original contribution arrived from a fork.
      selected[2]=scenarios.find(s=>s.eventClass==='pr-opened' && s.id.includes(':version:same:') && s.publishCrate===publishCrate);
      const runs=selected.flatMap(s=>s.runs.map(run=>({...run,event:s.id,head:s.head})));
      runs.push(...followDownstream(workflows,runs,{ref:'refs/heads/main',files:[],publishCrate}));
      lifecycles.push({id:`${fork?'fork':'same'}:${publishCrate?'crate':'archives'}`,runs,macosRuns:runs.filter(r=>r.jobs.some(j=>j.macos)).length});
    }
    return {schemaVersion:1,sourceDigest:sourceDigest(),scenarios,lifecycles,measurements:{jobsPerRun:Math.max(0,...scenarios.flatMap(s=>s.runs.map(r=>r.jobs.filter(j=>j.macos).length))),runsPerChange:Math.max(0,...lifecycles.map(l=>l.macosRuns))}};
  } finally {rmSync(h.dir,{recursive:true,force:true});}
}
if(import.meta.main) {
  const telemetry=journeys();mkdirSync(join(root,'.github/.telemetry'),{recursive:true});writeFileSync(join(root,'.github/.telemetry/macos-policy.json'),JSON.stringify(telemetry,null,2)+'\n');
  console.log(JSON.stringify(telemetry.measurements));
}

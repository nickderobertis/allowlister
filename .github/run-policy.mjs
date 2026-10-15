// Execute the workflow's actual policy command in an isolated generated git
// history. Only Actions event/output injection is doubled, not the classifier.
import { spawnSync } from 'node:child_process';
import { existsSync, symlinkSync, writeFileSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { root, resolve } from './workflow-graph.mjs';
export function runPolicy(workflow, scenario, history) {
  if(!existsSync(join(history.dir,'.github'))) symlinkSync(join(root,'.github'),join(history.dir,'.github'),process.platform==='win32'?'junction':'dir');
  const event=join(history.dir,'route-event.json'),output=join(history.dir,'route-output');
  writeFileSync(event,scenario.rawEvent ?? JSON.stringify(scenario.event));writeFileSync(output,'');
  const step=workflow.jobs.policy.steps.find(s=>s.id==='policy');
  const context={github:{event_name:scenario.eventName,event:scenario.event},inputs:scenario.inputs};
  const env=resolve(step.env,context);
  const out=spawnSync('bash',['-eo','pipefail','-c',step.run],{cwd:history.dir,encoding:'utf8',env:{...process.env,...env,GITHUB_EVENT_NAME:scenario.eventName,GITHUB_REPOSITORY:'nickderobertis/allowlister',GITHUB_EVENT_PATH:event,GITHUB_OUTPUT:output}});
  if(out.status!==0) throw new Error(`actual workflow policy step failed: ${out.stderr}`);
  const outputs=Object.fromEntries(readFileSync(output,'utf8').trim().split('\n').map(line=>line.split('=')));
  if(!['true','false'].includes(outputs.macosRequired)) throw new Error('workflow produced no boolean Mac policy output');
  return outputs;
}

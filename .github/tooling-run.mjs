// Private locked install, quiet when successful. Nx invokes this on clean clones
// so workflow tests never depend on a developer's globally installed packages.
// Exit 0: selected checks pass; nonzero: install/check failure or invalid command.
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, existsSync, readdirSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
const directory=fileURLToPath(new URL('.',import.meta.url));
const root=fileURLToPath(new URL('../',import.meta.url));
const digest=createHash('sha256').update(readFileSync(directory+'package-lock.json')).digest('hex');
const stamp=directory+'node_modules/.locked-install';
if(!existsSync(stamp) || readFileSync(stamp,'utf8')!==digest || !existsSync(directory+'node_modules/@actions/expressions/dist/index.js')) {
  const out=spawnSync('npm',['ci','--prefix',directory,'--ignore-scripts','--no-audit','--no-fund'],{encoding:'utf8',shell:process.platform==='win32'});
  if(out.status!==0) {console.error(out.stdout,out.stderr);process.exit(out.status ?? 1);}
  writeFileSync(stamp,digest);
}
const command=process.argv[2];
let status=0;
function run(args) {const out=spawnSync(process.execPath,args,{cwd:root,stdio:'inherit'});status ||= out.status ?? 1;}
if(command==='test') run(['--test',...readdirSync(directory+'tests').filter(f=>f.endsWith('.test.mjs')).map(f=>'.github/tests/'+f)]);
else if(command==='lint') {
  for(const dir of ['', 'scripts/', 'tests/']) for(const file of readdirSync(directory+dir).filter(f=>f.endsWith('.mjs'))) run(['--check','.github/'+dir+file]);
  const out=spawnSync('bash',['scripts/check-e2e-matrix.sh'],{cwd:root,stdio:'inherit'});status ||= out.status ?? 1;
} else if(command==='budgets') {
  const {check}=await import('@onebudgetspec/sdk');
  const result=await check({paths:['.github/budgets.yaml'],cwd:root});
  for(const row of result.results) {console.log(`${row.id}: ${row.verdict} (${row.actual} ${row.unit})`);if(row.verdict!=='within') {console.error(row.error ?? row.detail);status=1;}}
} else throw new Error('usage: tooling-run.mjs test|lint|budgets');
process.exit(status);

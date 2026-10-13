import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { root } from '../workflow-graph.mjs';
export function history() {
  const dir = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), 'macos-history-'));
  const git = (...args) => execFileSync('git', args, { cwd: dir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  git('init', '-q'); git('config','user.email','test@example.invalid'); git('config','user.name','Contract');
  const manifest = '[package]\nname = "allowlister"\nversion = "0.1.0"\nedition = "2021"\n[dependencies]\nserde = "1"\n';
  const lock = 'version = 4\n[[package]]\nname = "allowlister"\nversion = "0.1.0"\n[[package]]\nname = "serde"\nversion = "1.0.0"\nchecksum = "abc"\n[[package]]\nname = "allowlister"\nversion = "2.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "def"\n';
  const source = 'fn main() {}\n';
  for (const [file, text] of Object.entries({'Cargo.toml':manifest, 'Cargo.lock':lock, 'CHANGELOG.md':'# Changelog\n\n## 0.1.0\nFirst release.\n', 'main.rs':source})) writeFileSync(join(dir,file), text);
  git('add','.'); git('commit','-qm','initial'); const base = git('rev-parse','HEAD');
  const heads = {};
  const make = (key, changes) => {
    git('checkout','-q',base);
    for (const [file,text] of Object.entries(changes)) writeFileSync(join(dir,file),text);
    git('add','.'); git('commit','-qm',key); heads[key] = git('rev-parse','HEAD');
  };
  make('version', {'Cargo.toml':manifest.replace('0.1.0','0.1.1'),'Cargo.lock':lock.replace('0.1.0','0.1.1'),'CHANGELOG.md':'# Changelog\n\n## 0.1.1\nFixed commands.\n\n## 0.1.0\nFirst release.\n'});
  make('source',{'main.rs':'fn main() { println!("hello"); }\n'});
  make('dependency',{'Cargo.toml':manifest.replace('0.1.0','0.1.1').replace('serde = "1"','serde = "2"')});
  make('lockDependency',{'Cargo.lock':lock.replace('1.0.0','2.0.0')});
  make('sameNameDependency', {'Cargo.lock':lock.replace('2.0.0','2.1.0')});
  make('mixed',{'Cargo.toml':manifest.replace('0.1.0','0.1.1'),'main.rs':'fn main() { println!("hello"); }\n'});
  make('malformed', {'Cargo.toml':'[package\n'});
  make('invalidVersion', {'Cargo.toml':manifest.replace('0.1.0','not-a-version')});
  make('changelog', {'CHANGELOG.md':'unparsed content\n'});
  make('added', {'extra.md':'release notes\n'});
  git('checkout','-q',base);git('rm','-q','CHANGELOG.md');git('commit','-qm','deleted');heads.deleted=git('rev-parse','HEAD');
  const event = (fork = false, branch = 'feature') => ({ repository:{full_name:'nickderobertis/allowlister'}, pull_request:{title:'chore: release', user:{login:'nickderobertis'}, head:{sha:heads.version,ref:branch,repo:{full_name:fork?'someone/fork':'nickderobertis/allowlister'}}, base:{sha:base,ref:'main',repo:{full_name:'nickderobertis/allowlister'}}} });
  const classify = (payload, head = heads.version, eventName = 'pull_request', ref = base) => {
    const path = join(dir,'event.json'); writeFileSync(path, typeof payload === 'string' ? payload : JSON.stringify(payload));
    const out = spawnSync(process.execPath,[join(root,'.github/macos-policy.mjs'),'classify','--event',path,'--base',ref,'--head',head],{cwd:dir,encoding:'utf8',env:{...process.env,GITHUB_EVENT_NAME:eventName}});
    if(out.status !== 0) throw new Error(out.stderr);
    return JSON.parse(out.stdout);
  };
  return {dir,base,heads,event,classify};
}

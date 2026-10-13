// Repository-local Actions graph resolver. Expressions use GitHub's published
// evaluator; unsupported calls/runners are errors, never an empty Mac count.
import { readFileSync, readdirSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'yaml';
import { Lexer, Parser, Evaluator, data } from '@actions/expressions';
export const root = fileURLToPath(new URL('../', import.meta.url));
export function loadWorkflows(directory = join(root, '.github/workflows')) {
  return Object.fromEntries(readdirSync(directory).filter(f => /\.ya?ml$/.test(f)).sort().map(f => [f, parse(readFileSync(join(directory, f), 'utf8'))]));
}
function boxed(value) {
  if (value === null || value === undefined) return new data.Null();
  if (Array.isArray(value)) return new data.Array(...value.map(boxed));
  if (typeof value === 'object') return new data.Dictionary(...Object.entries(value).map(([key, v]) => ({ key, value: boxed(v) })));
  if (typeof value === 'boolean') return new data.BooleanData(value);
  if (typeof value === 'number') return new data.NumberData(value);
  return new data.StringData(value);
}
function plain(value) {
  if (value instanceof data.Dictionary) return Object.fromEntries(value.pairs().map(({key, value}) => [key, plain(value)]));
  if (value instanceof data.Array) return value.values().map(plain);
  if (value instanceof data.Null) return null;
  return value.value;
}
export function evaluate(expression, context) {
  const statuses = Object.values(context.needs ?? {}).map(n => n.result);
  const functions = new Map(['always', 'success', 'failure', 'cancelled'].map(name => [name, {
    name, minArgs: 0, maxArgs: 0,
    call: () => boxed(name === 'always' || (name === 'success' && statuses.every(s => s === 'success')) || (name === 'failure' && statuses.includes('failure')) || (name === 'cancelled' && statuses.includes('cancelled'))),
  }]));
  const source = expression.trim().replace(/^\$\{\{\s*|\s*\}\}$/g, '');
  const ast = new Parser(new Lexer(source).lex().tokens, Object.keys(context), [...functions.values()]).parse();
  return plain(new Evaluator(ast, boxed(context), functions).evaluate());
}
export function resolve(value, context) {
  if (typeof value === 'string' && /^\s*\$\{\{[\s\S]*\}\}\s*$/.test(value)) return evaluate(value, context);
  if (typeof value === 'string') return value.replace(/\$\{\{([\s\S]*?)\}\}/g, (_, e) => {
    const result = evaluate(e, context);
    if (result === null || result === undefined || typeof result === 'object') throw new Error(`unresolved interpolation ${e}`);
    return String(result);
  });
  if (Array.isArray(value)) return value.map(v => resolve(v, context));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k,v]) => [k, resolve(v, context)]));
  return value;
}
export function expandMatrix(matrix = {}, context) {
  matrix = resolve(matrix, context);
  const { include = [], exclude = [], ...axes } = matrix;
  let cells = [{}];
  for (const [key, values] of Object.entries(axes)) {
    if (!Array.isArray(values) || !values.length) throw new Error(`unresolved/empty matrix axis ${key}`);
    cells = cells.flatMap(cell => values.map(value => ({ ...cell, [key]: value })));
  }
  cells = cells.filter(cell => !exclude.some(ex => Object.entries(ex).every(([k,v]) => cell[k] === v)));
  if (!Object.keys(axes).length && include.length) return include.filter(cell => !exclude.some(ex => Object.entries(ex).every(([k,v]) => cell[k] === v)));
  for (const extra of include) {
    const compatible = cells.filter(cell => Object.keys(axes).every(k => !(k in extra) || cell[k] === extra[k]));
    if (compatible.length) compatible.forEach(cell => Object.assign(cell, extra));
    else cells.push(extra);
  }
  return cells;
}
function glob(pattern, value) {
  const regex = pattern.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*\*/g, '\0').replace(/\*/g, '[^/]*').replace(/\0/g, '.*');
  return new RegExp(`^${regex}$`).test(value);
}
export function triggered(workflow, scenario) {
  const on = workflow.on;
  const event = scenario.eventName;
  if (typeof on === 'string') return on === event;
  if (Array.isArray(on)) return on.includes(event);
  if (!on || !(event in on)) return false;
  const config = on[event] ?? {};
  const supported = new Set(['types', 'workflows', 'branches', 'branches-ignore', 'tags', 'tags-ignore', 'paths', 'paths-ignore', 'inputs', 'secrets', 'outputs']);
  if (event === 'schedule') {
    if (!Array.isArray(config) || config.some(c => typeof c.cron !== 'string' || Object.keys(c).some(k => k !== 'cron'))) throw new Error('unsupported schedule trigger');
  } else if (Object.keys(config).some(key => !supported.has(key))) throw new Error(`unsupported trigger field for ${event}`);
  if (config.types && !config.types.includes(scenario.event.action)) return false;
  if (config.workflows && !config.workflows.includes(scenario.event.workflow_run?.name)) return false;
  const tagPush = event === 'push' && scenario.ref.startsWith('refs/tags/');
  if (tagPush && (config.branches || config['branches-ignore']) && !config.tags && !config['tags-ignore']) return false;
  if (event === 'push' && !tagPush && (config.tags || config['tags-ignore']) && !config.branches && !config['branches-ignore']) return false;
  const tag = scenario.ref.replace(/^refs\/tags\//, '');
  if (tagPush && config.tags && !config.tags.some(p => glob(p, tag))) return false;
  if (tagPush && config['tags-ignore']?.some(p => glob(p, tag))) return false;
  const branch = event === 'pull_request' ? scenario.event.pull_request.base.ref : scenario.ref.replace(/^refs\/heads\//, '');
  if (!tagPush && config.branches && !config.branches.some(p => glob(p, branch))) return false;
  if (!tagPush && config['branches-ignore']?.some(p => glob(p, branch))) return false;
  if (config.paths && !scenario.files.some(f => config.paths.some(p => glob(p, f)))) return false;
  if (config['paths-ignore'] && scenario.files.every(f => config['paths-ignore'].some(p => glob(p, f)))) return false;
  return true;
}
export function resolveRun(workflows, file, scenario, chain = []) {
  if (chain.includes(file)) throw new Error(`recursive workflow call: ${[...chain,file].join(' -> ')}`);
  const workflow = workflows[file];
  if (!workflow) throw new Error(`unresolved workflow call ${file}`);
  const github = { event_name: scenario.eventName, event: scenario.event, repository: 'nickderobertis/allowlister', ref: scenario.ref, sha: scenario.head, token: 'transport-double' };
  const context = { github, inputs: scenario.inputs ?? {}, vars: { PUBLISH_TO_CRATES_IO: scenario.publishCrate ? 'true' : 'false' }, needs: {} };
  const rows = [];
  const pending = new Map(Object.entries(workflow.jobs));
  while (pending.size) {
    let progressed = false;
    for (const [id, job] of pending) {
      const deps = job.needs ? [job.needs].flat() : [];
      if (deps.some(d => pending.has(d))) continue;
      if (deps.some(d => !(d in context.needs))) throw new Error(`${file}:${id}: missing prerequisite`);
      progressed = true;
      pending.delete(id);
      const local = { ...context, needs: Object.fromEntries(deps.map(d => [d, context.needs[d]])) };
      const implicit = deps.every(d => context.needs[d].result === 'success');
      const enabled = job.if === undefined ? implicit : Boolean(evaluate(job.if, local)) && (/\b(always|failure|cancelled|success)\s*\(/.test(job.if) || implicit);
      const outputs = id === 'policy' && file === 'ci.yml' ? scenario.policyOutputs : {};
      if (id === 'policy' && file === 'ci.yml' && !outputs) throw new Error('policy outputs must come from the actual workflow step');
      context.needs[id] = { result: enabled ? 'success' : 'skipped', outputs };
      if (!enabled) { rows.push({ workflow: file, job: id, state: 'skipped', macos: false }); continue; }
      for (const matrix of expandMatrix(job.strategy?.matrix, local)) {
        const cell = { ...local, matrix };
        if (job.uses) {
          const call = resolve(job.uses, cell);
          if (!call.startsWith('./.github/workflows/')) throw new Error(`unresolved external reusable workflow ${call}`);
          rows.push(...resolveRun(workflows, call.split('/').at(-1), { ...scenario, inputs: resolve(job.with ?? {}, cell) }, [...chain,file]).map(row => ({...row, caller: `${file}:${id}`})));
          continue;
        }
        const runner = resolve(job['runs-on'], cell);
        const labels = [runner].flat();
        if (!labels.length || labels.some(label => typeof label !== 'string' || !/^(ubuntu|windows|macos)-[\w.-]+$/.test(label))) throw new Error(`${file}:${id}: unresolved runner ${JSON.stringify(runner)}`);
        rows.push({ workflow: file, job: id, name: resolve(job.name ?? id, cell), runner, target: matrix.target ?? null, targets: job.steps?.map(s => s.run?.match(/for target in ([^;]+);/)?.[1]).filter(Boolean).flatMap(s => s.split(/\s+/)) ?? [], state: 'requested', macos: labels.some(l => l.startsWith('macos-')), matrix });
      }
    }
    if (!progressed) throw new Error(`${file}: cyclic job dependencies`);
  }
  return rows;
}
export function sourceDigest() {
  const hash = createHash('sha256');
  function walk(dir) {
    for (const name of readdirSync(dir, { withFileTypes: true }).sort((a,b) => a.name.localeCompare(b.name))) {
      if (['node_modules', '.telemetry', 'AGENTS.md', 'CLAUDE.md'].includes(name.name)) continue;
      const path = join(dir, name.name);
      if (name.isDirectory()) walk(path);
      else if (name.isFile()) { hash.update(path.slice(root.length)); hash.update(readFileSync(path)); }
    }
  }
  walk(join(root, '.github'));
  for (const file of ['scripts/check-e2e-matrix.sh', 'scripts/dist.sh', 'scripts/verify-release.sh', 'release-plz.toml', 'rust-toolchain.toml', 'justfile', '.tool-versions']) { hash.update(file); hash.update(readFileSync(join(root,file))); }
  return hash.digest('hex');
}

// Each successful parent run emits its own workflow_run event. Preserve repeated
// parents and exact head identity; cyclic automatic fan-out is an explicit error.
export function followDownstream(workflows, parents, defaults) {
  const result = [];
  const queue = parents.map(parent => ({ parent, lineage: [parent.workflow] }));
  while (queue.length) {
    const { parent, lineage } = queue.shift();
    for (const [file, workflow] of Object.entries(workflows)) {
      if (!workflow.on?.workflow_run) continue;
      const scenario = { ...defaults, eventName: 'workflow_run', event: { action: 'completed', workflow_run: { name: workflows[parent.workflow].name, head_sha: parent.head, conclusion: 'success' } }, head: parent.head, classification: {macosRequired: false} };
      if (!triggered(workflow, scenario)) continue;
      if (lineage.includes(file)) throw new Error(`cyclic downstream workflow trigger: ${[...lineage,file].join(' -> ')}`);
      const run = { workflow: file, event: `workflow_run:${parent.workflow}`, head: parent.head, jobs: resolveRun(workflows, file, scenario) };
      result.push(run); queue.push({ parent: run, lineage: [...lineage,file] });
    }
  }
  return result;
}

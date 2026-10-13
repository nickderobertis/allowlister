// Fail closed for PRs: only the configured same-repository release branch plus
// a semantic version/changelog diff authorizes omitting the Mac lane.
// Exit 0: JSON classification (unreadable PRs conservatively require Mac).
// Exit 1: invalid command invocation.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { parseArgs } from 'node:util';
import { releaseBranchPrefix } from './scripts/gate-tier.mjs';

function git(args) {
  return execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}
function toml(text) {
  return JSON.parse(execFileSync(process.platform === 'win32' ? 'python' : 'python3', ['-c', 'import sys,json,tomllib; print(json.dumps(tomllib.loads(sys.stdin.read())))'], { input: text, encoding: 'utf8' }));
}
// Numeric identifiers forbid leading zeroes; prerelease identifiers must be
// numeric or contain a letter/hyphen, and every dotted identifier is nonempty.
const version = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;
function stripVersion(value, lock) {
  if (lock) {
    const packages = (value.package ?? []).filter(pkg => pkg.name === 'allowlister' && pkg.source === undefined && pkg.checksum === undefined);
    if (packages.length !== 1) throw new Error('lockfile must identify exactly one local allowlister package');
    for (const pkg of packages) {
      if (!version.test(pkg.version)) throw new Error('invalid package version');
      delete pkg.version;
    }
    // Dependency versions/checksums, sources and all other fields remain intact.
  } else {
    if (value.package?.name !== 'allowlister' || !version.test(value.package.version)) throw new Error('not a versioned allowlister manifest');
    delete value.package.version;
  }
  return value;
}
function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(k => [k, canonical(value[k])]));
  return value;
}
const nonPrEvents = new Set(['push','release','workflow_run','repository_dispatch','schedule','workflow_dispatch']);
export function classify(eventName, event, base, head) {
  const required = reason => ({ isReleaseOnly: false, macosRequired: true, reason });
  if (nonPrEvents.has(eventName)) return { isReleaseOnly: false, macosRequired: false, reason: 'non-PR event' };
  if (eventName !== 'pull_request') return required('unknown event name; Mac coverage required');
  try {
    const pr = event.pull_request;
    const repo = event.repository?.full_name;
    if (typeof repo !== 'string' || !/^[^/\s]+\/[^/\s]+$/.test(repo) || pr?.head?.repo?.full_name !== repo || pr?.base?.repo?.full_name !== repo || !pr?.head?.ref?.startsWith(releaseBranchPrefix())) return required('ordinary or untrusted PR branch');
    if (!base || !head || base.startsWith('-') || head.startsWith('-')) return required('missing or invalid revisions');
    const mergeBase = git(['merge-base', base, head]).trim();
    const changes = git(['diff', '--name-status', '-z', '--no-renames', mergeBase, head]).split('\0').filter(Boolean);
    if (!changes.length) return required('empty diff');
    for (let i = 0; i < changes.length; i += 2) {
      const [status, file] = changes.slice(i, i + 2);
      if (status !== 'M') return required('release-only files must already exist');
      const before = git(['show', `${mergeBase}:${file}`]);
      const after = git(['show', `${head}:${file}`]);
      if (file === 'CHANGELOG.md') {
        if (!/^# Changelog\b/m.test(after) || !/^## \[?\d+\.\d+\.\d+/m.test(after)) return required('unparsed changelog');
      } else if (file === 'Cargo.toml' || file === 'Cargo.lock') {
        const old = stripVersion(toml(before), file.endsWith('.lock'));
        const next = stripVersion(toml(after), file.endsWith('.lock'));
        if (JSON.stringify(canonical(old)) !== JSON.stringify(canonical(next))) return required(`non-version fields changed in ${file}`);
      } else return required(`non-release file: ${file}`);
    }
    return { isReleaseOnly: true, macosRequired: false, reason: 'same-repository release-plz branch with semantic version/changelog-only diff' };
  } catch (error) {
    return required(`unreadable classification: ${error.message}`);
  }
}
if (import.meta.main) {
  const { values, positionals } = parseArgs({ options: { event: { type: 'string' }, base: { type: 'string' }, head: { type: 'string' } }, allowPositionals: true });
  if (positionals[0] !== 'classify') throw new Error('usage: macos-policy.mjs classify --event JSON --base REF --head REF');
  let event = {};
  let unreadable = false;
  try { event = JSON.parse(readFileSync(values.event, 'utf8')); if (!event || Array.isArray(event) || typeof event !== 'object') throw new Error('not an object'); } catch { unreadable = true; }
  const eventName = process.env.GITHUB_EVENT_NAME ?? event.event_name ?? 'pull_request';
  const result = unreadable && eventName === 'pull_request' ? { isReleaseOnly: false, macosRequired: true, reason: 'unreadable PR payload' } : classify(eventName, event, values.base, values.head);
  console.log(JSON.stringify(result));
}

// GitHub job outcomes are the boundary: omissions require a successful explicit
// classifier result; missing, cancelled and failed prerequisites never pass.
// Exit 0: proven success or authorized omission; 1: invalid/failed prerequisites.
import { readFileSync } from 'node:fs';
if (process.argv.length !== 3) throw new Error('usage: macos-result.mjs NEEDS_JSON_PATH');
const [needsPath] = process.argv.slice(2);
const needs = JSON.parse(readFileSync(needsPath, 'utf8'));
if (needs.policy?.result !== 'success') throw new Error('Mac policy prerequisite did not succeed');
const required = needs.policy.outputs?.macosRequired;
if (!['true', 'false'].includes(required)) throw new Error('missing Mac policy decision');
const result = needs.macos?.result;
if (required === 'true' && result !== 'success') throw new Error(`required Mac job ${result ?? 'missing'}`);
if (required === 'false' && result !== 'skipped') throw new Error(`unauthorized Mac outcome ${result ?? 'missing'}`);
console.log(required === 'true' ? 'Mac lane job succeeded' : 'Mac lane intentionally omitted by classifier; no Mac tests ran');

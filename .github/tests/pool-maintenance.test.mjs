import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { loadWorkflows, evaluate } from '../workflow-graph.mjs';

const ci = loadWorkflows()['ci.yml'];
test('maintenance budget follows a successful release-PR gate only', () => {
  const job = ci.jobs['pool-maintenance'];
  assert.deepEqual(job.needs, ['test']);
  assert.equal(ci.jobs.test.outputs['gate-tier'], '${{ steps.tier.outputs.tier }}');
  assert.equal(job.steps.at(-1).run, 'just budgets');
  for (const [event, tier, result, expected] of [
    ['pull_request', 'all', 'success', true],
    ['pull_request', 'affected', 'success', false],
    ['push', 'all', 'success', false],
    ['workflow_dispatch', 'all', 'success', false],
    ['pull_request', 'all', 'failure', false],
  ]) {
    const context = { github: { event_name: event }, needs: { test: { result, outputs: { 'gate-tier': tier } } } };
    assert.equal(evaluate(`success() && (${job.if})`, context), expected, `${event}/${tier}/${result}`);
  }
});

test('repository settings keep session setup and permissions without command-gating hooks', () => {
  const settings = JSON.parse(readFileSync(new URL('../../.claude/settings.json', import.meta.url), 'utf8'));
  for (const group of settings.hooks.PreToolUse ?? []) {
    for (const hook of group.hooks) {
      assert.doesNotMatch(hook.command ?? '', /allowlister\s+hook\s+claude-code/);
    }
  }
  assert.ok(settings.hooks.SessionStart.some(group =>
    group.hooks.some(hook => hook.command === 'bash "${CLAUDE_PROJECT_DIR}/scripts/session-setup.sh"')));
  assert.ok(settings.permissions.allow.includes('Bash(just bootstrap)'));
  assert.ok(settings.permissions.allow.includes('Bash(cargo metadata *)'));
});

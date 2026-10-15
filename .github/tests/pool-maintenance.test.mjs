import { test } from 'node:test';
import assert from 'node:assert/strict';
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

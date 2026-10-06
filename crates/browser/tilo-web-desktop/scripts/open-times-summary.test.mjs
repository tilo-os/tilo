import {test} from 'node:test';
import assert from 'node:assert/strict';
import {summarizeOpenTimes} from './open-times-summary.mjs';
const result = (opens, errors = []) => [{opens, errors}];
const open = (round, ms, opened = true) => ({round, ms, opened});
test('a slower repeated open fails even when every first open passed', () => {
  const summary = summarizeOpenTimes(result([open(1, 20), open(2, 251)]), 250);
  assert.equal(summary.passed, false);
  assert.equal(summary.first.max, 20);
  assert.equal(summary.steady.max, 251);
  assert.equal(summary.max, 251);
});
test('an unsuccessful or unavailable repeated open fails', () => {
  for (const row of [open(2, 30, false), open(2, NaN), open(2, null)]) {
    assert.equal(summarizeOpenTimes(result([open(1, 20), row]), 250).passed, false);
  }
});
test('runtime errors fail independently of good timing samples', () => {
  assert.equal(summarizeOpenTimes(result([open(1, 20)], ['WebGPU device lost']), 250).passed, false);
});
test('empty samples and invalid budgets fail closed', () => {
  assert.equal(summarizeOpenTimes(result([]), 250).passed, false);
  for (const budget of [NaN, 0, -1, Infinity]) assert.equal(summarizeOpenTimes(result([open(1, 20)]), budget).passed, false);
});
test('all pages and rounds share the inclusive unchanged budget', () => {
  const summary = summarizeOpenTimes([...result([open(1, 15), open(2, 200)]), ...result([open(1, 20), open(2, 250)])], 250);
  assert.equal(summary.passed, true);
  assert.equal(summary.opens, 4);
  assert.equal(summary.first.opens, 2);
  assert.equal(summary.steady.opens, 2);
});

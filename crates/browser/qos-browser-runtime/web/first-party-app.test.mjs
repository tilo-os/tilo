import test from 'node:test';
import assert from 'node:assert/strict';
import {firstPartyApp} from './first-party-app.mjs';

test('persisted retired first-party ids resolve only within their family', () => {
  for (const id of ['so.tilo.word-processor', 'dev.tilo.word-processor', 'dev.qos.word-processor']) {
    assert.equal(firstPartyApp(id, 'so.tilo.word-processor'), true, id);
  }
  assert.equal(firstPartyApp('dev.qos.chat', 'so.tilo.word-processor'), false);
  assert.equal(firstPartyApp('dev.qos.word-processor-x', 'so.tilo.word-processor'), false);
  assert.equal(firstPartyApp('dev.qos.desktop', 'dev.qos.desktop'), true);
  assert.equal(firstPartyApp('dev.qos.chat', 'third.party.chat'), false);
  assert.equal(firstPartyApp(undefined, 'so.tilo.chat'), false);
});

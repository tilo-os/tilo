import test from 'node:test';
import assert from 'node:assert/strict';
import {recoverableClose} from './close-recovery.mjs';

test('close begins the teardown once, and a failed settlement stays retryable', async () => {
  let began = 0, settled = 0, failing = true;
  const close = recoverableClose({
    begin() { began += 1; },
    async settle() { if (failing) throw Error('aborted close write'); settled += 1; },
  });
  await assert.rejects(close(), /aborted close write/);
  assert.equal(began, 1, 'a failed settlement does not undo the teardown that already ran');
  assert.equal(settled, 0);
  failing = false;
  await close();
  assert.equal(began, 1, 'the retry settles what was begun, and begins nothing again');
  assert.equal(settled, 1);
});

test('close is idempotent once it has settled', async () => {
  let began = 0, settled = 0;
  const close = recoverableClose({begin() { began += 1; }, async settle() { settled += 1; }});
  await close();
  await close();
  assert.deepEqual([began, settled], [1, 1]);
});

test('concurrent closes share the run already under way', async () => {
  let began = 0, settled = 0, release;
  const gate = new Promise(resolve => { release = resolve; });
  const close = recoverableClose({
    begin() { began += 1; },
    async settle() { settled += 1; await gate; },
  });
  const first = close(), second = close();
  assert.equal(first, second, 'a second caller gets the run in flight, not a second teardown');
  release();
  await Promise.all([first, second]);
  assert.deepEqual([began, settled], [1, 1]);
});

test('a failure reaches onError and still rejects the close', async () => {
  const errors = [];
  const close = recoverableClose({
    begin() {},
    async settle() { throw Error('commit refused'); },
    onError(error) { errors.push(error); },
  });
  await assert.rejects(close(), /commit refused/);
  assert.deepEqual(errors.map(error => error.message), ['commit refused']);
});

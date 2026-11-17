import test from 'node:test';
import assert from 'node:assert/strict';
import {changed, listen, next} from './changes.mjs';

test('a wait for the next change ends on the change, not on its time', async () => {
  const waiting = next(60_000);
  changed();
  assert.equal(await waiting, true);
});

test('a wait with no change ends when its time is up', async () => {
  assert.equal(await next(1), false);
});

test('every listener hears a change until it stops, and one that throws stops no other', async () => {
  let heard = 0;
  const failing = listen(() => { throw new Error('a broken listener'); });
  const stop = listen(() => { heard += 1; });
  const quiet = console.error;
  console.error = () => {};
  try { changed(); } finally { console.error = quiet; }
  stop();
  failing();
  changed();
  assert.equal(heard, 1);
  assert.equal(typeof globalThis.tiloChanges.next, 'function');
});

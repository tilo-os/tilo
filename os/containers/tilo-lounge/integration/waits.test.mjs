// The gate's waits end on what the desk says, not on how many frames this
// machine drew. A desk with nothing left to draw presents nothing, so a wait
// that counted frames alone would hang until its timeout — when the same wait
// read the desk's own answer, it would already be done.
import test from 'node:test';
import assert from 'node:assert/strict';
import {settle, settleSearch, searching} from './waits.mjs';

/** A page whose frames never advance and whose search owes nothing: the desk
 * has finished, it is just not drawing. */
function settledDesk({frames = 5, searchSearching = false} = {}) {
  const reads = [];
  return {
    reads,
    evaluate: async expression => {
      reads.push(expression);
      if (expression.includes('searchSearching')) return searchSearching;
      if (expression.includes('frames')) return frames;
      return false;
    },
  };
}

test('a settle ends on the desk saying it owes nothing, without a frame advancing', async () => {
  const desk = settledDesk({frames: 5});
  let slept = 0;
  const how = await settle({
    evaluate: desk.evaluate,
    by: 2,
    owed: () => false,
    sleep: async () => { slept += 1; },
    timeoutMs: 1_000,
  });
  assert.equal(how, 'settled', 'the wait ended on the desk owning no work, not on frames');
  assert.ok(slept <= 1, 'it did not spin on the clock while the desk owed nothing');
});

test('a settle waits for the frames it was asked for while the desk still owes them', async () => {
  // Each read of the frame count reports one more than the last: the desk is
  // presenting, and the wait ends once it has seen `by` more than it started
  // from — however many turns that takes.
  let frames = 0;
  const evaluate = async expression =>
    expression.includes('frames') ? frames++ : false;
  let turns = 0;
  const how = await settle({
    evaluate,
    by: 2,
    owed: () => true,
    sleep: async () => { turns += 1; },
    timeoutMs: 1_000,
  });
  assert.equal(how, 'frames');
  assert.ok(frames >= 3, 'the frame count was read until it had risen by the number asked for');
  assert.equal(turns, 1, 'it paused once, for the frame that had not come yet');
});

test('the search settle ends on searchSearching false, not on a frame', async () => {
  // Frames never move; the search owes nothing once it answers.
  let searchingNow = true;
  const tab = {
    evaluate: async expression => {
      if (expression.includes('searchSearching')) return searchingNow;
      if (expression.includes('frames')) return 7;
      return false;
    },
  };
  // The pause yields to the event loop, so the search can answer between reads.
  let turn = 0;
  const promises = settleSearch(tab, {by: 2, sleep: async () => {
    turn += 1;
    if (turn === 2) searchingNow = false;
    await new Promise(resolve => setImmediate(resolve));
  }, timeoutMs: 1_000});
  const how = await promises;
  assert.equal(how, 'settled', 'the search settle ended the moment the desk owed no rows');
});

test('the searching predicate reads only the desk’s own answer', async () => {
  const seen = [];
  const tab = {evaluate: async expression => { seen.push(expression); return true; }};
  assert.equal(await searching(tab), true);
  assert.match(seen.at(-1), /searchSearching/);
});

test('a settle with idleMs ends on the desk drawing nothing, not on its timeout', async () => {
  // The tour's case: no answer of the desk's own to read, frames that never
  // move, and a desk that has simply stopped drawing. Waiting for the timeout
  // spends it on every screen.
  let clock = 0;
  const evaluate = async expression => (expression.includes('frames') ? 5 : false);
  const how = await settle({
    evaluate,
    by: 3,
    owed: () => true,
    idleMs: 2_500,
    sleep: async ms => { clock += ms; },
    now: () => clock,
    timeoutMs: 20_000,
    pollMs: 100,
  });
  assert.equal(how, 'idle');
  assert.ok(clock >= 2_500 && clock < 4_000, `it ended after ${clock} ms of silence, not at its timeout`);
});

test('a desk still drawing keeps an idle wait alive until its frames come', async () => {
  // A frame a second on a slow rasteriser: every frame pushes the silence
  // back, so the wait ends on the third frame and never calls the desk idle.
  let clock = 0;
  let frame = 0;
  const evaluate = async expression => {
    if (!expression.includes('frames')) return false;
    clock += 1_000;
    return frame++;
  };
  const how = await settle({
    evaluate,
    by: 3,
    owed: () => true,
    idleMs: 2_500,
    sleep: async () => {},
    now: () => clock,
    timeoutMs: 20_000,
  });
  assert.equal(how, 'frames');
});

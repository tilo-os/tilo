// The gate's waits end on what the shell says, not on how many frames this
// machine drew. A shell with nothing left to draw presents nothing, so a wait
// that counted frames alone would hang until its timeout — when the same wait
// read the shell's own answer, it would already be done.
import test from 'node:test';
import assert from 'node:assert/strict';
import {reloaded, settle, settleSearch, searching, STRIP_REST, stripAtRest} from './waits.mjs';

/** A page whose frames never advance and whose search owes nothing: the shell
 * has finished, it is just not drawing. */
function settledShell({frames = 5, searchSearching = false} = {}) {
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

test('a settle ends on the shell saying it owes nothing, without a frame advancing', async () => {
  const shell = settledShell({frames: 5});
  let slept = 0;
  const how = await settle({
    evaluate: shell.evaluate,
    by: 2,
    owed: () => false,
    sleep: async () => { slept += 1; },
    timeoutMs: 1_000,
  });
  assert.equal(how, 'settled', 'the wait ended on the shell owning no work, not on frames');
  assert.ok(slept <= 1, 'it did not spin on the clock while the shell owed nothing');
});

test('a settle waits for the frames it was asked for while the shell still owes them', async () => {
  // Each read of the frame count reports one more than the last: the shell is
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
  assert.equal(how, 'settled', 'the search settle ended the moment the shell owed no rows');
});

test('the searching predicate reads only the shell’s own answer', async () => {
  const seen = [];
  const tab = {evaluate: async expression => { seen.push(expression); return true; }};
  assert.equal(await searching(tab), true);
  assert.match(seen.at(-1), /searchSearching/);
});

/** What the page answers for a navigation snapshot, as the page evaluates
 * [`STRIP_REST`]: the expression reads `globalThis.tiloMount`. */
function pageWith(snapshot) {
  const globalThis = {tiloMount: {navigation: {getSnapshot: () => snapshot}}};
  return new Function('globalThis', `return ${STRIP_REST}`)(globalThis);
}

test('the strip is at rest only when the shell says its pan has nowhere to go', () => {
  // A strip still easing, or one a page does not describe, is not at rest: a
  // wait on frames would time out on the first, and pass the second unseen.
  assert.equal(pageWith({desktopPan: {offset: 40, moving: true}}), '');
  assert.equal(pageWith({desktopPan: {offset: 40}}), '');
  assert.equal(pageWith({desktopPan: null}), '');
  // A strip at rest answers its pan, so the caller reads where it stopped.
  const pan = {workspace: '1', strip: '2', offset: 40, viewport: 800, content: 2000, max: 1200, moving: false};
  assert.deepEqual(JSON.parse(pageWith({desktopPan: pan})), pan);
  assert.equal(stripAtRest(pan), true);
  assert.equal(stripAtRest({...pan, moving: true}), false);
  assert.equal(stripAtRest(null), false);
});

test('a settle with idleMs ends on the shell drawing nothing, not on its timeout', async () => {
  // The tour's case: no answer of the shell's own to read, frames that never
  // move, and a shell that has simply stopped drawing. Waiting for the timeout
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

test('a shell still drawing keeps an idle wait alive until its frames come', async () => {
  // A frame a second on a slow rasteriser: every frame pushes the silence
  // back, so the wait ends on the third frame and never calls the shell idle.
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

/** A tab mid-reload: the old document answers (marked, its shell presented)
 * until `turns` polls in, when the new one takes over and presents after
 * `presents` more turns (never, when `presents` is null). */
function reloading({turns = 2, presents = 1} = {}) {
  let turn = 0;
  let reloadedAt = null;
  return {
    tab: {
      reload: async () => {},
      evaluate: async expression => {
        if (!expression.includes('readyState')) return true; // marking the old document
        turn += 1;
        if (reloadedAt === null && turn > turns) reloadedAt = turn;
        const old = reloadedAt === null;
        const up = presents === null ? null : reloadedAt !== null && turn > reloadedAt + presents ? 'presented' : null;
        return JSON.stringify({old, ready: true, phase: old ? 'presented' : up, seen: old ? 'old' : 'new'});
      },
    },
    turns: () => turn,
  };
}

test('a reload wait ends on the new document’s shell, never the one going away', async () => {
  // The old document keeps answering as a presented shell after the browser
  // took the reload. A wait that read that would judge the page that is going
  // away; this one ends only once the new document's shell says it is up.
  const {tab} = reloading({turns: 3, presents: 1});
  let clock = 0;
  const how = await reloaded(tab, {sleep: async ms => { clock += ms; }, now: () => clock, timeoutMs: 20_000});
  assert.equal(how, 'presented', 'the wait ended on the new document, not the old one’s shell');
  assert.ok(clock >= 500, `it waited ${clock} ms for the new document to stay`);
});

test('a page that presents and is replaced by the next navigation is waited past', async () => {
  // The reloaded document presents, then a navigation replaces it before the
  // next read: the wait must not take the first for the page that stayed.
  let turn = 0;
  const tab = {
    reload: async () => {},
    evaluate: async expression => {
      if (!expression.includes('readyState')) return true; // marking the old document
      turn += 1;
      const at = turn <= 1 ? {old: true, ready: true, phase: 'presented', seen: 'old'}
        : turn === 2 ? {old: false, ready: true, phase: 'presented', seen: 'first'}
        : turn === 3 ? {old: false, ready: true, phase: 'gpu-pending', seen: 'second'}
        : {old: false, ready: true, phase: 'presented', seen: 'second'};
      return JSON.stringify(at);
    },
  };
  let clock = 0;
  const how = await reloaded(tab, {sleep: async ms => { clock += ms; }, now: () => clock, timeoutMs: 20_000});
  assert.equal(how, 'presented');
  assert.ok(clock >= 300, `it waited past the page that was replaced (${clock} ms)`);
});

test('a reloaded document whose shell never says it is up is stalled', async () => {
  // A failure screen: the new document is there, its shell never is.
  const {tab} = reloading({turns: 1, presents: null});
  let clock = 0;
  const how = await reloaded(tab, {sleep: async ms => { clock += ms; }, now: () => clock, timeoutMs: 1_000});
  assert.equal(how, 'stalled');
});

test('a document that never reloads times out instead of judging the old page', async () => {
  const tab = {reload: async () => {}, evaluate: async expression =>
    (expression.includes('readyState')
      ? JSON.stringify({old: true, ready: true, phase: 'presented'})
      : true)};
  let clock = 0;
  const how = await reloaded(tab, {sleep: async ms => { clock += ms; }, now: () => clock, timeoutMs: 1_000});
  assert.equal(how, 'timeout');
});

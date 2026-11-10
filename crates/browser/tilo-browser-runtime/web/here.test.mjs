import test from 'node:test';
import assert from 'node:assert/strict';

import {createHere, PAIR_WAIT_MS, POST_EVERY_MS, TICK_MS} from './here.mjs';
import {runtimeFake} from './runtime-surface.mjs';

function fakeLayer(members = []) {
  return {
    available: true,
    publisherId: 'me',
    members: () => [...members],
    join(publisher) { members.push(publisher); },
  };
}

function harness({actions = [], window = 'a1', share = true} = {}) {
  const calls = [];
  const timers = [];
  let clock = 1_000_000;
  const state = {actions, window, share};
  const runtime = runtimeFake({
    desktop_set_people: (id, json) => calls.push(['people', JSON.parse(json)]),
    desktop_set_here: (id, json) => calls.push(['here', JSON.parse(json)]),
    desktop_here_tick: (id, since) => { calls.push(['tick', since]); return JSON.stringify(state); },
  });
  const posts = [];
  const peer = {
    participant: {label: 'Ann', color: '#112233'},
    userPublicKey: 'ab'.repeat(32),
    async here(win, list) { posts.push({window: win, actions: list}); return [{device: 'me', status: 'Working on Notes'}]; },
  };
  return {
    calls, timers, posts, state, runtime, peer,
    now: () => clock,
    advance(ms) { clock += ms; },
    setTimer: (fn, ms) => { timers.push({fn, ms}); return timers.length; },
    clearTimer: () => {},
    async runTimer() { const next = timers.shift(); await next.fn(); },
  };
}

const settle = () => new Promise(resolve => setImmediate(resolve));

test('the page posts its window and steps, and pushes everyone\'s lines, all over its session with the host', async () => {
  const h = harness({actions: [{id: '01', action: 'Edited the text', resource: 'Welcome', app: 'Notes', at_ms: 1}]});
  const layer = fakeLayer(['bob']);
  // Presence travels over the page's session and live plane alone: a
  // request beside them reaches a route the host does not serve (it once
  // read a watching count from the door's `presence`, which answered 405).
  const requests = [];
  const fetched = globalThis.fetch;
  globalThis.fetch = async url => { requests.push(String(url)); return new Response(null, {status: 405}); };
  let here;
  try {
    here = createHere({runtime: h.runtime, mountId: () => 7, active: () => true, layer: () => layer, peer: h.peer,
      now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
    await settle();
  } finally {
    globalThis.fetch = fetched;
  }
  assert.deepEqual(requests, [], 'no request of its own');
  assert.deepEqual(h.posts, [{window: 'a1', actions: h.state.actions}]);
  assert.deepEqual(h.calls.find(([kind]) => kind === 'here')[1], [{device: 'me', status: 'Working on Notes'}]);
  assert.equal(h.calls.filter(([kind]) => kind === 'people').length, 0, 'members come from the board, not from here');
  assert.equal(h.timers.at(-1).ms, TICK_MS);
  here.stop();
});

test('nothing new: no post until the refresh interval; a changed window posts at once', async () => {
  const h = harness();
  const layer = fakeLayer();
  createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => layer, peer: h.peer,
    now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
  await settle();
  assert.equal(h.posts.length, 1);
  h.advance(TICK_MS);
  await h.runTimer();
  assert.equal(h.posts.length, 1, 'unchanged within the interval');
  h.state.window = 'b2';
  h.advance(TICK_MS);
  await h.runTimer();
  assert.equal(h.posts.length, 2, 'the person moved to another window');
  h.advance(POST_EVERY_MS);
  await h.runTimer();
  assert.equal(h.posts.length, 3, 'refreshed so the host keeps them listed');
});

test('a hidden page neither posts nor reads', async () => {
  const h = harness();
  const here = createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => fakeLayer(), peer: h.peer,
    now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => false});
  await settle();
  assert.equal(h.posts.length, 0);
  assert.equal(h.calls.filter(([kind]) => kind === 'tick').length, 0);
  h.advance(TICK_MS);
  await h.runTimer();
  assert.equal(h.posts.length, 0, 'still nothing while hidden');
  here.stop();
});

test('a new here is paired with their person at the next tick, until the host names them', async () => {
  const h = harness();
  let known = [{device: 'me', user: 'ab'.repeat(32)}];
  h.peer.here = async (win, list) => { h.posts.push({window: win, actions: list}); return known; };
  const layer = fakeLayer();
  createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => layer, peer: h.peer,
    now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
  await settle();
  assert.equal(h.posts.length, 1);
  // Bob is here: the page asks the host, not only at its next refresh.
  layer.join('bob');
  h.advance(TICK_MS);
  await h.runTimer();
  assert.equal(h.posts.length, 2, 'asked as soon as it is noticed');
  // His page has not posted yet: ask again each tick until it has.
  h.advance(TICK_MS);
  await h.runTimer();
  assert.equal(h.posts.length, 3, 'still not known whose device it is');
  known = [...known, {device: 'bob', user: 'cd'.repeat(32)}];
  h.advance(TICK_MS);
  await h.runTimer();
  assert.equal(h.posts.length, 4);
  assert.deepEqual(h.calls.filter(([kind]) => kind === 'here').at(-1)[1], known);
  h.advance(TICK_MS);
  await h.runTimer();
  assert.equal(h.posts.length, 4, 'paired: back to the usual refresh');
  // A page that never posts is not asked about for ever.
  layer.join('quiet');
  h.advance(TICK_MS);
  await h.runTimer();
  const asked = h.posts.length;
  h.advance(PAIR_WAIT_MS);
  await h.runTimer();
  h.advance(TICK_MS);
  await h.runTimer();
  assert.ok(h.posts.length <= asked + 1, 'gave up waiting for its line');
});

test('asked to check while the page is asking, it checks again as soon as that answer is in', async () => {
  const h = harness();
  let release;
  const known = [{device: 'me', user: 'ab'.repeat(32)}];
  h.peer.here = (win, list) => {
    h.posts.push({window: win, actions: list});
    return new Promise(resolve => { release = () => resolve(known); });
  };
  const layer = fakeLayer();
  const here = createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => layer, peer: h.peer,
    now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
  assert.equal(h.posts.length, 1, 'the first post is in flight');
  // Bob is here while that post is out: asking again immediately would be
  // lost, so the page checks once the answer is in.
  layer.join('bob');
  here.wake();
  release();
  await settle();
  assert.equal(h.timers.at(-1).ms, 0, 'checks again at once, not in a tick');
  const ran = h.runTimer();
  await settle();
  assert.equal(h.posts.length, 2);
  release();
  await ran;
});

test('a post that failed is tried again on the next tick, not at the next refresh', async () => {
  const h = harness();
  let fail = true;
  h.peer.here = async (win, list) => {
    h.posts.push({window: win, actions: list});
    if (fail) throw new Error('not admitted yet');
    return [{device: 'me', user: 'ab'.repeat(32)}];
  };
  const layer = fakeLayer();
  createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => layer, peer: h.peer,
    now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
  await settle();
  assert.equal(h.posts.length, 1);
  fail = false;
  h.advance(TICK_MS);
  await h.runTimer();
  assert.equal(h.posts.length, 2);
  assert.equal(h.calls.filter(([kind]) => kind === 'here').length, 1);
});

test('their own words go on their presence once, and again only when they change', async () => {
  const h = harness();
  const said = [];
  h.peer.setStatus = text => said.push(text);
  h.state.status = 'Reviewing the plan';
  const here = createHere({runtime: h.runtime, mountId: () => 7, active: () => true, layer: () => fakeLayer(), peer: h.peer,
    now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
  await settle();
  assert.deepEqual(said, ['Reviewing the plan']);
  h.advance(TICK_MS);
  await h.runTimer();
  assert.deepEqual(said, ['Reviewing the plan'], 'unchanged words are not said again');
  h.state.status = null;
  h.advance(TICK_MS);
  await h.runTimer();
  assert.deepEqual(said, ['Reviewing the plan', null], 'cleared, they are taken off');
  here.stop();
});

test('a plane that cannot be reached is said once in the log, and joining is said when it comes back', async () => {
  const h = harness();
  const said = [];
  const warn = console.warn;
  const info = console.info;
  console.warn = line => said.push(['warn', line]);
  console.info = line => said.push(['info', line]);
  try {
    let note = null;
    h.peer.liveNote = () => note;
    const here = createHere({runtime: h.runtime, mountId: () => 7, active: () => true, layer: () => fakeLayer(),
      peer: h.peer, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer,
      visible: () => true});
    await settle();
    assert.deepEqual(said, []);
    note = 'this page cannot reach live presence';
    h.advance(TICK_MS);
    await h.runTimer();
    assert.deepEqual(said, [['warn', 'live: this page cannot reach live presence']]);
    h.advance(TICK_MS);
    await h.runTimer();
    assert.deepEqual(said, [['warn', 'live: this page cannot reach live presence']], 'said once while it lasts');
    note = null;
    h.advance(TICK_MS);
    await h.runTimer();
    assert.deepEqual(said.at(-1), ['info', 'live: presence joined']);
    here.stop();
  } finally {
    console.warn = warn;
    console.info = info;
  }
});

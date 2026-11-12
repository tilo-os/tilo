import test from 'node:test';
import assert from 'node:assert/strict';

import {createHere, TICK_MS} from './here.mjs';
import {runtimeFake} from './runtime-surface.mjs';

const OWN = 'ab'.repeat(32);
const ADA = 'cd'.repeat(32);

function fakeLayer(members = []) {
  return {presenceMembers: () => members.map(member => ({...member}))};
}

function harness() {
  const said = [];
  const timers = [];
  let clock = 1_000_000;
  const runtime = runtimeFake({
    desktop_say_doing: (id, now) => { said.push([id, now]); },
  });
  const peer = {userPublicKey: OWN, seen: [], membersSeen(people) { this.seen.push(people); }};
  return {
    said, timers, runtime, peer,
    now: () => clock,
    advance(ms) { clock += ms; },
    setTimer: (fn, ms) => { timers.push({fn, ms}); return timers.length; },
    clearTimer: () => {},
    runTimer() { timers.shift().fn(); },
  };
}

test('every tick the runtime says what the person is doing, over the live plane and nothing else', () => {
  const h = harness();
  const requests = [];
  const fetched = globalThis.fetch;
  globalThis.fetch = async url => { requests.push(String(url)); return new Response(null, {status: 405}); };
  let here;
  try {
    here = createHere({runtime: h.runtime, mountId: () => 7, active: () => true, layer: () => fakeLayer(), peer: h.peer,
      now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
  } finally {
    globalThis.fetch = fetched;
  }
  assert.deepEqual(requests, [], 'no request of its own: presence is the live plane alone');
  assert.deepEqual(h.said, [[7, 1_000_000]]);
  assert.equal(h.timers.at(-1).ms, TICK_MS);
  h.advance(TICK_MS);
  h.runTimer();
  assert.deepEqual(h.said.at(-1), [7, 1_000_000 + TICK_MS]);
  here.stop();
});

test('a hidden page says nothing', () => {
  const h = harness();
  const here = createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => fakeLayer(), peer: h.peer,
    now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => false});
  h.advance(TICK_MS);
  h.runTimer();
  assert.deepEqual(h.said, []);
  here.stop();
});

test('the people here, this person included, are one entry each with their newest profile stamp', () => {
  const h = harness();
  const seen = [];
  const layer = fakeLayer([
    {publisher: 'ada-laptop', user: ADA, label: 'Ada', profileMs: 10},
    {publisher: 'ada-phone', user: ADA, label: 'Ada', profileMs: 20},
    {publisher: 'own-phone', user: OWN, label: 'Me', profileMs: 30},
    {publisher: 'claude', user: OWN, label: 'Claude Code', agent: true, profileMs: 0},
    {publisher: 'nameless', label: 'Guest'},
  ]);
  createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => layer, peer: h.peer,
    photos: {seen: people => seen.push(people)}, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer,
    visible: () => true});
  const expected = [
    {user: OWN, label: 'Me', profile_ms: 30},
    {user: ADA, label: 'Ada', profile_ms: 20},
  ];
  assert.deepEqual(seen, [expected], 'an agent is not a person with a picture; an uncertified device is nobody yet');
  assert.deepEqual(h.peer.seen, [expected], 'the peer hears the same names');
});

test('a plane that cannot be reached is said once in the log, and joining is said when it comes back', () => {
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
      peer: h.peer, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
    assert.deepEqual(said, []);
    note = 'this page cannot reach live presence';
    h.advance(TICK_MS);
    h.runTimer();
    assert.deepEqual(said, [['warn', 'live: this page cannot reach live presence']]);
    h.advance(TICK_MS);
    h.runTimer();
    assert.deepEqual(said, [['warn', 'live: this page cannot reach live presence']], 'said once while it lasts');
    note = null;
    h.advance(TICK_MS);
    h.runTimer();
    assert.deepEqual(said.at(-1), ['info', 'live: presence joined']);
    here.stop();
  } finally {
    console.warn = warn;
    console.info = info;
  }
});

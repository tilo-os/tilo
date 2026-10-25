import test from 'node:test';
import assert from 'node:assert/strict';

import {createHere, liveMembers, PAIR_WAIT_MS, POST_EVERY_MS, TICK_MS, WATCHING_EVERY_MS} from './here.mjs';

function fakeLayer(members = []) {
  const handlers = {presence: new Set(), member: new Set(), state: new Set()};
  const on = kind => handler => { handlers[kind].add(handler); return () => handlers[kind].delete(handler); };
  return {
    available: true,
    publisherId: 'me',
    members: () => [...members],
    remoteState: () => [],
    onPresence: on('presence'),
    onMember: on('member'),
    onState: on('state'),
    presence(publisher, frame) { for (const handler of handlers.presence) handler({publisher, frame}); },
    join(publisher) { members.push(publisher); for (const handler of handlers.member) handler({publisher, active: true}); },
    handlers,
  };
}

function harness({actions = [], window = 'a1', share = true} = {}) {
  const calls = [];
  const timers = [];
  let clock = 1_000_000;
  const state = {actions, window, share};
  const runtime = {
    desktop_set_people: (id, json) => calls.push(['people', JSON.parse(json)]),
    desktop_set_here: (id, json) => calls.push(['here', JSON.parse(json)]),
    desktop_set_watching: (id, n) => calls.push(['watching', n]),
    desktop_here_tick: (id, since) => { calls.push(['tick', since]); return JSON.stringify(state); },
  };
  const posts = [];
  const peer = {
    participant: {label: 'Ann', color: '#112233'},
    userPublicKey: 'ab'.repeat(32),
    async here(win, list) { posts.push({window: win, actions: list}); return [{device: 'me', status: 'Working on Notes'}]; },
  };
  const fetches = [];
  const fetch = async url => { fetches.push(url); return {ok: true, json: async () => ({online: 3, watching: 2})}; };
  return {
    calls, timers, posts, fetches, state, runtime, peer, fetch,
    now: () => clock,
    advance(ms) { clock += ms; },
    setTimer: (fn, ms) => { timers.push({fn, ms}); return timers.length; },
    clearTimer: () => {},
    async runTimer() { const next = timers.shift(); await next.fn(); },
  };
}

const settle = () => new Promise(resolve => setImmediate(resolve));

test('live members carry an agent\'s record and activity, everyone\'s focus and a person\'s strip', () => {
  const seen = new Map([
    ['bot', {participant: {kind: 'agent', label: 'Scribe', color: '#010203', activity: 'Reading'}, focus: {app: 'a', record: 'r'}, place: '7'}],
    ['ann', {participant: {kind: 'person', label: 'Ann'}, focus: {app: 'a', record: 'r'}, place: '7', wave: {seq: 2, target: 'b0b'}, layout: {size: [10, 10], windows: []}}],
  ]);
  const layer = {available: true, members: () => ['bot', 'ann', 'new']};
  assert.deepEqual(liveMembers(layer, seen), [
    {publisher: 'bot', kind: 'agent', label: 'Scribe', focus: {app: 'a', record: 'r'}, activity: 'Reading', place: null, wave: null, layout: null},
    {publisher: 'ann', kind: 'person', label: 'Ann', focus: {app: 'a', record: 'r'}, activity: null, place: '7', wave: {seq: 2, target: 'b0b'}, layout: {size: [10, 10], windows: []}},
    {publisher: 'new', kind: 'person', label: '', focus: null, activity: null, place: null, wave: null, layout: null},
  ]);
  assert.equal(liveMembers({available: false}, seen), null, 'no live layer: the host lines decide');
});

test('the page posts its window and steps, pushes everyone\'s lines and the spectator count', async () => {
  const h = harness({actions: [{id: '01', action: 'Edited the text', resource: 'Welcome', app: 'Notes', at_ms: 1}]});
  const layer = fakeLayer(['bob']);
  const here = createHere({runtime: h.runtime, mountId: () => 7, active: () => true, layer: () => layer, peer: h.peer,
    presenceUrl: 'https://rooms.example/presence', fetch: h.fetch, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
  await settle();
  assert.deepEqual(h.posts, [{window: 'a1', actions: h.state.actions}]);
  assert.deepEqual(h.calls.find(([kind]) => kind === 'here')[1], [{device: 'me', status: 'Working on Notes'}]);
  assert.deepEqual(h.calls.find(([kind]) => kind === 'watching'), ['watching', 2]);
  assert.deepEqual(h.fetches, ['https://rooms.example/presence']);
  const people = h.calls.find(([kind]) => kind === 'people')[1];
  // With its person, so the desktop counts the person's other tabs as them.
  assert.deepEqual(people.own, {publisher: 'me', kind: 'person', label: 'Ann', user: 'ab'.repeat(32)});
  assert.deepEqual(people.members.map(m => m.publisher), ['bob']);
  assert.equal(h.timers.at(-1).ms, TICK_MS);
  here.stop();
});

test('nothing new: no post until the refresh interval; a changed window posts at once', async () => {
  const h = harness();
  const layer = fakeLayer();
  createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => layer, peer: h.peer,
    fetch: h.fetch, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
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
  assert.ok(WATCHING_EVERY_MS > TICK_MS);
  assert.equal(h.fetches.length, 0, 'no room service, no spectator count');
});

test('a hidden page neither posts nor reads; a member\'s name reaches the runtime as it arrives', async () => {
  const h = harness();
  const layer = fakeLayer();
  const here = createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => layer, peer: h.peer,
    fetch: h.fetch, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => false});
  await settle();
  assert.equal(h.posts.length, 0);
  assert.equal(h.calls.filter(([kind]) => kind === 'tick').length, 0);
  layer.join('bot');
  layer.presence('bot', {participant: {kind: 'agent', label: 'Scribe', activity: 'Summarising'}, focus: {app: 'x', record: 'y'}});
  const last = h.calls.filter(([kind]) => kind === 'people').at(-1)[1];
  assert.deepEqual(last.members, [{publisher: 'bot', kind: 'agent', label: 'Scribe', focus: {app: 'x', record: 'y'}, activity: 'Summarising', place: null, wave: null, layout: null}]);
  // A person's focus reaches the runtime too: in a room whose layout is
  // personal no window here is theirs, and the app they focus is all that
  // says where they are ("In Chat").
  layer.join('ann');
  layer.presence('ann', {participant: {kind: 'person', label: 'Ann'}, focus: {app: 'so.tilo.chat', record: 'lounge'}, place: '3'});
  const ann = h.calls.filter(([kind]) => kind === 'people').at(-1)[1].members.find(member => member.publisher === 'ann');
  assert.deepEqual(ann.focus, {app: 'so.tilo.chat', record: 'lounge'});
  here.stop();
  assert.equal(layer.handlers.presence.size, 0, 'stopping lets go of the layer');
});

test('someone arriving is paired with their person at once, so their face is never drawn in a guessed colour', async () => {
  const h = harness();
  let known = [{device: 'me', user: 'ab'.repeat(32)}];
  h.peer.here = async (win, list) => { h.posts.push({window: win, actions: list}); return known; };
  const layer = fakeLayer();
  createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => layer, peer: h.peer,
    fetch: h.fetch, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
  await settle();
  assert.equal(h.posts.length, 1);
  // Bob arrives: the page asks the host now, not at its next refresh.
  layer.join('bob');
  await settle();
  assert.equal(h.posts.length, 2, 'asked at once');
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
  await settle();
  const asked = h.posts.length;
  h.advance(PAIR_WAIT_MS);
  await h.runTimer();
  h.advance(TICK_MS);
  await h.runTimer();
  assert.ok(h.posts.length <= asked + 1, 'gave up waiting for its line');
});

test('someone arriving while the page is asking is asked about as soon as that answer is in', async () => {
  const h = harness();
  let release;
  let known = [{device: 'me', user: 'ab'.repeat(32)}];
  h.peer.here = (win, list) => {
    h.posts.push({window: win, actions: list});
    return new Promise(resolve => { release = () => resolve(known); });
  };
  const layer = fakeLayer();
  createHere({runtime: h.runtime, mountId: () => 1, active: () => true, layer: () => layer, peer: h.peer,
    fetch: h.fetch, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
  assert.equal(h.posts.length, 1, 'the first post is in flight');
  // That answer was made before Bob came: it does not know him.
  layer.join('bob');
  release();
  await settle();
  assert.equal(h.timers.at(-1).ms, 0, 'checks again at once, not in a tick');
  known = [...known, {device: 'bob', user: 'cd'.repeat(32)}];
  const ran = h.runTimer();
  await settle();
  assert.equal(h.posts.length, 2);
  release();
  await ran;
  assert.deepEqual(h.calls.filter(([kind]) => kind === 'here').at(-1)[1], known);
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
    fetch: h.fetch, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
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
    fetch: h.fetch, now: h.now, setTimer: h.setTimer, clearTimer: h.clearTimer, visible: () => true});
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

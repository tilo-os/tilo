import test from 'node:test';
import assert from 'node:assert/strict';

import {createHere, liveMembers, POST_EVERY_MS, TICK_MS, WATCHING_EVERY_MS} from './here.mjs';

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

test('live members carry an agent\'s record and activity, a person\'s strip but never their focus', () => {
  const seen = new Map([
    ['bot', {participant: {kind: 'agent', label: 'Scribe', color: '#010203', activity: 'Reading'}, focus: {app: 'a', record: 'r'}, place: '7'}],
    ['ann', {participant: {kind: 'person', label: 'Ann'}, focus: {app: 'a', record: 'r'}, place: '7'}],
  ]);
  const layer = {available: true, members: () => ['bot', 'ann', 'new']};
  assert.deepEqual(liveMembers(layer, seen), [
    {publisher: 'bot', kind: 'agent', label: 'Scribe', color: '#010203', focus: {app: 'a', record: 'r'}, activity: 'Reading', place: null},
    {publisher: 'ann', kind: 'person', label: 'Ann', color: null, focus: null, activity: null, place: '7'},
    {publisher: 'new', kind: 'person', label: '', color: null, focus: null, activity: null, place: null},
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
  assert.deepEqual(people.own, {publisher: 'me', kind: 'person', label: 'Ann', color: '#112233', user: 'ab'.repeat(32)});
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
  assert.deepEqual(last.members, [{publisher: 'bot', kind: 'agent', label: 'Scribe', color: null, focus: {app: 'x', record: 'y'}, activity: 'Summarising', place: null}]);
  here.stop();
  assert.equal(layer.handlers.presence.size, 0, 'stopping lets go of the layer');
});

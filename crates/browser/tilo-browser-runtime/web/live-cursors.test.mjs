import test from 'node:test';
import assert from 'node:assert/strict';
import {CHAT_FADE_MS, CHAT_LINGER_MS, POINTER_STALE_MS, REACTION_MS, chatOpacity, createLiveCursors, reactionAt, SETTLE_MS} from './live-cursors.mjs';

function surfaceElement() {
  const listeners = new Map();
  return {
    addEventListener(type, fn) { listeners.set(type, fn); },
    removeEventListener(type) { listeners.delete(type); },
    dispatch(type, event) { listeners.get(type)?.(event); },
    getBoundingClientRect: () => ({left: 10, top: 20}),
  };
}

function fakeLayer() {
  const handlers = {presence: new Set(), member: new Set()};
  const layer = {
    published: [], remotes: [],
    publishPresence(partial) { layer.published.push(partial); return true; },
    remoteState() { return layer.remotes; },
    onPresence(fn) { handlers.presence.add(fn); return () => handlers.presence.delete(fn); },
    onMember(fn) { handlers.member.add(fn); return () => handlers.member.delete(fn); },
    emit(kind, value) { for (const fn of handlers[kind]) fn(value); },
  };
  return layer;
}

function frames() {
  const queue = [];
  return {
    request: fn => { queue.push(fn); return queue.length; },
    cancel() {},
    run() { const current = queue.splice(0); for (const fn of current) fn(); return current.length; },
    get pending() { return queue.length; },
  };
}

function timers() {
  const pending = new Map();
  let next = 0;
  return {
    set: (fn, delay) => { pending.set(++next, {fn, delay}); return next; },
    clear: handle => pending.delete(handle),
    fire() { const current = [...pending.values()]; pending.clear(); for (const {fn} of current) fn(); },
    get delays() { return [...pending.values()].map(entry => entry.delay); },
  };
}

/** Live cursors over a fake surface, handing what they draw to `drawn`. */
function mount({clock = () => 0, ...options} = {}) {
  const surface = surfaceElement();
  const layer = fakeLayer();
  const f = frames();
  const t = timers();
  const drawn = [];
  const cursors = createLiveCursors({
    layer, surface, setPointers: pointers => drawn.push(pointers), now: clock,
    requestFrame: f.request, cancelFrame: f.cancel, setTimer: t.set, clearTimer: t.clear, ...options,
  });
  return {surface, layer, f, t, drawn, cursors, last: () => drawn.at(-1) ?? []};
}

test('publishes the local pointer relative to what it is over, and clears it on leave', () => {
  // The runtime names what is under a surface point (`pointer_anchor`).
  const asked = [];
  const anchor = (x, y) => {
    asked.push([x, y]);
    return x < 200 ? {strip: '7', at: [x + 300, y - 40]} : {window: {app: 'so.tilo.notes', record: 'note-1'}, at: [x - 200, y - 100]};
  };
  const {surface, layer, cursors} = mount({anchor});
  surface.dispatch('pointermove', {clientX: 110, clientY: 70});
  surface.dispatch('pointermove', {clientX: 260, clientY: 150});
  surface.dispatch('pointerleave', {});
  assert.deepEqual(asked, [[100, 50], [250, 130]], 'in surface pixels');
  assert.deepEqual(layer.published, [
    {pointer: {strip: '7', at: [400, 10]}},
    {pointer: {window: {app: 'so.tilo.notes', record: 'note-1'}, at: [50, 30]}},
    {pointer: null},
  ]);
  cursors.close();
});

test('a pointer over nothing the runtime names is not published as a point', () => {
  const {surface, layer, cursors} = mount();
  surface.dispatch('pointermove', {clientX: 110, clientY: 70});
  assert.deepEqual(layer.published, [{pointer: null}]);
  cursors.close();
});

test('hands the runtime remote pointers to draw and stops requesting frames once the room is idle', () => {
  let clock = 0;
  const {layer, f, drawn, cursors, last} = mount({clock: () => clock});
  f.run();
  assert.equal(f.pending, 0, 'an idle room requests no frames');
  assert.equal(drawn.length, 0, 'nothing to draw is not handed over');

  layer.remotes = [{publisher: 'aaaaaaaaaaaa', pointer: {strip: '1', at: [5, 6]}, stale: false, receivedAtMs: 0, participant: {kind: 'person', label: 'Ada', color: '#0ea5e9'}}];
  layer.emit('presence', {});
  f.run();
  assert.deepEqual(last(), [{publisher: 'aaaaaaaaaaaa', pointer: {strip: '1', at: [5, 6]}, name: 'Ada'}], 'the runtime places it in this layout and colours it as the room does');
  assert.deepEqual(cursors.shown(), ['aaaaaaaaaaaa']);
  assert.equal(f.pending, 1, 'keeps sampling while the pointer may move');

  clock = SETTLE_MS + 1;
  f.run();
  assert.equal(f.pending, 0, 'settles after the newest frame');
  assert.equal(drawn.length, 1, 'an unchanged set is not handed over again');

  // The member left: the layer drops its state and says so.
  layer.remotes = [];
  layer.emit('member', {publisher: 'aaaaaaaaaaaa', active: false});
  f.run();
  assert.deepEqual(last(), [], 'gone at once');
  cursors.close();
});

test("a pointer whose publisher went silent is taken down even when no frame arrives", () => {
  let clock = 1000;
  const {layer, f, t, cursors, last} = mount({clock: () => clock});
  layer.remotes = [{publisher: 'gone', pointer: {strip: '1', at: [5, 6]}, stale: false, receivedAtMs: 1000}];
  layer.emit('presence', {});
  f.run();
  assert.deepEqual(cursors.shown(), ['gone']);
  assert.deepEqual(t.delays, [POINTER_STALE_MS + 1], 'a timer waits for its silence');
  // Its page closed without a word: nothing more arrives.
  clock = 1000 + POINTER_STALE_MS + 1;
  t.fire();
  f.run();
  assert.deepEqual(last(), []);
  assert.ok(POINTER_STALE_MS < 5000, 'well before anyone would wonder whether they are still there');
  cursors.close();
});

test('marks agents apart and shows what they are doing', () => {
  const {layer, f, cursors, last} = mount();
  layer.remotes = [
    {publisher: 'aaaaaaaaaaaa', pointer: {strip: '1', at: [5, 6]}, stale: false, receivedAtMs: 0, participant: {kind: 'person', label: 'Ada', color: '#0ea5e9', activity: 'ignored'}},
    {publisher: 'bbbbbbbbbbbb', pointer: {strip: '1', at: [220, 47]}, stale: false, receivedAtMs: 0, participant: {kind: 'agent', label: 'Writer', color: '#7c3aed', activity: 'New note · Notes'}},
  ];
  layer.emit('presence', {});
  f.run();
  assert.deepEqual(last(), [
    {publisher: 'aaaaaaaaaaaa', pointer: {strip: '1', at: [5, 6]}, name: 'Ada'},
    {publisher: 'bbbbbbbbbbbb', pointer: {strip: '1', at: [220, 47]}, name: 'Writer', agent: true, activity: 'New note · Notes'},
  ]);
  cursors.close();
});

test("a member's cursor chat shows beside its pointer while typed and fades after it is sent", () => {
  let clock = 0;
  const {layer, f, cursors, last} = mount({clock: () => clock});
  const ada = 'aaaaaaaaaaaa';
  const remote = (chat, chatSentAt = null) => ({publisher: ada, pointer: {strip: '1', at: [40, 50]}, stale: false, receivedAtMs: clock, participant: {kind: 'person', label: 'Ada'}, chat, chatSentAt});

  layer.remotes = [remote({text: 'hello th', sent: false})];
  layer.emit('presence', {});
  f.run();
  assert.deepEqual(cursors.chats(), [[ada, 'hello th']], 'shown live while typed');
  assert.deepEqual(last()[0].chat, {text: 'hello th', typing: true, opacity: 1});

  clock = CHAT_LINGER_MS - CHAT_FADE_MS / 2;
  layer.remotes = [remote({text: 'hello there', sent: true}, 0)];
  layer.emit('presence', {});
  f.run();
  assert.deepEqual(cursors.chats(), [[ada, 'hello there']], 'kept after it is sent');
  const {opacity, typing} = last()[0].chat;
  assert.equal(typing, false);
  assert.ok(opacity < 1 && opacity > 0, 'and fading at the end');
  assert.ok(f.pending > 0, 'frames continue while it fades');

  clock = CHAT_LINGER_MS;
  f.run();
  assert.deepEqual(cursors.chats(), [], 'gone once it has lingered');
  cursors.close();
});

test('a reaction floats up from the pointer once and is gone after its life', () => {
  let clock = 1000;
  const {layer, f, cursors, last} = mount({clock: () => clock});
  layer.remotes = [{publisher: 'bbbbbbbbbbbb', pointer: {strip: '1', at: [40, 50]}, stale: false, receivedAtMs: 1000, reaction: {seq: 1, emoji: '\u{1f389}', at: 1000}}];
  layer.emit('presence', {});
  f.run();
  assert.deepEqual(last()[0].reaction, {emoji: '\u{1f389}', rise: 0, opacity: 1});
  clock = 1000 + REACTION_MS / 2;
  f.run();
  assert.ok(last()[0].reaction.rise > 0, 'it rises');
  clock = 1000 + REACTION_MS;
  f.run();
  assert.equal(last()[0].reaction, undefined);
  assert.equal(f.pending, 0, 'and the room is idle again');
  cursors.close();
});

test('closing takes every pointer down', () => {
  const {layer, f, cursors, last} = mount();
  layer.remotes = [{publisher: 'a', pointer: {strip: '1', at: [1, 2]}, stale: false, receivedAtMs: 0}];
  layer.emit('presence', {});
  f.run();
  cursors.close();
  assert.deepEqual(last(), []);
});

test('chat opacity: typed shows, empty does not, sent lingers then fades', () => {
  assert.equal(chatOpacity(null, null, 0), 0);
  assert.equal(chatOpacity({text: '', sent: false}, null, 0), 0);
  assert.equal(chatOpacity({text: 'hi', sent: false}, null, 99999), 1);
  assert.equal(chatOpacity({text: 'hi', sent: true}, 100, 100), 1);
  assert.equal(chatOpacity({text: 'hi', sent: true}, 100, 100 + CHAT_LINGER_MS), 0);
});

test('a reaction is shown only during its life', () => {
  assert.equal(reactionAt(null, 0), null);
  assert.equal(reactionAt({emoji: 'x', at: 10}, 5), null);
  assert.equal(reactionAt({emoji: 'x', at: 10}, 10 + REACTION_MS), null);
  assert.equal(reactionAt({emoji: 'x', at: 10}, 10 + REACTION_MS * 0.75).opacity, 0.5);
});

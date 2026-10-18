import test from 'node:test';
import assert from 'node:assert/strict';
import {createLiveCursors, placePointer, publishedPointer, SETTLE_MS} from './live-cursors.mjs';

function element() {
  const listeners = new Map();
  const node = {
    children: [], style: {}, dataset: {}, parent: null, textContent: '',
    setAttribute() {},
    append(child) { child.parent = node; node.children.push(child); },
    remove() { if (node.parent) node.parent.children = node.parent.children.filter(c => c !== node); node.parent = null; },
    addEventListener(type, fn) { listeners.set(type, fn); },
    removeEventListener(type) { listeners.delete(type); },
    dispatch(type, event) { listeners.get(type)?.(event); },
    getBoundingClientRect: () => ({left: 10, top: 20}),
  };
  return node;
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

test('publishes the local pointer relative to the surface and clears it on leave', () => {
  const surface = element();
  const layer = fakeLayer();
  const f = frames();
  const cursors = createLiveCursors({layer, surface, document: {createElement: element}, now: () => 0, requestFrame: f.request, cancelFrame: f.cancel});
  surface.dispatch('pointermove', {clientX: 110, clientY: 70});
  surface.dispatch('pointerleave', {});
  assert.deepEqual(layer.published, [{pointer: [100, 50]}, {pointer: null}]);
  cursors.close();
});

test('a pointer over the focused window leaves relative to it, as receivers place it', () => {
  const windows = [{app: 'so.tilo.notes', record: 'note-1', rect: [200, 100, 600, 400]}];
  const focus = {app: 'so.tilo.notes', record: 'note-1'};
  assert.deepEqual(publishedPointer([250, 130], focus, windows), [50, 30]);
  assert.deepEqual(placePointer(publishedPointer([250, 130], focus, windows), focus, windows), [250, 130]);
  assert.deepEqual(publishedPointer([250, 130], {app: 'so.tilo.browser-desktop', record: 'desktop'}, windows), [250, 130]);
  assert.deepEqual(publishedPointer([250, 130], null, windows), [250, 130]);
  const surface = element();
  const layer = fakeLayer();
  const f = frames();
  const cursors = createLiveCursors({layer, surface, document: {createElement: element}, windows: () => windows, localFocus: () => focus, now: () => 0, requestFrame: f.request, cancelFrame: f.cancel});
  surface.dispatch('pointermove', {clientX: 260, clientY: 150});
  assert.deepEqual(layer.published, [{pointer: [50, 30]}]);
  cursors.close();
});

test('draws remote pointers and stops requesting frames once the room is idle', () => {
  const surface = element();
  const layer = fakeLayer();
  const f = frames();
  let clock = 0;
  const cursors = createLiveCursors({layer, surface, document: {createElement: element}, now: () => clock, requestFrame: f.request, cancelFrame: f.cancel});
  f.run();
  assert.equal(f.pending, 0, 'an idle room requests no frames');

  layer.remotes = [{publisher: 'aaaaaaaaaaaa', pointer: [5, 6], stale: false}];
  layer.emit('presence', {});
  f.run();
  assert.deepEqual(cursors.shown(), ['aaaaaaaaaaaa']);
  const overlay = surface.children[0];
  assert.equal(overlay.children[0].style.transform, 'translate(5px,6px)');
  assert.equal(f.pending, 1, 'keeps sampling while the pointer may move');

  clock = SETTLE_MS + 1;
  f.run();
  assert.equal(f.pending, 0, 'settles after the newest frame');

  layer.emit('member', {publisher: 'aaaaaaaaaaaa', active: false});
  assert.deepEqual(cursors.shown(), []);
  cursors.close();
  assert.equal(surface.children.length, 0);
});

test('marks agents apart and shows what they are doing', () => {
  const surface = element();
  const layer = fakeLayer();
  const f = frames();
  const cursors = createLiveCursors({layer, surface, document: {createElement: element}, now: () => 0, requestFrame: f.request, cancelFrame: f.cancel});
  layer.remotes = [
    {publisher: 'aaaaaaaaaaaa', pointer: [5, 6], stale: false, participant: {kind: 'person', label: 'Ada', color: '#0ea5e9', activity: null}},
    {publisher: 'bbbbbbbbbbbb', pointer: [220, 47], stale: false, participant: {kind: 'agent', label: 'Writer', color: '#7c3aed', activity: 'New note · Notes'}},
  ];
  layer.emit('presence', {});
  f.run();
  const [person, agent] = surface.children[0].children;
  assert.equal(person.textContent, 'Ada');
  assert.equal(person.dataset.kind, 'person');
  assert.equal(agent.textContent, 'Writer · New note · Notes');
  assert.equal(agent.dataset.kind, 'agent');
  assert.match(agent.style.cssText, /#7c3aed/);
  assert.match(agent.style.cssText, /box-shadow/);
  assert.equal(agent.style.transform, 'translate(220px,47px)');

  layer.remotes[1] = {...layer.remotes[1], pointer: [900, 400], participant: {...layer.remotes[1].participant, activity: 'New sheet · Workbook'}};
  layer.emit('presence', {});
  f.run();
  assert.equal(agent.textContent, 'Writer · New sheet · Workbook', 'the activity follows the agent');
  assert.equal(agent.style.transform, 'translate(900px,400px)', 'the marker moves between controls');
  cursors.close();
});

test('a focused pointer is drawn inside the window of its record', () => {
  const windows = [
    {app: 'so.tilo.notes', record: 'uuid:notes', rect: [0, 40, 400, 600]},
    {app: 'so.tilo.sheets', record: 'uuid:sheets', rect: [400, 40, 1000, 600]},
  ];
  assert.deepEqual(placePointer([30, 20], {app: 'so.tilo.sheets', record: 'uuid:sheets'}, windows), [430, 60]);
  assert.deepEqual(placePointer([900, 20], {app: 'so.tilo.notes', record: 'uuid:notes'}, windows), [399, 60], 'kept within the window');
  assert.equal(placePointer([30, 20], {app: 'so.tilo.notes', record: 'uuid:other'}, windows), null, 'hidden while no window shows its record');
  assert.deepEqual(placePointer([30, 20], null, windows), [30, 20], 'an unfocused pointer is in surface coordinates');
  assert.deepEqual(placePointer([30, 20], {app: 'a', record: 'r'}, undefined), [30, 20], 'a single application has no windows');
  assert.deepEqual(placePointer([30, 20], {app: 'so.tilo.browser-desktop', record: 'desktop'}, windows), [30, 20], 'a member focused on the desktop points in surface coordinates');
});

test('an app\'s room record is told apart by its app', () => {
  // The live layer names an app's room record '' whichever app it is.
  const windows = [
    {app: 'so.tilo.notes', record: '', rect: [0, 40, 400, 600]},
    {app: 'so.tilo.chat', record: '', rect: [400, 40, 1000, 600]},
  ];
  const chat = {app: 'so.tilo.chat', record: ''};
  assert.deepEqual(publishedPointer([450, 100], chat, windows), [50, 60]);
  assert.deepEqual(placePointer([50, 60], chat, windows), [450, 100]);
  assert.equal(placePointer([50, 60], {app: 'so.tilo.sheets', record: ''}, windows), null);
});

test('a focused pointer is drawn as a marker inside its window', () => {
  const windows = [
    {app: 'so.tilo.notes', record: 'uuid:notes', rect: [0, 40, 400, 600]},
    {app: 'so.tilo.sheets', record: 'uuid:sheets', rect: [400, 40, 1000, 600]},
  ];
  const surface = element();
  const layer = fakeLayer();
  const f = frames();
  const cursors = createLiveCursors({layer, surface, document: {createElement: element}, now: () => 0, requestFrame: f.request, cancelFrame: f.cancel, windows: () => windows});
  layer.remotes = [
    {publisher: 'agent', pointer: [30, 20], focus: {app: 'so.tilo.sheets', record: 'uuid:sheets'}, participant: {kind: 'agent', label: 'Writer', color: '#db2777'}, stale: false},
    {publisher: 'away', pointer: [5, 5], focus: {app: 'so.tilo.slides', record: 'uuid:elsewhere'}, stale: false},
  ];
  layer.emit('presence', {});
  f.run();
  assert.deepEqual(cursors.shown(), ['agent']);
  assert.equal(surface.children[0].children[0].style.transform, 'translate(430px,60px)');
  cursors.close();
});

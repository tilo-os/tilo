import test from 'node:test';
import assert from 'node:assert/strict';
import {
  CONTENT_SPACE_X, PEER_PALETTE, commandDrag, createDragTracker, createGhostTracker, createLiveOverlay, createOverlayView,
  createSelectionProjector, createTypingTracker, TYPING_EXPIRE_MS, labelInk, localPointerPresence, memberColor, memberLabel, rectForRecord, remotePointerPosition,
} from './live-overlay.mjs';
import {createRemotePresence} from './live-presence.mjs';

const WRITER = {app: 'so.tilo.word-processor', record: 'uuid:doc'};
const CHAT = {app: 'so.tilo.chat', record: '@qos:package-primary'};
const G1 = '0000000000000001';
const G2 = '0000000000000002';
const G3 = '0000000000000003';
/** Two members see the same windows at different places and sizes. */
const snapshotA = {
  focus: WRITER, focusRect: [100, 50, 600, 700], focusGroup: G1, selection: null, drag: null,
  groups: [{id: G1, rect: [100, 50, 600, 700], record: WRITER}, {id: G2, rect: [710, 50, 400, 700], record: CHAT}],
};
const snapshotB = {
  focus: CHAT, focusRect: [20, 30, 300, 500], focusGroup: G2, selection: null, drag: null,
  groups: [{id: G2, rect: [20, 30, 300, 500], record: CHAT}, {id: G1, rect: [330, 30, 500, 500], record: WRITER}],
};

test('member colours are stable, match the Rust vectors and label ink contrasts', () => {
  assert.equal(memberColor('alice'), '#ec4899');
  assert.equal(memberColor('dave'), PEER_PALETTE[23]);
  assert.equal(memberColor('eve'), PEER_PALETTE[21]);
  assert.equal(memberColor('a1b2c3d4'), PEER_PALETTE[21]);
  assert.equal(memberColor('7f00aa55'.repeat(8)), PEER_PALETTE[13]);
  assert.equal(memberColor('dave'), memberColor('dave'));
  assert.equal(labelInk('#dfdc3a'), '#111111');
  assert.equal(labelInk('#6366f1'), '#ffffff');
  assert.equal(memberLabel('abcdef12', {name: '  Ada  '}), 'Ada');
  assert.equal(memberLabel('abcdef12', null), 'Guest abcd');
  assert.equal(memberLabel('abcdef12', {name: null, owner: true}), 'Owner');
  assert.equal(memberLabel('abcdef12', {name: 'Xaver', owner: true}), 'Xaver');
});

test('content-space pointers stay on the same content when the two people are scrolled differently', () => {
  // A scrolled its Writer 400 px down, B 200 px.
  const a = {...snapshotA, focusScroll: 400, groups: [{...snapshotA.groups[0], scroll: 400}, snapshotA.groups[1]]};
  const b = {...snapshotB, groups: [snapshotB.groups[0], {...snapshotB.groups[1], scroll: 200}]};
  const published = localPointerPresence(a, [400, 250]);
  assert.deepEqual(published, {focus: WRITER, pointer: [CONTENT_SPACE_X + 300, 600]}, 'content y = window y + scroll');
  // B's Writer is at (330, 30): content y 600 is 400 px into its window.
  assert.deepEqual(remotePointerPosition(b, published), [630, 430]);
  // Content above B's view: hidden instead of pinned to the wrong line.
  const high = localPointerPresence({...a, focusScroll: 0}, [400, 60]);
  assert.equal(remotePointerPosition({...b, groups: [b.groups[0], {...b.groups[1], scroll: 300}]}, high), null);
  // Outside the focused window: window-relative as before (no scroll applied).
  assert.deepEqual(localPointerPresence(a, [50, 20]), {focus: WRITER, pointer: [-50, -30]});
  // A window without a scroll region: window-relative.
  assert.deepEqual(localPointerPresence(snapshotA, [400, 250]).pointer, [300, 200]);
  // Receiver without a scroll offset for the record treats it as 0.
  assert.deepEqual(remotePointerPosition(snapshotB, localPointerPresence(a, [400, 100])), [630, 480]);
});

test('pointers travel in the focused window coordinates and land on the receiver window for that record', () => {
  const published = localPointerPresence(snapshotA, [400, 250]);
  assert.deepEqual(published, {focus: WRITER, pointer: [300, 200]});
  // B shows Writer at a different place: the cursor keeps its offset inside the window.
  assert.deepEqual(remotePointerPosition(snapshotB, published), [630, 230]);
  // A record B does not show: the cursor is hidden rather than misplaced.
  assert.equal(remotePointerPosition({...snapshotB, groups: [snapshotB.groups[0]], focus: CHAT}, published), null);
  // No focused record: desktop coordinates both ways.
  const desktop = localPointerPresence({...snapshotA, focus: null, focusRect: null}, [12.345, 7]);
  assert.deepEqual(desktop, {focus: null, pointer: [12.35, 7]});
  assert.deepEqual(remotePointerPosition(snapshotB, desktop), [12.35, 7]);
  assert.deepEqual(localPointerPresence(snapshotA, null), {focus: WRITER, pointer: null});
  assert.equal(remotePointerPosition(snapshotB, {focus: WRITER, pointer: null}), null);
});

test('the focused window wins when a record is open twice', () => {
  const twice = {...snapshotA, focusRect: [900, 10, 100, 100], groups: [...snapshotA.groups, {id: G3, rect: [900, 10, 100, 100], record: WRITER}]};
  assert.deepEqual(rectForRecord(twice, WRITER), [900, 10, 100, 100]);
  assert.deepEqual(rectForRecord({...twice, focus: CHAT}, WRITER), [100, 50, 600, 700]);
  assert.equal(rectForRecord(twice, {app: 'x', record: 'y'}), null);
});

const dragSnapshot = (offset, order = [G1, G2], kind = 'move') => ({
  ...snapshotA,
  groups: order.map(id => snapshotA.groups.find(g => g.id === id)),
  drag: offset ? {kind, object: `${kind === 'move' ? '01' : '02'}${G1}`, group: G1, offset, rect: [100, 50, 600, 700]} : null,
});

test('drag tracker publishes Start, Move, End for a reorder that changed the order', () => {
  const tracker = createDragTracker();
  assert.equal(tracker.update(snapshotA), undefined);
  assert.deepEqual(tracker.update(dragSnapshot([10, 0])), {object: {record: null, id: `01${G1}`}, phase: 'start', position: [10, 0], size: [600, 700]});
  assert.equal(tracker.update(dragSnapshot([250, 3])).phase, 'move');
  assert.ok(tracker.active);
  const end = tracker.update(dragSnapshot(null, [G2, G1]));
  assert.deepEqual(end, {object: {record: null, id: `01${G1}`}, phase: 'end', position: [250, 3], size: [600, 700]});
  assert.equal(tracker.update(dragSnapshot(null, [G2, G1])), undefined);
});

test('drag tracker cancels a released reorder that left the order unchanged; resizes end', () => {
  const tracker = createDragTracker();
  tracker.update(dragSnapshot([40, 0]));
  assert.equal(tracker.update(dragSnapshot(null)).phase, 'cancel');
  tracker.update(dragSnapshot([0, 0], [G1, G2], 'resize'));
  assert.equal(tracker.update(dragSnapshot(null)).phase, 'end');
});

test('ghosts follow a live remote drag, hold at the drop until the durable order lands, and vanish on cancel', () => {
  const ghosts = createGhostTracker();
  const remote = drag => ({publisher: 'p1', drag, endedDrags: []});
  const moving = {object: {record: null, id: `01${G1}`}, phase: 'move', position: [30, -5], size: [500, 500], snapped: false};
  assert.deepEqual(ghosts.ghosts(snapshotB, [remote(moving)]).map(g => g.rect), [[360, 25, 500, 500]]);
  const ended = {...moving, phase: 'end', position: [-310, 0], snapped: true};
  const held = ghosts.ghosts(snapshotB, [remote(ended)]);
  assert.equal(held.length, 1);
  assert.ok(held[0].released);
  assert.deepEqual(held[0].rect, [20, 30, 500, 500]);
  assert.equal(ghosts.ghosts(snapshotB, [remote(ended)]).length, 1, 'still held before the durable change');
  const landed = {...snapshotB, groups: [snapshotB.groups[1], snapshotB.groups[0]]};
  assert.equal(ghosts.ghosts(landed, [remote(ended)]).length, 0, 'the durable order change snaps the ghost away');
  assert.equal(ghosts.held, 1);
  assert.equal(ghosts.ghosts(landed, [remote(null)]).length, 0);
  assert.equal(ghosts.held, 0);
  const cancelled = {...moving, phase: 'cancel', snapped: true};
  assert.equal(ghosts.ghosts(snapshotB, [remote(cancelled)]).length, 0, 'cancel restores the durable layout');
  const resized = {object: {record: null, id: `02${G1}`}, phase: 'move', position: [0, 0], size: [640, 500], snapped: false};
  assert.deepEqual(ghosts.ghosts(snapshotB, [remote(resized)])[0].rect, [330, 30, 640, 500]);
  assert.equal(ghosts.ghosts(snapshotB, [remote({...resized, phase: 'end', snapped: true})]).length, 0);
  assert.equal(ghosts.ghosts(snapshotB, [remote({...moving, object: {record: WRITER, id: `01${G1}`}})]).length, 0, 'in-record objects are not windows');
});

test('drag preview lifecycle through the lane B receiver: interpolated moves, End held, durable snap, Cancel restore', () => {
  const receiver = createRemotePresence();
  const ghosts = createGhostTracker();
  const frame = (seq, drag, sentAtMs) => ({seq, sentAtMs, focus: null, pointer: [0, 0], selection: null, drag});
  const object = {record: null, id: `01${G1}`};
  receiver.accept(frame(1, {object, phase: 'start', position: [0, 0], size: [500, 500]}, 1000), 5000);
  receiver.accept(frame(2, {object, phase: 'move', position: [100, 0], size: [500, 500]}, 1016), 5016);
  const sample = at => ({publisher: 'p1', ...receiver.sample(at)});
  const mid = ghosts.ghosts(snapshotB, [sample(5016 - 8 + receiver.interpolationDelayMs())]);
  assert.equal(mid.length, 1);
  assert.ok(mid[0].rect[0] > 330 && mid[0].rect[0] < 430, `interpolated between samples: ${mid[0].rect[0]}`);
  receiver.accept(frame(3, {object, phase: 'end', position: [-310, 0], size: [500, 500]}, 1032), 5032);
  assert.deepEqual(ghosts.ghosts(snapshotB, [sample(5040)]).map(g => g.rect[0]), [20]);
  const landed = {...snapshotB, groups: [snapshotB.groups[1], snapshotB.groups[0]]};
  assert.equal(ghosts.ghosts(landed, [sample(5050)]).length, 0);
  // A new drag on the same window that is then cancelled leaves no ghost.
  receiver.accept(frame(4, {object, phase: 'start', position: [0, 0], size: [500, 500]}, 1100), 5100);
  assert.equal(ghosts.ghosts(landed, [sample(5110)]).length, 1);
  receiver.accept(frame(5, {object, phase: 'cancel', position: [0, 0], size: [500, 500]}, 1116), 5116);
  assert.equal(ghosts.ghosts(landed, [sample(5120)]).length, 0);
});

test('a pending window reorder command becomes a released drag to its destination', () => {
  const big = 9007199254740993n; // beyond Number.MAX_SAFE_INTEGER
  const hex = big.toString(16).padStart(16, '0');
  const snapshot = {groups: [{id: hex, rect: [0, 0, 300, 400]}, {id: G2, rect: [310, 0, 300, 400]}, {id: G3, rect: [620, 0, 200, 400]}]};
  const right = {command: `{"source":{"room":"r","heads":[]},"command":{"ReorderGroup":{"strip":7,"group":${big},"before":3}}}`};
  assert.deepEqual(commandDrag(snapshot, right), {object: {record: null, id: `01${hex}`}, phase: 'end', position: [320, 0], size: [300, 400]});
  const left = {command: `{"source":{"room":"r","heads":[]},"command":{"ReorderGroup":{"strip":7,"group":3,"before":2}}}`};
  assert.deepEqual(commandDrag(snapshot, left).position, [-310, 0]);
  const last = {command: `{"source":{"room":"r","heads":[]},"command":{"ReorderGroup":{"strip":7,"group":${big},"before":null}}}`};
  assert.deepEqual(commandDrag(snapshot, last).position, [520, 0]);
  assert.equal(commandDrag(snapshot, {command: '{"command":{"CloseTab":{}}}'}), null);
  assert.equal(commandDrag(null, right), null);
});

test('an optimistic reorder uses the displacement measured before the local move', () => {
  const big = 9007199254740993n;
  const hex = big.toString(16).padStart(16, '0');
  // The local snapshot already shows the moved order; the preview wins.
  const moved = {groups: [{id: G2, rect: [0, 0, 300, 400]}, {id: hex, rect: [310, 0, 300, 400]}]};
  const pending = {command: `{"source":{"room":"r","heads":[]},"command":{"ReorderGroup":{"strip":7,"group":${big},"before":null}}}`,
    preview: {group: big.toString(), dx: 310, width: 300, height: 400}};
  assert.deepEqual(commandDrag(moved, pending), {object: {record: null, id: `01${hex}`}, phase: 'end', position: [310, 0], size: [300, 400]});
  assert.equal(commandDrag(moved, {...pending, preview: {...pending.preview, dx: 0.5}}), null);
  assert.equal(commandDrag(null, pending).position[0], 310);
});

test('selection projector reports only changes', () => {
  const projector = createSelectionProjector();
  const identity = publisher => ({label: `L${publisher}`, color: '#10b981'});
  const remotes = [{publisher: 'a', selection: {anchor: '01', head: '02'}}, {publisher: 'b', selection: null}];
  assert.deepEqual(projector.rows(remotes, identity), [{publisher: 'a', name: 'La', color: '#10b981', selection: {anchor: '01', head: '02'}}]);
  assert.equal(projector.rows(remotes, identity), null);
  assert.deepEqual(projector.rows([{...remotes[0], stale: true}], identity), []);
  assert.equal(projector.rows([], identity), null);
});

// --- minimal DOM ---------------------------------------------------------------
function fakeDocument() {
  const writes = {count: 0};
  function element(tag) {
    const node = {
      tag, children: [], parent: null, attributes: {}, listeners: {}, dataset: {}, _text: '',
      style: new Proxy({}, {set(target, key, value) { if (target[key] !== value) writes.count++; target[key] = value; return true; }}),
      setAttribute(key, value) { this.attributes[key] = String(value); },
      append(...nodes) { for (const child of nodes) { child.parent = this; this.children.push(child); } },
      remove() { if (this.parent) this.parent.children = this.parent.children.filter(child => child !== this); this.parent = null; },
      set textContent(value) { this._text = value; writes.count++; },
      get textContent() { return this._text; },
      addEventListener(type, callback, options) {
        (this.listeners[type] ??= []).push(callback);
        options?.signal?.addEventListener('abort', () => { this.listeners[type] = this.listeners[type].filter(c => c !== callback); });
      },
      dispatch(type, event) { for (const callback of this.listeners[type] ?? []) callback(event); },
      getBoundingClientRect() { return {left: 10, top: 20, width: 1200, height: 800}; },
    };
    return node;
  }
  return {writes, createElement: element, createElementNS: (_, tag) => element(tag)};
}

test('overlay view moves cursors by transform only and writes nothing for an unchanged frame', () => {
  const doc = fakeDocument();
  const root = doc.createElement('div');
  const view = createOverlayView(root, doc);
  const cursor = {id: 'p1', x: 10, y: 20, color: '#ec4899', label: 'Ada'};
  view.render({cursors: [cursor], ghosts: [], members: new Set(['p1'])});
  const node = view.layer.children[0];
  assert.equal(node.style.transform, 'translate3d(10.0px,20.0px,0)');
  assert.equal(node.children[1].textContent, 'Ada');
  const before = doc.writes.count;
  view.render({cursors: [cursor], ghosts: [], members: new Set(['p1'])});
  assert.equal(doc.writes.count, before, 'unchanged frame performs no DOM writes');
  view.render({cursors: [{...cursor, x: 15}], ghosts: [], members: new Set(['p1'])});
  assert.equal(node.style.transform, 'translate3d(15.0px,20.0px,0)');
  assert.equal(doc.writes.count, before + 1, 'a move is exactly one transform write');
  view.render({cursors: [], ghosts: [{id: 'g', rect: [1, 2, 30, 40], color: '#10b981', label: 'Ada', released: true}], members: new Set(['p1'])});
  assert.equal(node.style.display, 'none');
  assert.equal(view.cursorCount, 0);
  assert.equal(view.ghostCount, 1);
  assert.equal(view.layer.children[1].style.borderStyle, 'dashed');
  view.render({cursors: [], ghosts: [], members: new Set()});
  assert.equal(view.layer.children.length, 0, 'departed members and ghosts are removed');
  assert.equal(view.layer.attributes['aria-hidden'], 'true');
  assert.match(view.layer.style.cssText, /pointer-events:none/);
});

test('typing tracker keeps in-flight text until the typist\'s signed change lands or it expires', () => {
  const typing = createTypingTracker();
  const insert = (text, at) => typing.preview({publisher: 'p1', record: WRITER, ops: [{kind: 'insert', at: '01', text}]}, at);
  insert('he', 0);
  insert('l', 10);
  typing.preview({publisher: 'p1', record: WRITER, ops: [{kind: 'delete', at: '01', len: 1}]}, 11);
  insert('lo', 20);
  assert.equal(typing.text('p1', WRITER, 30), 'helo');
  assert.equal(typing.text('p1', CHAT, 30), null, 'per record');
  assert.equal(typing.text('p2', WRITER, 30), null, 'per publisher');
  // The forwarded change arrives 150 ms after the first keystroke; a chunk
  // typed 20 ms before it is still in flight.
  insert('!', 130);
  typing.landed({publisher: 'p1', record: WRITER}, 150);
  assert.equal(typing.text('p1', WRITER, 151), '!');
  assert.equal(typing.text('p1', WRITER, 130 + TYPING_EXPIRE_MS + 1), null, 'expired');
  assert.equal(typing.size, 0);
});

function fakeLive() {
  const listeners = {presence: new Set(), member: new Set(), state: new Set(), durable: new Set(), preview: new Set(), signedChange: new Set()};
  const on = set => callback => { set.add(callback); return () => set.delete(callback); };
  const live = {
    available: true, published: [], remotes: [],
    publishPresence(partial) { this.published.push(structuredClone(partial)); return true; },
    remoteState() { return this.remotes; },
    onRemotePresence: on(listeners.presence), onMember: on(listeners.member), onState: on(listeners.state), onDurable: on(listeners.durable),
    onPreview: on(listeners.preview), onSignedChange: on(listeners.signedChange),
    previews: [],
    publishPreview(record, ops) { this.previews.push({record, ops}); return true; },
    emit(kind, value) { for (const callback of listeners[kind]) callback(value); },
    listeners,
  };
  return live;
}

test('overlay glue publishes focus-relative pointers and anchors, draws remotes and hands selections to the runtime', () => {
  const doc = fakeDocument();
  const surface = doc.createElement('div');
  const canvas = doc.createElement('canvas');
  const live = fakeLive();
  const frames = [];
  const applied = [];
  let frameValue = 1;
  let clockNow = 1000;
  let snapshots = 0;
  const overlay = createLiveOverlay({
    surface, canvas, document: doc, live: () => live,
    snapshot: () => { snapshots++; return JSON.stringify({...snapshotA, selection: {anchor: '0103', head: '0205'}}); },
    applyRemote: rows => { applied.push(rows); return true; },
    members: () => [{device: 'p1', user: 'alice', name: 'Ada'}],
    presented: () => frameValue,
    now: () => clockNow,
    requestFrame: callback => { frames.push(callback); return {n: frames.length}; },
    cancelFrame: () => {},
    setInterval: () => 1, clearInterval: () => {},
  });
  // One animation frame per call (the loop re-arms itself while remotes move).
  const run = () => { for (const callback of frames.splice(0)) callback(1000); };
  run();
  const before = live.published.length;
  surface.dispatch('pointermove', {clientX: 410, clientY: 270});
  assert.equal(live.published.length, before + 1, 'the pointer is published from the event, not the next frame');
  assert.deepEqual(live.published.at(-1), {focus: WRITER, pointer: [300, 200]});
  run();
  const last = live.published.at(-1);
  assert.deepEqual(last.focus, WRITER);
  assert.deepEqual(last.pointer, [300, 200], 'canvas-relative, then focused-window-relative');
  assert.deepEqual(last.selection, {anchor: '0103', head: '0205'});
  assert.equal(last.drag, undefined, 'no drag field without a drag');
  // A remote Writer cursor and caret.
  live.remotes = [{publisher: 'p1', focus: WRITER, pointer: [5, 6], selection: {anchor: '0101', head: '0201'}, drag: null, endedDrags: [], stale: false, receivedAtMs: 990}];
  live.emit('presence', {});
  run();
  const cursor = overlay.view.layer.children[0];
  assert.equal(cursor.style.transform, 'translate3d(105.0px,56.0px,0)');
  assert.equal(cursor.children[1].textContent, 'Ada');
  assert.equal(cursor.children[0].children[0].attributes.fill, '#ec4899', 'coloured by the member user id');
  assert.deepEqual(applied.at(-1), [{publisher: 'p1', name: 'Ada', color: '#ec4899', selection: {anchor: '0101', head: '0201'}}]);
  // Local typing leaves at once as a preview at the caret anchor.
  surface.dispatch('keydown', {key: 'h', target: {tagName: 'CANVAS'}});
  surface.dispatch('keydown', {key: 'Shift', target: {tagName: 'CANVAS'}});
  surface.dispatch('keydown', {key: 'k', ctrlKey: true, target: {tagName: 'CANVAS'}});
  assert.deepEqual(live.previews, [{record: WRITER, ops: [{kind: 'insert', text: 'h', at: '0103'}]}]);
  // The IME textarea path: keys at keydown, composed text once at compositionend.
  surface.dispatch('keydown', {key: 'i', target: {tagName: 'TEXTAREA'}});
  surface.dispatch('keydown', {key: 'e', isComposing: true, target: {tagName: 'TEXTAREA'}});
  surface.dispatch('compositionend', {data: '\u00e9'});
  surface.dispatch('keydown', {key: 'Backspace', target: {tagName: 'TEXTAREA'}});
  assert.deepEqual(live.previews.slice(1).map(p => p.ops[0]), [
    {kind: 'insert', text: 'i', at: '0103'}, {kind: 'insert', text: '\u00e9', at: '0103'}, {kind: 'delete', len: 1, at: '0103'},
  ]);
  // A remote preview is shown with that member's caret until its change lands.
  live.emit('preview', {publisher: 'p1', record: WRITER, seq: 1, ops: [{kind: 'insert', at: '0101', text: 'yo'}]});
  run();
  assert.equal(applied.at(-1)[0].typing, 'yo');
  clockNow = 1150;
  live.emit('signedChange', {publisher: 'p1', record: WRITER, digest: 'd', result: 'admitted'});
  run();
  clockNow = 1000;
  assert.equal(applied.at(-1)[0].typing, undefined, 'the landed change replaces the preview');
  const appliedCount = applied.length;
  const snapshotCount = snapshots;
  assert.equal(frames.length, 1, 'a moving remote keeps the frame loop armed');
  // Remote present: the loop keeps animating; unchanged selection is not re-applied,
  // and the snapshot is re-read only when the runtime presents a new frame.
  live.emit('presence', {});
  run();
  assert.equal(applied.length, appliedCount);
  assert.equal(snapshots, snapshotCount);
  frameValue = 2;
  live.emit('presence', {});
  run();
  assert.equal(snapshots, snapshotCount + 1);
  // A remote whose last datagram is old has settled: the loop sleeps until woken.
  live.remotes = [{...live.remotes[0], receivedAtMs: 100}];
  run();
  assert.equal(frames.length, 0, 'settled remotes do not keep the loop running');
  overlay.wake();
  assert.equal(frames.length, 1, 'a presented runtime frame re-maps visible remote cursors');
  run();
  // Remote leaves: cursor removed, carets cleared.
  live.remotes = [];
  live.emit('presence', {});
  run();
  assert.equal(frames.length, 0, 'the loop sleeps once nothing moves');
  assert.equal(overlay.view.layer.children.length, 0);
  assert.deepEqual(applied.at(-1), []);
  const stats = overlay.stats();
  assert.ok(stats.frames > 0 && stats.attached);
  overlay.close();
  assert.deepEqual(live.published.at(-1), {pointer: null, selection: null, drag: null});
  assert.equal(surface.children.length, 0);
  assert.equal(live.listeners.presence.size, 0);
});

test('overlay stays inert without a live client and attaches when it appears', () => {
  const doc = fakeDocument();
  const surface = doc.createElement('div');
  let live;
  let tick;
  const frames = [];
  const overlay = createLiveOverlay({
    surface, canvas: doc.createElement('canvas'), document: doc, live: () => live, snapshot: () => null,
    requestFrame: callback => { frames.push(callback); return {}; }, cancelFrame: () => {},
    setInterval: callback => { tick = callback; return 1; }, clearInterval: () => {},
  });
  surface.dispatch('pointermove', {clientX: 1, clientY: 1});
  for (const callback of frames.splice(0)) callback(0);
  assert.equal(overlay.stats().attached, false);
  live = fakeLive();
  tick();
  for (const callback of frames.splice(0)) callback(0);
  assert.equal(overlay.stats().attached, true);
  assert.deepEqual(live.published.at(-1), {focus: null, pointer: [-9, -19], selection: null});
  overlay.close();
});

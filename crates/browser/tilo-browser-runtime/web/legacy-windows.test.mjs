import test from 'node:test';
import assert from 'node:assert/strict';
import {
  CHUNK_FIRST, CHUNK_KEY, CHUNK_LAST, INDEX_RECORD, LEGACY_APP,
  createLegacyWindows, createReassembler, createStreamDecoder, decodeFrame, evdevKey, h264CodecString,
  wheelPixels, windowPoint, windowTitle,
} from './legacy-windows.mjs';

const unhex = value => Uint8Array.from(value.match(/../g), byte => parseInt(byte, 16));
const utf8 = new TextEncoder();
const STREAM = {surface: '0000000000000007', app: 'org.chromium.Chromium', codec: 'h264', width: 1280, height: 720, record: '@tilo:legacy-window:0000000000000007'};
const SHELL = 'ab'.repeat(32);
const KEY = `${SHELL}:${STREAM.surface}`;
const json = (kind, value) => Uint8Array.from([0xf1, kind, ...utf8.encode(JSON.stringify(value))]);
function chunk(flags, seq, data, total = data.length) {
  const out = new Uint8Array(19 + data.length);
  const view = new DataView(out.buffer);
  out.set([0xf1, 3, flags]);
  view.setUint32(3, seq);
  view.setBigUint64(7, 99n);
  view.setUint32(15, total);
  out.set(data, 19);
  return out;
}
const SPS_IDR = [0, 0, 0, 1, 0x67, 0x64, 0x00, 0x28, 0xac, 0, 0, 1, 0x65, 0x88];
const tick = () => new Promise(resolve => setTimeout(resolve, 0));

test('frames decode exactly as the host encodes them', () => {
  // `legacy_window::Frame::Chunk` for these fields, byte for byte.
  assert.deepEqual(decodeFrame(unhex('f10307000000090000000000000063' + '00000003000001')), {
    kind: 'chunk', flags: 7, seq: 9, tsUs: 99, total: 3, data: Uint8Array.from([0, 0, 1]),
  });
  assert.deepEqual(decodeFrame(json(1, [STREAM])), {kind: 'index', streams: [STREAM]});
  assert.deepEqual(decodeFrame(json(2, STREAM)), {kind: 'start', stream: STREAM});
  assert.deepEqual(decodeFrame(Uint8Array.from([0xf1, 4])), {kind: 'end'});
  for (const bad of [[], [0xf1], [0x01, 1], [0xf1, 9], [0xf1, 4, 0], [0xf1, 3, 7, 0, 0]]) assert.equal(decodeFrame(Uint8Array.from(bad)), null);
  assert.equal(decodeFrame(json(1, [{...STREAM, width: -1}])), null, 'a malformed stream spoils the index');
  assert.equal(decodeFrame(json(2, {...STREAM, surface: '7'})), null);
});

test('chunks reassemble in order and a gap drops the frame', () => {
  const r = createReassembler();
  assert.equal(r.push(decodeFrame(chunk(CHUNK_KEY | CHUNK_FIRST, 1, [1, 2], 5))), null);
  assert.equal(r.push(decodeFrame(chunk(CHUNK_KEY, 1, [3, 4], 5))), null);
  assert.deepEqual(r.push(decodeFrame(chunk(CHUNK_KEY | CHUNK_LAST, 1, [5], 5))), {key: true, seq: 1, tsUs: 99, data: Uint8Array.from([1, 2, 3, 4, 5])});
  assert.equal(r.push(decodeFrame(chunk(CHUNK_FIRST, 2, [1], 3))), null);
  assert.equal(r.push(decodeFrame(chunk(CHUNK_LAST, 3, [3], 3))), null, 'a chunk of another frame');
  assert.equal(r.push(decodeFrame(chunk(CHUNK_LAST, 2, [3], 3))), null, 'nothing pending any more');
  assert.equal(r.push(decodeFrame(chunk(CHUNK_FIRST | CHUNK_LAST, 4, [1, 2], 3))), null, 'short of its total');
  assert.deepEqual(r.push(decodeFrame(chunk(CHUNK_FIRST | CHUNK_LAST, 5, [7]))).data, Uint8Array.from([7]));
});

test('the H.264 codec string comes from the sequence parameter set', () => {
  assert.equal(h264CodecString(Uint8Array.from(SPS_IDR)), 'avc1.640028');
  assert.equal(h264CodecString(Uint8Array.from([0, 0, 1, 0x65, 0x88])), 'avc1.42E01F');
});

function fakeCodecs() {
  const decoders = [];
  class VideoDecoder {
    constructor({output, error}) { this.output = output; this.error = error; this.state = 'unconfigured'; this.decodeQueueSize = 0; this.chunks = []; decoders.push(this); }
    configure(config) { this.config = config; this.state = 'configured'; }
    decode(chunk) { this.chunks.push(chunk); }
    close() { this.state = 'closed'; }
  }
  class EncodedVideoChunk { constructor(init) { Object.assign(this, init); } }
  return {VideoDecoder, EncodedVideoChunk, decoders};
}

test('decoding starts at a keyframe and skips a backlog until the next one', () => {
  const codecs = fakeCodecs();
  const decoder = createStreamDecoder({...codecs, onFrame: () => {}});
  const delta = {key: false, seq: 1, tsUs: 1, data: Uint8Array.from([0, 0, 1, 0x41])};
  const key = {key: true, seq: 2, tsUs: 2, data: Uint8Array.from(SPS_IDR)};
  assert.equal(decoder.decode(delta), false);
  assert.equal(codecs.decoders.length, 0, 'nothing to configure before a keyframe');
  assert.equal(decoder.decode(key), true);
  assert.equal(codecs.decoders[0].config.codec, 'avc1.640028');
  assert.equal(decoder.decode(delta), true);
  assert.deepEqual(codecs.decoders[0].chunks.map(chunk => chunk.type), ['key', 'delta']);
  codecs.decoders[0].decodeQueueSize = 100;
  assert.equal(decoder.decode(delta), false, 'a backlog waits for the next keyframe');
  codecs.decoders[0].decodeQueueSize = 0;
  assert.equal(decoder.decode(delta), false);
  assert.equal(decoder.decode(key), true);
  assert.equal(codecs.decoders.length, 1, 'the same stream keeps its decoder');
  assert.deepEqual(decoder.stats, {decoded: 0, skipped: 3, errors: 0});
});

// --- minimal DOM and live layer --------------------------------------------------
function fakeDocument() {
  function element(tag) {
    return {
      tag, children: [], parent: null, dataset: {}, style: {}, width: 0, height: 0, drawn: 0,
      listeners: {},
      append(...nodes) { for (const child of nodes) { child.parent = this; this.children.push(child); } },
      addEventListener(type, handler) { (this.listeners[type] ??= []).push(handler); },
      dispatch(type, event = {}) { for (const handler of this.listeners[type] ?? []) handler({preventDefault() {}, ...event}); },
      focus() {},
      getBoundingClientRect() { return {left: 10, top: 36, width: 400, height: 264}; },
      remove() { if (this.parent) this.parent.children = this.parent.children.filter(child => child !== this); this.parent = null; },
      getContext() { const node = this; return {drawImage() { node.drawn++; }}; },
    };
  }
  return {createElement: element};
}
function fakeLive() {
  const watches = [];
  const members = new Set();
  return {
    watches,
    members,
    watchRecordFrames(record, onFrame) { const entry = {record, onFrame, active: true}; watches.push(entry); return () => { entry.active = false; }; },
    subscribeMemberRecord(publisher, record, onFrame) { const entry = {publisher, record, onFrame: (_, bytes) => onFrame(bytes), active: true}; watches.push(entry); return () => { entry.active = false; }; },
    onMember(listener) { members.add(listener); return () => members.delete(listener); },
    leave(publisher) { for (const listener of members) listener({publisher, active: false}); },
    deliver(record, publisher, bytes) {
      for (const entry of watches) if (entry.active && entry.record.record === record && (!entry.publisher || entry.publisher === publisher)) entry.onFrame(publisher, bytes);
    },
  };
}
function mount({control = false, refuse = false} = {}) {
  const doc = fakeDocument();
  const surface = doc.createElement('div');
  const live = fakeLive();
  const codecs = fakeCodecs();
  const frames = [];
  const shown = [];
  const sent = [];
  const clock = {t: 0};
  let rects = [];
  let poll;
  const layer = createLegacyWindows({
    surface, document: doc, live: () => live, ...codecs, now: () => clock.t, mac: false,
    windows: () => rects,
    setWindows: list => shown.push(list),
    sendInput: async (publisher, stream, events) => {
      if (events.length) sent.push({publisher, stream, events});
      return events.length ? control && !refuse : control;
    },
    requestFrame: callback => { frames.push(callback); return frames.length; }, cancelFrame: () => {},
    setInterval: callback => { poll = callback; return 1; }, clearInterval: () => {},
  });
  return {
    layer, live, codecs, shown, sent, clock, surface,
    run: () => { for (const callback of frames.splice(0)) callback(); },
    place: next => { rects = next; },
    poll: () => poll(),
  };
}

test('a listed stream opens a desktop window, is decoded and drawn over it until the index drops it', async () => {
  const m = mount();
  assert.deepEqual(m.live.watches.map(w => w.record), [INDEX_RECORD]);
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [STREAM]));
  assert.deepEqual(m.shown.at(-1), [{key: KEY, app: STREAM.app, title: 'Chromium'}], 'the desktop opens a window for it');
  assert.deepEqual(m.live.watches[1], {...m.live.watches[1], publisher: SHELL, record: {app: LEGACY_APP, record: STREAM.record}});
  m.live.deliver(STREAM.record, 'cd'.repeat(32), chunk(CHUNK_KEY | CHUNK_FIRST | CHUNK_LAST, 0, SPS_IDR));
  assert.equal(m.codecs.decoders.length, 0, 'another namespace cannot feed the stream');
  m.live.deliver(STREAM.record, SHELL, json(2, STREAM));
  m.live.deliver(STREAM.record, SHELL, chunk(CHUNK_KEY | CHUNK_FIRST, 0, SPS_IDR.slice(0, 6), SPS_IDR.length));
  m.live.deliver(STREAM.record, SHELL, chunk(CHUNK_KEY | CHUNK_LAST, 0, SPS_IDR.slice(6), SPS_IDR.length));
  assert.equal(m.codecs.decoders[0].chunks.length, 1);
  m.codecs.decoders[0].output({displayWidth: 1280, displayHeight: 720, close() { this.closed = true; }});
  const canvas = m.layer.layer.children[0];
  assert.equal(canvas.drawn, 1);
  m.run();
  assert.notEqual(canvas.style.display, 'block', 'no window laid out yet');
  m.place([{key: KEY, rect: [10, 36, 410, 300]}]);
  m.run();
  assert.equal(canvas.style.display, 'block');
  assert.equal(canvas.style.transform, 'translate3d(10.0px,36.0px,0)');
  assert.equal(canvas.style.width, '400.0px');
  assert.equal(canvas.style.height, '264.0px');
  assert.equal(m.layer.stats().visible, 1);
  // Search covers the desktop (or the window scrolled away).
  m.place([]);
  m.run();
  assert.equal(canvas.style.display, 'none');
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, []));
  assert.equal(m.layer.stats().streams, 0);
  assert.deepEqual(m.shown.at(-1), [], 'its window closes');
  assert.equal(m.live.watches[1].active, false, 'an unlisted stream is unsubscribed');
  assert.equal(m.codecs.decoders[0].state, 'closed');
  assert.equal(m.layer.layer.children.length, 0);
  // An index that stops arriving expires its streams; a member that leaves takes them along.
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [STREAM]));
  m.clock.t += 4000;
  m.poll();
  assert.equal(m.layer.stats().streams, 1, 'still within the index lifetime');
  m.clock.t += 1001;
  m.poll();
  assert.equal(m.layer.stats().streams, 0);
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [STREAM]));
  m.live.leave(SHELL);
  assert.equal(m.layer.stats().streams, 0);
  m.layer.close();
  assert.equal(m.surface.children.length, 0);
  await tick();
});

test('keys map to evdev codes, points to window pixels through the letterbox, wheels to pixels', () => {
  assert.deepEqual(['KeyA', 'KeyQ', 'Digit1', 'Digit0', 'Enter', 'F5', 'ArrowLeft', 'Numpad7'].map(code => evdevKey(code)), [30, 16, 2, 11, 28, 63, 105, null]);
  assert.equal(evdevKey('MetaLeft'), 125);
  assert.equal(evdevKey('MetaLeft', {mac: true}), 29, 'Command is Control for the window');
  const box = {left: 10, top: 20, width: 400, height: 300};
  assert.deepEqual(windowPoint(box, 1280, 720, 10, 57.5), [0, 0]);
  assert.deepEqual(windowPoint(box, 1280, 720, 210, 170), [640, 360]);
  assert.equal(windowPoint(box, 1280, 720, 210, 30), null, 'on the bar');
  assert.deepEqual(wheelPixels({deltaMode: 1, deltaX: 0, deltaY: 3}, 600), [0, 120]);
  assert.deepEqual(wheelPixels({deltaMode: 0, deltaX: 5, deltaY: -2}, 600), [5, -2]);
  assert.equal(windowTitle('org.libreoffice.writer'), 'Writer');
});

async function controlled(options) {
  const m = mount(options);
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [STREAM]));
  await tick();
  m.place([{key: KEY, rect: [10, 36, 410, 300]}]);
  m.run();
  return {...m, canvas: m.layer.layer.children[0]};
}

test('a member the host lets drive the window clicks, scrolls, types and pastes into it', async () => {
  const m = await controlled({control: true});
  assert.equal(m.canvas.style.pointerEvents, 'auto');
  assert.equal(m.layer.streams()[0].control, true);
  m.canvas.dispatch('pointerdown', {button: 0, clientX: 210, clientY: 168});
  m.canvas.dispatch('pointerup', {button: 0});
  m.canvas.dispatch('wheel', {deltaMode: 1, deltaX: 0, deltaY: 1});
  m.canvas.dispatch('wheel', {deltaMode: 1, deltaX: 0, deltaY: 1});
  m.canvas.dispatch('keydown', {code: 'KeyA'});
  m.canvas.dispatch('keydown', {code: 'ControlLeft', ctrlKey: true});
  m.canvas.dispatch('keydown', {code: 'KeyV', ctrlKey: true});
  m.canvas.dispatch('paste', {clipboardData: {getData: type => (type === 'text/plain' ? 'hello' : '')}});
  m.canvas.dispatch('keyup', {code: 'KeyV', ctrlKey: true});
  await tick();
  await tick();
  assert.deepEqual(new Set(m.sent.map(request => `${request.publisher}:${request.stream}`)), new Set([KEY]));
  const events = m.sent.flatMap(request => request.events);
  assert.deepEqual(events[0], {KeyboardFocus: {surface: 0, focused: true}});
  assert.ok('PointerMotion' in events[1]);
  assert.deepEqual(events.slice(2), [
    {PointerButton: {button: 272, pressed: true}},
    {PointerButton: {button: 272, pressed: false}},
    {PointerAxis: {dx: 0, dy: 80}},
    {Key: {keysym: 30, pressed: true, modifiers: 0}},
    {Key: {keysym: 29, pressed: true, modifiers: 0}},
    {ClipboardText: {text: 'hello'}},
    {Key: {keysym: 47, pressed: true, modifiers: 0}},
    {Key: {keysym: 47, pressed: false, modifiers: 0}},
  ], 'the pasted text reaches the window before its paste key');
  assert.equal(m.layer.stats().inputSent, events.length);
  m.layer.close();
});

test('a watching member sends nothing, and a refusal stops input', async () => {
  const watcher = await controlled({control: false});
  assert.equal(watcher.canvas.style.pointerEvents, 'none');
  watcher.canvas.dispatch('pointerdown', {button: 0, clientX: 210, clientY: 168});
  await tick();
  assert.equal(watcher.sent.length, 0);
  watcher.layer.close();
  const refused = await controlled({control: true, refuse: true});
  refused.canvas.dispatch('pointerdown', {button: 0, clientX: 210, clientY: 168});
  await tick();
  assert.equal(refused.layer.stats().inputRefused, 1, 'the first refused request ends input; the rest is dropped');
  assert.equal(refused.sent.length, 1);
  refused.run();
  assert.equal(refused.canvas.style.pointerEvents, 'none', 'the window no longer takes input');
  refused.layer.close();
});


test('keyframes cannot grow a decoder backlog without bound', () => {
  const codecs = fakeCodecs();
  const decoder = createStreamDecoder({...codecs, onFrame: () => {}});
  const frame = {key: true, seq: 2, tsUs: 2, data: Uint8Array.from(SPS_IDR)};
  assert.equal(decoder.decode(frame), true);
  codecs.decoders[0].decodeQueueSize = 100;
  assert.equal(decoder.decode(frame), false);
  assert.equal(codecs.decoders[0].chunks.length, 1);
  decoder.close();
});

test('chunks with inconsistent timestamps, sizes or key flags drop the access unit', () => {
  for (const changed of [{tsUs: 100}, {total: 3}, {flags: CHUNK_LAST | CHUNK_KEY}]) {
    const r = createReassembler();
    r.push(decodeFrame(chunk(CHUNK_FIRST, 1, [1], 2)));
    assert.equal(r.push({...decodeFrame(chunk(CHUNK_LAST, 1, [2], 2)), ...changed}), null);
  }
});

test('an index cannot redirect a surface to an arbitrary record or codec', () => {
  assert.equal(decodeFrame(json(1, [{...STREAM, record: '@tilo:private-document'}])), null);
  assert.equal(decodeFrame(json(1, [{...STREAM, codec: 'vp9'}])), null);
});

test('closing while input is in flight drops unsent events', async () => {
  const m = await controlled({control: true});
  let complete;
  // Use an actual pending send rather than delaying the whole test environment.
  const doc = fakeDocument();
  const live = fakeLive();
  const sent = [];
  const layer = createLegacyWindows({surface: doc.createElement('div'), document: doc, live: () => live,
    ...fakeCodecs(), windows: () => [], setWindows() {},
    sendInput: async (_, __, events) => {
      if (!events.length) return true;
      sent.push(events);
      return await new Promise(resolve => { complete = resolve; });
    }, requestFrame: () => 1, cancelFrame() {}, setInterval: () => 1, clearInterval() {}});
  live.deliver(INDEX_RECORD.record, SHELL, json(1, [STREAM]));
  await tick();
  const canvas = layer.layer.children[0];
  canvas.dispatch('keydown', {code: 'KeyA'});
  canvas.dispatch('keydown', {code: 'KeyB'});
  layer.close();
  complete(true);
  await tick();
  assert.equal(sent.length, 1, 'no queued key is sent after close');
  m.layer.close();
});


test('blur releases held buttons and keys before relinquishing keyboard focus', async () => {
  const m = await controlled({control: true});
  m.canvas.dispatch('pointerdown', {button: 0, pointerId: 5, clientX: 210, clientY: 168});
  m.canvas.dispatch('keydown', {code: 'ShiftLeft'});
  await tick();
  await tick();
  m.canvas.dispatch('blur');
  await tick();
  await tick();
  const events = m.sent.flatMap(request => request.events);
  assert.ok(events.some(event => event.Key?.keysym === 42 && event.Key.pressed === false));
  assert.ok(events.some(event => event.PointerButton?.button === 272 && event.PointerButton.pressed === false));
  assert.deepEqual(events.at(-1), {KeyboardFocus: {surface: 0, focused: false}});
  m.layer.close();
});

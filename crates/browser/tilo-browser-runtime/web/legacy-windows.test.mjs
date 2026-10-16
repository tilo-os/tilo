import test from 'node:test';
import assert from 'node:assert/strict';
import {
  CHUNK_FIRST, CHUNK_KEY, CHUNK_LAST, INDEX_RECORD, LEGACY_APP, MAX_TEXT_BYTES, VIDEO_DECODER_CONFIGS,
  createLegacyWindows, createReassembler, createStreamDecoder, decodableCodecs, decodeFrame, evdevKey, h264CodecString,
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
  const doc = {createElement: element, activeElement: null};
  function element(tag) {
    return {
      tag, children: [], parent: null, dataset: {}, style: {}, width: 0, height: 0, drawn: 0,
      listeners: {},
      append(...nodes) { for (const child of nodes) { child.parent = this; this.children.push(child); } },
      addEventListener(type, handler) { (this.listeners[type] ??= []).push(handler); },
      dispatch(type, event = {}) { for (const handler of this.listeners[type] ?? []) handler({preventDefault() {}, ...event}); },
      focus() { doc.activeElement = this; },
      getBoundingClientRect() { return {left: 10, top: 36, width: 400, height: 264}; },
      remove() { if (this.parent) this.parent.children = this.parent.children.filter(child => child !== this); this.parent = null; },
      getContext() { const node = this; return {drawImage() { node.drawn++; }}; },
    };
  }
  return doc;
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
function mount({control = false, refuse = false, dismissed = () => [], paintFrame = () => false, takePaintInput = () => [], focusWindow, focusDesktop, sendInput, onError} = {}) {
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
    dismissed, focusWindow, focusDesktop, onError,
    inputPause: async ms => { clock.t += ms; },
    paintFrame,
    takePaintInput,
    sendInput: sendInput ?? (async (publisher, stream, events) => {
      if (events.length) sent.push({publisher, stream, events});
      return events.length ? control && !refuse : control;
    }),
    requestFrame: callback => { frames.push(callback); return frames.length; }, cancelFrame: () => {},
    setInterval: callback => { poll = callback; return 1; }, clearInterval: () => {},
  });
  return {
    layer, live, codecs, shown, sent, clock, surface, doc,
    run: () => { for (const callback of frames.splice(0)) callback(); },
    place: next => { rects = next; },
    poll: () => poll(),
  };
}

test('a listed stream opens a desktop window, is decoded and drawn over it until the index drops it', async () => {
  const m = mount();
  assert.deepEqual(m.live.watches.map(w => w.record), [INDEX_RECORD]);
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [STREAM]));
  assert.deepEqual(m.shown.at(-1), [{key: KEY, app: STREAM.app, title: 'Chromium', codec: 'h264'}], 'the desktop opens a window for it');
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

/** The hidden text area a window's keyboard (and input method) lives in. */
const keyboardOf = (layer, index = 0) => layer.children.filter(child => child.tag === 'textarea')[index];
async function controlled(options) {
  const m = mount(options);
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [STREAM]));
  await tick();
  m.place([{key: KEY, rect: [10, 36, 410, 300]}]);
  m.run();
  return {...m, canvas: m.layer.layer.children[0], keyboard: keyboardOf(m.layer.layer)};
}

test('a member the host lets drive the window clicks, scrolls, types and pastes into it', async () => {
  const m = await controlled({control: true});
  assert.equal(m.canvas.style.pointerEvents, 'auto');
  assert.equal(m.layer.streams()[0].control, true);
  m.canvas.dispatch('pointerdown', {button: 0, clientX: 210, clientY: 168});
  m.canvas.dispatch('pointerup', {button: 0});
  m.canvas.dispatch('wheel', {deltaMode: 1, deltaX: 0, deltaY: 1});
  m.canvas.dispatch('wheel', {deltaMode: 1, deltaX: 0, deltaY: 1});
  m.keyboard.dispatch('keydown', {code: 'KeyA'});
  m.keyboard.dispatch('keydown', {code: 'ControlLeft', ctrlKey: true});
  m.keyboard.dispatch('keydown', {code: 'KeyV', ctrlKey: true});
  m.keyboard.dispatch('paste', {clipboardData: {getData: type => (type === 'text/plain' ? 'hello' : '')}});
  m.keyboard.dispatch('keyup', {code: 'KeyV', ctrlKey: true});
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

test('typing reaches the window as every stroke in order, and a held key repeats as the browser repeats it', async () => {
  const m = await controlled({control: true});
  m.canvas.dispatch('pointerdown', {button: 0, clientX: 210, clientY: 168});
  m.canvas.dispatch('pointerup', {button: 0});
  const SHIFTED = {'!': 'Digit1', ':': 'Semicolon', '?': 'Slash'};
  const code = c => SHIFTED[c] ?? ({' ': 'Space', '.': 'Period', ',': 'Comma', '/': 'Slash', ';': 'Semicolon'}[c]
    ?? (/[0-9]/.test(c) ? `Digit${c}` : `Key${c.toUpperCase()}`));
  const text = 'Hello, World! example.com';
  for (const c of text) {
    const shift = c in SHIFTED || /[A-Z]/.test(c);
    if (shift) m.keyboard.dispatch('keydown', {code: 'ShiftLeft', shiftKey: true});
    m.keyboard.dispatch('keydown', {code: code(c), shiftKey: shift});
    m.keyboard.dispatch('keyup', {code: code(c), shiftKey: shift});
    if (shift) m.keyboard.dispatch('keyup', {code: 'ShiftLeft'});
  }
  m.keyboard.dispatch('keydown', {code: 'ShiftLeft', shiftKey: true});
  m.keyboard.dispatch('keydown', {code: 'ShiftLeft', shiftKey: true, repeat: true});
  m.keyboard.dispatch('keyup', {code: 'ShiftLeft'});
  m.keyboard.dispatch('keydown', {code: 'KeyO'});
  m.keyboard.dispatch('keydown', {code: 'KeyO', repeat: true});
  m.keyboard.dispatch('keydown', {code: 'KeyO', repeat: true});
  m.keyboard.dispatch('keyup', {code: 'KeyO'});
  m.keyboard.dispatch('keydown', {code: 'KeyP', repeat: true});
  for (let i = 0; i < 8; i++) await tick();
  const keys = m.sent.flatMap(request => request.events).filter(event => 'Key' in event).map(({Key}) => [Key.keysym, Key.pressed]);
  const strokes = [];
  for (const c of text) {
    const shift = c in SHIFTED || /[A-Z]/.test(c);
    const keysym = evdevKey(code(c));
    if (shift) strokes.push([42, true]);
    strokes.push([keysym, true], [keysym, false]);
    if (shift) strokes.push([42, false]);
  }
  assert.deepEqual(keys, [...strokes, [42, true], [42, false], [24, true], [24, true], [24, true], [24, false]],
    'every press and release in order; a held letter repeats, a held modifier and a key not held do not');
  m.layer.close();
});

test('character modifier snapshots reach the window as shift edges even without a shift key event', async () => {
  const m = await controlled({control: true});
  m.place([{key: KEY, rect: [10, 36, 410, 300], focused: true}]);
  m.run();
  for (const [code, shiftKey] of [['KeyM', true], ['KeyI', false], ['Slash', true], ['Digit7', true], ['KeyA', false]]) {
    m.keyboard.dispatch('keydown', {code, shiftKey, ctrlKey: false, altKey: false, metaKey: false});
    m.keyboard.dispatch('keyup', {code, shiftKey, ctrlKey: false, altKey: false, metaKey: false});
  }
  for (let i = 0; i < 8; i++) await tick();
  const keys = m.sent.flatMap(request => request.events).filter(event => 'Key' in event).map(({Key}) => [Key.keysym, Key.pressed]);
  assert.deepEqual(keys, [
    [42, true], [50, true], [50, false], [42, false], [23, true], [23, false],
    [42, true], [53, true], [53, false], [8, true], [8, false],
    [42, false], [30, true], [30, false],
  ], 'Mi?&a retains capitals and shifted punctuation without duplicate modifier presses');
  m.layer.close();
});

test('a shortcut held before focus is restored and its modifier is released on blur', async () => {
  const m = await controlled({control: true});
  m.place([{key: KEY, rect: [10, 36, 410, 300], focused: true}]);
  m.run();
  m.keyboard.dispatch('keydown', {code: 'KeyL', ctrlKey: true, shiftKey: false, altKey: false, metaKey: false});
  m.keyboard.dispatch('keyup', {code: 'KeyL', ctrlKey: true, shiftKey: false, altKey: false, metaKey: false});
  m.keyboard.dispatch('blur');
  for (let i = 0; i < 8; i++) await tick();
  const keys = m.sent.flatMap(request => request.events).filter(event => 'Key' in event).map(({Key}) => [Key.keysym, Key.pressed]);
  assert.deepEqual(keys, [[29, true], [38, true], [38, false], [29, false]]);
  m.layer.close();
});

test('the window the desktop focuses takes the keyboard, and a click focuses its desktop window', async () => {
  const focused = [];
  let desktop = 0;
  const m = await controlled({control: true, focusWindow: key => focused.push(key), focusDesktop: () => { desktop++; m.doc.activeElement = m.surface; }});
  assert.notEqual(m.doc.activeElement, m.keyboard, 'an unfocused window leaves the keyboard alone');
  m.place([{key: KEY, rect: [10, 36, 410, 300], focused: true}]);
  m.run();
  assert.equal(m.doc.activeElement, m.keyboard, 'opening it, or choosing its tab, focuses it');
  await tick();
  assert.deepEqual(m.sent.flatMap(request => request.events)[0], {KeyboardFocus: {surface: 0, focused: true}});
  m.place([{key: KEY, rect: [10, 36, 410, 300], focused: false}]);
  m.run();
  assert.equal(desktop, 1, 'the desktop focusing another window takes the keyboard back');
  m.canvas.dispatch('pointerdown', {button: 0, clientX: 210, clientY: 168});
  assert.equal(m.doc.activeElement, m.keyboard);
  assert.deepEqual(focused, [KEY], 'a click into the window focuses its desktop window');
  m.layer.close();
  const watcher = await controlled({control: false});
  watcher.place([{key: KEY, rect: [10, 36, 410, 300], focused: true}]);
  watcher.run();
  assert.notEqual(watcher.doc.activeElement, watcher.keyboard, 'a watcher keeps typing on the desktop');
  watcher.layer.close();
});

test('an input method\'s composition reaches the focused window once as text, never as the keys that composed it', async () => {
  const m = await controlled({control: true});
  m.canvas.dispatch('pointerdown', {button: 0, clientX: 210, clientY: 168});
  m.canvas.dispatch('pointerup', {button: 0});
  await tick();
  const before = m.sent.flatMap(request => request.events).length;
  // Chromium's order for typing にほん and committing 日本 with Enter.
  m.keyboard.dispatch('keydown', {code: 'KeyN', key: 'Process', keyCode: 229});
  m.keyboard.dispatch('compositionstart', {data: ''});
  m.keyboard.dispatch('beforeinput', {inputType: 'insertCompositionText', data: 'n', isComposing: true});
  m.keyboard.value = 'n';
  m.keyboard.dispatch('input', {inputType: 'insertCompositionText', isComposing: true});
  m.keyboard.dispatch('keyup', {code: 'KeyN', key: 'n', keyCode: 78, isComposing: true});
  m.keyboard.dispatch('keydown', {code: 'KeyI', key: 'Process', keyCode: 229, isComposing: true});
  m.keyboard.dispatch('beforeinput', {inputType: 'insertCompositionText', data: 'に', isComposing: true});
  m.keyboard.dispatch('keydown', {code: 'Space', key: 'Process', keyCode: 229, isComposing: true});
  m.keyboard.dispatch('keydown', {code: 'Enter', key: 'Process', keyCode: 229, isComposing: true});
  m.keyboard.dispatch('beforeinput', {inputType: 'insertCompositionText', data: '日本', isComposing: true});
  m.keyboard.dispatch('compositionend', {data: '日本'});
  assert.equal(m.keyboard.value, '', 'the hidden target is empty after each commit');
  m.keyboard.dispatch('keyup', {code: 'Enter', key: 'Enter', keyCode: 13});
  // An emoji picker or dictation commits without a composition.
  let cancelled = 0;
  m.keyboard.dispatch('beforeinput', {inputType: 'insertText', data: '🎉', preventDefault() { cancelled++; }});
  m.keyboard.dispatch('keydown', {code: 'KeyA', key: 'a', keyCode: 65});
  m.keyboard.dispatch('keyup', {code: 'KeyA', key: 'a', keyCode: 65});
  for (let i = 0; i < 8; i++) await tick();
  assert.equal(cancelled, 1, 'the picker\'s text is not left in the hidden target');
  assert.deepEqual(m.sent.flatMap(request => request.events).slice(before), [
    {Text: {text: '日本'}},
    {Text: {text: '🎉'}},
    {Key: {keysym: 30, pressed: true, modifiers: 0}},
    {Key: {keysym: 30, pressed: false, modifiers: 0}},
  ], 'one Text per commit, no key of the composition, plain keys still keys');
  m.layer.close();
});

test('committed text is bounded like the session bounds it and goes only to the window holding the keyboard', async () => {
  const m = await controlled({control: true});
  const commit = text => {
    m.keyboard.dispatch('compositionstart', {data: ''});
    m.keyboard.dispatch('compositionend', {data: text});
  };
  commit('unfocused');
  m.keyboard.dispatch('beforeinput', {inputType: 'insertText', data: 'unfocused'});
  m.place([{key: KEY, rect: [10, 36, 410, 300], focused: true}]);
  m.run();
  commit('');
  commit('x'.repeat(MAX_TEXT_BYTES + 1));
  commit('é'.repeat(MAX_TEXT_BYTES / 2));
  m.keyboard.dispatch('beforeinput', {inputType: 'insertText', data: '語'.repeat(MAX_TEXT_BYTES)});
  for (let i = 0; i < 8; i++) await tick();
  const texts = m.sent.flatMap(request => request.events).filter(event => 'Text' in event).map(({Text}) => Text.text);
  assert.deepEqual(texts, ['é'.repeat(MAX_TEXT_BYTES / 2)], 'empty, oversize and unfocused text never leaves the page');
  m.layer.close();
  const watcher = await controlled({control: false});
  watcher.place([{key: KEY, rect: [10, 36, 410, 300], focused: true}]);
  watcher.run();
  watcher.keyboard.dispatch('compositionstart', {data: ''});
  watcher.keyboard.dispatch('compositionend', {data: '日本'});
  watcher.keyboard.dispatch('beforeinput', {inputType: 'insertText', data: '🎉'});
  await tick();
  assert.deepEqual(watcher.sent, [], 'a watcher sends no text');
  watcher.layer.close();
});

/**
 * One keystroke as a browser delivers it to the hidden text area: `keydown`;
 * then, unless the page cancelled that keydown, the browser's default action
 * inserts what the platform produced for it, one `beforeinput`/`input` pair
 * per character it reports (a cancelled `beforeinput` leaves the area empty);
 * then `keyup`.
 */
function stroke(keyboard, down, inserted = [], up = {...down, key: down.key === 'Dead' ? 'Dead' : down.key?.toLowerCase()}) {
  let prevented = false;
  keyboard.dispatch('keydown', {...down, preventDefault() { prevented = true; }});
  if (!prevented) {
    for (const data of inserted) {
      let cancelled = false;
      keyboard.dispatch('beforeinput', {inputType: 'insertText', data, preventDefault() { cancelled = true; }});
      if (!cancelled) keyboard.value = `${keyboard.value ?? ''}${data}`;
      keyboard.dispatch('input', {inputType: 'insertText', data});
    }
  }
  keyboard.dispatch('keyup', up);
}
const modifiersOff = {shiftKey: false, ctrlKey: false, altKey: false, metaKey: false};
/**
 * A dead key as Chromium and Firefox on Windows report it, and Linux browsers
 * whose input method commits without a preedit: a `keydown` with
 * `key: 'Dead'` and no composition. Nothing is inserted until the next key,
 * whose default action inserts the composed character (`insertText`).
 */
const dead = (code, keyCode, extra = {}) => ({code, key: 'Dead', keyCode, ...modifiersOff, ...extra});

test('a dead key in a Windows or Linux browser reaches the focused window as the composed character, once', async () => {
  const m = await controlled({control: true});
  m.place([{key: KEY, rect: [10, 36, 410, 300], focused: true}]);
  m.run();
  await tick();
  const before = m.sent.flatMap(request => request.events).length;
  // German layout: ´ (the Equal key) then e is é; ^ (Backquote) then o is ô.
  stroke(m.keyboard, dead('Equal', 221));
  stroke(m.keyboard, {code: 'KeyE', key: 'é', keyCode: 69, ...modifiersOff}, ['é'], {code: 'KeyE', key: 'e', keyCode: 69, ...modifiersOff});
  stroke(m.keyboard, dead('Backquote', 220));
  stroke(m.keyboard, {code: 'KeyO', key: 'ô', keyCode: 79, ...modifiersOff}, ['ô'], {code: 'KeyO', key: 'o', keyCode: 79, ...modifiersOff});
  // A dead key then Space is the accent itself.
  stroke(m.keyboard, dead('Equal', 221));
  stroke(m.keyboard, {code: 'Space', key: '´', keyCode: 32, ...modifiersOff}, ['´'], {code: 'Space', key: ' ', keyCode: 32, ...modifiersOff});
  // A dead key then a key it does not combine with is both characters, as
  // the browser reports them (Windows produces two).
  stroke(m.keyboard, dead('Equal', 221));
  stroke(m.keyboard, {code: 'KeyX', key: 'x', keyCode: 88, ...modifiersOff}, ['´', 'x']);
  // French layout: ^ (BracketLeft), then Shift+e is Ê; the Shift edges still reach the window.
  stroke(m.keyboard, dead('BracketLeft', 221));
  m.keyboard.dispatch('keydown', {code: 'ShiftLeft', key: 'Shift', keyCode: 16, ...modifiersOff, shiftKey: true});
  stroke(m.keyboard, {code: 'KeyE', key: 'Ê', keyCode: 69, ...modifiersOff, shiftKey: true}, ['Ê'], {code: 'KeyE', key: 'E', keyCode: 69, ...modifiersOff, shiftKey: true});
  m.keyboard.dispatch('keyup', {code: 'ShiftLeft', key: 'Shift', keyCode: 16, ...modifiersOff});
  // Plain keys after it are keys again, and so is a shortcut chord pressed after a dead key.
  stroke(m.keyboard, {code: 'KeyA', key: 'a', keyCode: 65, ...modifiersOff});
  stroke(m.keyboard, dead('Equal', 221));
  m.keyboard.dispatch('keydown', {code: 'ControlLeft', key: 'Control', keyCode: 17, ...modifiersOff, ctrlKey: true});
  stroke(m.keyboard, {code: 'KeyL', key: 'l', keyCode: 76, ...modifiersOff, ctrlKey: true});
  m.keyboard.dispatch('keyup', {code: 'ControlLeft', key: 'Control', keyCode: 17, ...modifiersOff});
  stroke(m.keyboard, {code: 'KeyB', key: 'b', keyCode: 66, ...modifiersOff});
  for (let i = 0; i < 8; i++) await tick();
  assert.equal(m.keyboard.value ?? '', '', 'nothing is left in the hidden target');
  const key = (keysym, pressed) => ({Key: {keysym, pressed, modifiers: 0}});
  assert.deepEqual(m.sent.flatMap(request => request.events).slice(before), [
    {Text: {text: 'é'}},
    {Text: {text: 'ô'}},
    {Text: {text: '´'}},
    {Text: {text: '´'}},
    {Text: {text: 'x'}},
    key(42, true),
    {Text: {text: 'Ê'}},
    key(42, false),
    key(30, true), key(30, false),
    key(29, true), key(38, true), key(38, false), key(29, false),
    key(48, true), key(48, false),
  ], 'one Text per composed character, never the dead key or the key that completed it');
  m.layer.close();
});

test('a dead key that opens a composition (Linux preedit, Firefox on macOS) leaves no key held in the window', async () => {
  const m = await controlled({control: true});
  m.place([{key: KEY, rect: [10, 36, 410, 300], focused: true}]);
  m.run();
  await tick();
  const before = m.sent.flatMap(request => request.events).length;
  // Firefox with a GTK input method that shows the accent as a preedit: the
  // dead keydown arrives before the composition starts.
  m.keyboard.dispatch('keydown', dead('Equal', 0));
  m.keyboard.dispatch('compositionstart', {data: ''});
  m.keyboard.dispatch('beforeinput', {inputType: 'insertCompositionText', data: '´', isComposing: true});
  m.keyboard.dispatch('keyup', {...dead('Equal', 0), isComposing: true});
  m.keyboard.dispatch('keydown', {code: 'KeyE', key: 'Process', keyCode: 229, isComposing: true});
  m.keyboard.dispatch('beforeinput', {inputType: 'insertCompositionText', data: 'é', isComposing: true});
  m.keyboard.dispatch('compositionend', {data: 'é'});
  m.keyboard.dispatch('keyup', {code: 'KeyE', key: 'e', keyCode: 69});
  // Firefox on macOS: Option+e is a dead key reported with its keyCode, then a composition.
  m.keyboard.dispatch('keydown', {code: 'AltLeft', key: 'Alt', keyCode: 18, ...modifiersOff, altKey: true});
  m.keyboard.dispatch('keydown', dead('KeyE', 69, {altKey: true}));
  m.keyboard.dispatch('compositionstart', {data: ''});
  m.keyboard.dispatch('keyup', {...dead('KeyE', 69, {altKey: true}), isComposing: true});
  m.keyboard.dispatch('keyup', {code: 'AltLeft', key: 'Alt', keyCode: 18, ...modifiersOff, isComposing: true});
  m.keyboard.dispatch('keydown', {code: 'KeyU', key: 'Process', keyCode: 229, isComposing: true});
  m.keyboard.dispatch('compositionend', {data: 'ú'});
  m.keyboard.dispatch('keyup', {code: 'KeyU', key: 'u', keyCode: 85});
  m.keyboard.dispatch('blur');
  for (let i = 0; i < 8; i++) await tick();
  const events = m.sent.flatMap(request => request.events).slice(before);
  assert.deepEqual(events.filter(event => 'Text' in event), [{Text: {text: 'é'}}, {Text: {text: 'ú'}}]);
  assert.deepEqual(events.filter(event => 'Key' in event && event.Key.keysym !== 56), [],
    'neither dead key is pressed in the window, so blur has none to release');
  m.layer.close();
});

test('dead-key text goes only to the focused window of a member who drives it', async () => {
  const typeAcute = keyboard => {
    stroke(keyboard, dead('Equal', 221));
    stroke(keyboard, {code: 'KeyE', key: 'é', keyCode: 69, ...modifiersOff}, ['é']);
  };
  const m = await controlled({control: true});
  typeAcute(m.keyboard);
  for (let i = 0; i < 4; i++) await tick();
  assert.deepEqual(m.sent.flatMap(request => request.events).filter(event => 'Text' in event || 'Key' in event), [],
    'a window that does not hold the keyboard gets neither text nor keys');
  m.layer.close();
  const watcher = await controlled({control: false});
  watcher.place([{key: KEY, rect: [10, 36, 410, 300], focused: true}]);
  watcher.run();
  typeAcute(watcher.keyboard);
  await tick();
  assert.deepEqual(watcher.sent, [], 'a watcher sends nothing');
  watcher.layer.close();
});

test('closing a window a member drives closes it at its source', async () => {
  let closedKeys = [];
  const options = {dismissed: () => { const keys = closedKeys; closedKeys = []; return keys; }};
  const driver = await controlled({control: true, ...options});
  closedKeys = [KEY, 'cd'.repeat(32) + ':0000000000000001'];
  driver.run();
  await tick();
  assert.deepEqual(driver.sent.map(request => request.events), [['Close']]);
  driver.layer.close();
  const watcher = await controlled({control: false, ...options});
  closedKeys = [KEY];
  watcher.poll();
  await tick();
  assert.deepEqual(watcher.sent, [], 'a watcher only closes its own view');
  watcher.layer.close();
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
  assert.deepEqual(decodeFrame(json(1, [{...STREAM, codec: 'hevc-main10'}])), {kind: 'index', streams: []},
    'a codec the page does not know lists nothing');
  assert.equal(decodeFrame(json(2, {...STREAM, codec: 'hevc-main10'})), null);
  assert.equal(decodeFrame(json(1, [STREAM, 7])), null, 'a stream that is not one still spoils the index');
});

test('a stream in a codec the page does not know is left out, and the publisher\'s other windows still show', () => {
  const other = {...STREAM, surface: '0000000000000008', record: '@tilo:legacy-window:0000000000000008', codec: 'hevc-main10'};
  assert.deepEqual(decodeFrame(json(1, [other, STREAM])), {kind: 'index', streams: [STREAM]});
  const m = mount();
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [other, STREAM]));
  assert.deepEqual(m.shown.at(-1), [{key: KEY, app: STREAM.app, title: 'Chromium', codec: 'h264'}]);
  m.layer.close();
});

test('VP9 and AV1 streams decode with their codec\'s configuration, and a session\'s switch reconfigures', () => {
  const m = mount();
  const vp9 = {...STREAM, codec: 'vp9'};
  // The first bytes of a libvpx key frame and the inter frame after it.
  const vp9Key = [0x82, 0x49, 0x83, 0x42, 0x00, 0x00, 0xf0, 0x00];
  const vp9Inter = [0x86, 0x00, 0x40, 0x92, 0x9c, 0x28, 0x49, 0x40];
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [vp9]));
  assert.deepEqual(m.shown.at(-1), [{key: KEY, app: STREAM.app, title: 'Chromium', codec: 'vp9'}]);
  m.live.deliver(STREAM.record, SHELL, json(2, vp9));
  m.live.deliver(STREAM.record, SHELL, chunk(CHUNK_KEY | CHUNK_FIRST | CHUNK_LAST, 0, vp9Key));
  m.live.deliver(STREAM.record, SHELL, chunk(CHUNK_FIRST | CHUNK_LAST, 1, vp9Inter));
  assert.equal(m.codecs.decoders[0].config.codec, 'vp09.00.10.08');
  assert.deepEqual(m.codecs.decoders[0].chunks.map(chunk => chunk.type), ['key', 'delta']);
  // Its session switched to AV1 (a viewer joined that decodes it): the
  // window stays, and the next key frame reconfigures.
  const av1 = {...STREAM, codec: 'av1'};
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [av1]));
  m.live.deliver(STREAM.record, SHELL, json(2, av1));
  m.live.deliver(STREAM.record, SHELL, chunk(CHUNK_FIRST | CHUNK_LAST, 2, vp9Inter));
  assert.equal(m.codecs.decoders.length, 1, 'nothing decodes before the new key frame');
  m.live.deliver(STREAM.record, SHELL, chunk(CHUNK_KEY | CHUNK_FIRST | CHUNK_LAST, 3, [0x12, 0x00, 0x0a, 0x0b]));
  assert.equal(m.codecs.decoders.length, 2);
  assert.equal(m.codecs.decoders[0].state, 'closed');
  assert.equal(m.codecs.decoders[1].config.codec, 'av01.0.04M.08');
  assert.equal(m.layer.stats().streams, 1);
  m.layer.close();
});

test('the page says which video codecs its WebCodecs decodes', async () => {
  const asked = [];
  const VideoDecoder = {
    async isConfigSupported(config) {
      asked.push(config.codec);
      if (config.codec.startsWith('av01')) throw new Error('not here');
      return {supported: config.codec !== 'nope', config};
    },
  };
  assert.deepEqual(await decodableCodecs(VideoDecoder), ['h264', 'vp9']);
  assert.deepEqual(asked, Object.values(VIDEO_DECODER_CONFIGS));
  assert.deepEqual(await decodableCodecs({isConfigSupported: async () => ({supported: false})}), []);
  assert.deepEqual(await decodableCodecs(undefined), [], 'without WebCodecs the page plays no window');
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
  const keyboard = keyboardOf(layer.layer);
  keyboard.dispatch('keydown', {code: 'KeyA'});
  await tick();
  assert.equal(sent.length, 1, 'the first key is in flight before closing');
  keyboard.dispatch('keydown', {code: 'KeyB'});
  layer.close();
  complete(true);
  await tick();
  assert.equal(sent.length, 1, 'no queued key is sent after close');
  m.layer.close();
});


test('blur releases held buttons and keys before relinquishing keyboard focus', async () => {
  const m = await controlled({control: true});
  m.canvas.dispatch('pointerdown', {button: 0, pointerId: 5, clientX: 210, clientY: 168});
  m.keyboard.dispatch('keydown', {code: 'ShiftLeft'});
  await tick();
  await tick();
  m.keyboard.dispatch('blur');
  await tick();
  await tick();
  const events = m.sent.flatMap(request => request.events);
  assert.ok(events.some(event => event.Key?.keysym === 42 && event.Key.pressed === false));
  assert.ok(events.some(event => event.PointerButton?.button === 272 && event.PointerButton.pressed === false));
  assert.deepEqual(events.at(-1), {KeyboardFocus: {surface: 0, focused: false}});
  m.layer.close();
});

const PAINT = {...STREAM, app: 'so.tilo.browser', codec: 'tilo-paint'};
const PAINT_KEY = `${SHELL}:${PAINT.surface}`;

test('a painted page is handed to the runtime to draw, not decoded as video', async () => {
  const painted = [];
  const m = mount({paintFrame: (key, bytes) => { painted.push({key, bytes: [...bytes]}); return true; }});
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [PAINT]));
  assert.deepEqual(m.shown.at(-1), [{key: PAINT_KEY, app: PAINT.app, title: 'Browser', codec: 'tilo-paint'}],
    'the desktop opens a window it draws itself');
  assert.equal(m.layer.layer.children.length, 0, 'no canvas over the desktop');
  m.live.deliver(PAINT.record, SHELL, json(2, PAINT));
  // A whole frame (first byte 0) split across two chunks, then a patch.
  m.live.deliver(PAINT.record, SHELL, chunk(CHUNK_KEY | CHUNK_FIRST, 0, [0, 1, 2], 5));
  m.live.deliver(PAINT.record, SHELL, chunk(CHUNK_KEY | CHUNK_LAST, 0, [3, 4], 5));
  m.live.deliver(PAINT.record, SHELL, chunk(CHUNK_FIRST | CHUNK_LAST, 1, [1, 9]));
  assert.deepEqual(painted, [{key: PAINT_KEY, bytes: [0, 1, 2, 3, 4]}, {key: PAINT_KEY, bytes: [1, 9]}]);
  assert.equal(m.codecs.decoders.length, 0, 'no video decoder');
  assert.equal(m.layer.stats().drawn, 2);
  // A start that changes the codec cannot turn the page into video.
  m.live.deliver(PAINT.record, SHELL, json(2, {...PAINT, codec: 'h264'}));
  assert.equal(m.layer.streams()[0].codec, 'tilo-paint');
  m.place([{key: PAINT_KEY, rect: [0, 0, 100, 100]}]);
  m.run();
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, []));
  assert.equal(m.layer.stats().streams, 0);
  m.layer.close();
  await tick();
});

test('input on a painted page goes to its host only when this member drives it', async () => {
  const events = [{PointerMotion: {x: 20, y: 30}}, {PointerButton: {button: 272, pressed: true}}];
  let pending = [{key: PAINT_KEY, events}];
  const take = () => { const out = pending; pending = []; return out; };
  const watcher = mount({takePaintInput: take});
  watcher.live.deliver(INDEX_RECORD.record, SHELL, json(1, [PAINT]));
  await tick();
  watcher.run();
  await tick();
  assert.deepEqual(watcher.sent, [], 'a watching member sends nothing');
  watcher.layer.close();

  pending = [{key: PAINT_KEY, events}, {key: 'unknown:0000000000000001', events}];
  const driver = mount({control: true, takePaintInput: take});
  driver.live.deliver(INDEX_RECORD.record, SHELL, json(1, [PAINT]));
  await tick();
  driver.run();
  await tick();
  assert.ok(driver.sent.every(({publisher, stream}) => publisher === SHELL && stream === PAINT.surface),
    'only to the painted stream\'s host; an unknown window\'s input is dropped');
  assert.deepEqual(driver.sent.flatMap(request => request.events), events, 'in order');
  driver.layer.close();
  await tick();
});


test('a delayed HTTPS send preserves a mixed-case typing burst in wire order', async () => {
  let release;
  const sent = [], errors = [], delivered = [];
  let clock;
  const m = await controlled({control: true, onError: error => errors.push(String(error)),
    sendInput: async (_, __, events) => {
      if (!events.length) return true;
      sent.push(events);
      if (sent.length === 1) await new Promise(resolve => { release = resolve; });
      delivered.push({at: clock.t, count: events.length});
      return true;
    }});
  clock = m.clock;
  const physical = [
    ['ShiftLeft', 42, true, true], ['KeyA', 30, true, true], ['KeyA', 30, false, true],
    ['ShiftLeft', 42, false, false], ['KeyB', 48, true, false], ['KeyB', 48, false, false],
    ['Digit1', 2, true, false], ['Digit1', 2, false, false],
    ['ShiftLeft', 42, true, true], ['Slash', 53, true, true], ['Slash', 53, false, true],
    ['ShiftLeft', 42, false, false],
  ];
  const expected = [];
  for (let repeat = 0; repeat < 20; repeat++) {
    for (const [code, keysym, pressed, shiftKey] of physical) {
      m.keyboard.dispatch(pressed ? 'keydown' : 'keyup', {code, shiftKey});
      expected.push({Key: {keysym, pressed, modifiers: 0}});
    }
  }
  await tick();
  assert.equal(sent.length, 1, 'one send in flight while the remaining physical keys queue');
  assert.equal(m.layer.streams()[0].control, true, 'typing retains the granted control');
  assert.deepEqual(errors, []);
  clock.t = 4000;
  release();
  for (let batch = 0; batch < 10; batch++) await tick();
  assert.deepEqual(sent.flat(), expected, 'every physical key reaches the host exactly once in order');
  assert.ok(sent.every(batch => batch.length <= 64), 'each request respects the host wire bound');
  for (const sample of delivered) {
    const count = delivered.filter(row => row.at >= sample.at && row.at < sample.at + 1000).reduce((sum, row) => sum + row.count, 0);
    assert.ok(count <= 120, `catch-up offered ${count} events inside the native rate window`);
  }
  m.layer.close();
});

test('an unresponsive host still bounds the input queue and revokes control', async () => {
  let release;
  const errors = [], sent = [];
  const m = await controlled({control: true, onError: error => errors.push(String(error)),
    sendInput: async (_, __, events) => {
      if (!events.length) return true;
      sent.push(events);
      await new Promise(resolve => { release = resolve; });
      return true;
    }});
  for (let i = 0; i < 600; i++) m.keyboard.dispatch(i % 2 ? 'keyup' : 'keydown', {code: 'KeyA'});
  assert.equal(m.layer.streams()[0].control, false);
  assert.deepEqual(errors, ['Error: legacy window input backlog exceeded']);
  await tick();
  release();
  await tick();
  assert.equal(sent.length, 1, 'revocation drops the unsent queue');
  m.layer.close();
});


test('two windows share the native input rate budget while draining queued keys', async () => {
  const sent = [], errors = [];
  let m;
  m = mount({control: true, onError: error => errors.push(String(error)),
    sendInput: async (_, surface, events) => {
      if (events.length) sent.push({surface, events, at: m.clock.t});
      return true;
    }});
  const second = {...STREAM, surface: '0000000000000008', record: '@tilo:legacy-window:0000000000000008'};
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [STREAM, second]));
  await tick();
  const [first, other] = [keyboardOf(m.layer.layer), keyboardOf(m.layer.layer, 1)];
  for (let i = 0; i < 60; i++) {
    for (const [keyboard, code] of [[first, 'KeyA'], [other, 'KeyB']]) {
      keyboard.dispatch('keydown', {code});
      keyboard.dispatch('keyup', {code});
    }
  }
  for (let batch = 0; batch < 10; batch++) await tick();
  for (const [surface, keysym] of [[STREAM.surface, 30], [second.surface, 48]]) {
    const expected = Array.from({length: 120}, (_, i) => ({Key: {keysym, pressed: i % 2 === 0, modifiers: 0}}));
    assert.deepEqual(sent.filter(row => row.surface === surface).flatMap(row => row.events), expected);
  }
  for (const sample of sent) {
    const count = sent.filter(row => row.at >= sample.at && row.at < sample.at + 1000).reduce((sum, row) => sum + row.events.length, 0);
    assert.ok(count <= 120, `both windows offered ${count} events inside one native rate window`);
  }
  assert.deepEqual(errors, []);
  assert.ok(m.layer.streams().every(stream => stream.control));
  m.layer.close();
});


test('a dismissed window queues one close while its host acknowledges it', async () => {
  let release;
  const sent = [];
  const m = await controlled({control: true, dismissed: () => [KEY],
    sendInput: async (_, __, events) => {
      if (!events.length) return true;
      sent.push(events);
      await new Promise(resolve => { release = resolve; });
      return true;
    }});
  await tick();
  for (let frame = 0; frame < 100; frame++) m.run();
  assert.deepEqual(sent, [['Close']], 'painting must not flood the shared sender with repeated closes');
  release();
  await tick();
  m.run();
  await tick();
  assert.deepEqual(sent, [['Close']], 'the same dismissal is still one action after acknowledgement');
  m.layer.close();
});

test('a window that goes still while its decoder skips a backlog is subscribed again and ends on its last picture', () => {
  const m = mount();
  m.live.deliver(INDEX_RECORD.record, SHELL, json(1, [STREAM]));
  const send = (seq, key) => m.live.deliver(STREAM.record, SHELL,
    chunk((key ? CHUNK_KEY : 0) | CHUNK_FIRST | CHUNK_LAST, seq, key ? SPS_IDR : [0, 0, 1, 0x41]));
  const group = (from, to) => {
    m.live.deliver(STREAM.record, SHELL, json(2, STREAM));
    for (let seq = from; seq <= to; seq++) send(seq, seq === from);
  };
  // The first group (what the host replays to the new subscription) is decoded
  // whole even though it arrives at once and outruns the decoder.
  group(0, 1);
  const decoder = m.codecs.decoders[0];
  decoder.decodeQueueSize = 100;
  send(2, false);
  assert.equal(decoder.chunks.length, 3, 'the replayed group is caught up on');
  // Live: the next group is subject to the backlog bound, and its tail is skipped.
  group(3, 3);
  send(4, false);
  send(5, false);
  assert.equal(decoder.chunks.length, 3, 'a backlog skips until the next keyframe');
  // Still within the quiet time, nothing changes.
  m.clock.t += 100;
  m.poll();
  m.run();
  assert.equal(m.layer.stats().resyncs, 0);
  // The window went still: no keyframe will come. Subscribe again.
  m.clock.t += 500;
  m.poll();
  m.run();
  assert.equal(m.layer.stats().resyncs, 1);
  const watches = m.live.watches.filter(watch => watch.record.record === STREAM.record);
  assert.deepEqual(watches.map(watch => watch.active), [false, true], 'one live subscription, the new one');
  // The host replays its current group: all of it is decoded, ending on frame 5.
  group(3, 5);
  assert.deepEqual(decoder.chunks.slice(3).map(chunk => chunk.type), ['key', 'delta', 'delta']);
  assert.equal(m.codecs.decoders.length, 1, 'the same decoder');
  // Caught up, nothing further to resync.
  m.clock.t += 1000;
  m.poll();
  m.run();
  assert.equal(m.layer.stats().resyncs, 1);
  m.layer.close();
});

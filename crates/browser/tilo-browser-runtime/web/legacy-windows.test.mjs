import test from 'node:test';
import assert from 'node:assert/strict';
import {
  CHUNK_FIRST, CHUNK_KEY, CHUNK_LAST, INDEX_RECORD, LEGACY_APP,
  createLegacyWindows, createReassembler, createStreamDecoder, decodeFrame, h264CodecString, matchStreams,
} from './legacy-windows.mjs';

const unhex = value => Uint8Array.from(value.match(/../g), byte => parseInt(byte, 16));
const utf8 = new TextEncoder();
const STREAM = {source: '000000000000abcd', surface: '0000000000000007', app: 'org.chromium.Chromium', codec: 'h264', width: 1280, height: 720, record: '@tilo:legacy-window:000000000000abcd:0000000000000007'};
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

test('frames decode exactly as the host encodes them', () => {
  // `live_wire::legacy_window::Frame::Chunk` for these fields, byte for byte.
  assert.deepEqual(decodeFrame(unhex('f10307000000090000000000000063' + '00000003000001')), {
    kind: 'chunk', flags: 7, seq: 9, tsUs: 99, total: 3, data: Uint8Array.from([0, 0, 1]),
  });
  assert.deepEqual(decodeFrame(json(1, [STREAM])), {kind: 'index', streams: [STREAM]});
  assert.deepEqual(decodeFrame(json(2, STREAM)), {kind: 'start', stream: STREAM});
  assert.deepEqual(decodeFrame(Uint8Array.from([0xf1, 4])), {kind: 'end'});
  for (const bad of [[], [0xf1], [0x01, 1], [0xf1, 9], [0xf1, 4, 0], [0xf1, 3, 7, 0, 0]]) assert.equal(decodeFrame(Uint8Array.from(bad)), null);
  assert.equal(decodeFrame(json(1, [{...STREAM, width: -1}])), null, 'a malformed stream spoils the index');
  assert.equal(decodeFrame(json(2, {...STREAM, extra: 1}).subarray(0, 5)), null);
});

test('chunks reassemble in order and a gap drops the frame', () => {
  const r = createReassembler();
  assert.equal(r.push(decodeFrame(chunk(CHUNK_KEY | CHUNK_FIRST, 1, [1, 2], 5))), null);
  assert.equal(r.push(decodeFrame(chunk(CHUNK_KEY, 1, [3, 4], 5))), null);
  assert.deepEqual(r.push(decodeFrame(chunk(CHUNK_KEY | CHUNK_LAST, 1, [5], 5))), {key: true, seq: 1, tsUs: 99, data: Uint8Array.from([1, 2, 3, 4, 5])});
  // A missing middle chunk: the frame never completes.
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

test('streams are drawn over tabs of their app, in desktop order, and keep their tab', () => {
  const tile = (tab, app, x) => ({id: tab, rect: [x, 0, 100, 100], legacy: {app, tab, content: [x, 30, 100, 70]}});
  const snapshot = {covered: false, groups: [tile('t1', 'org.chromium.Chromium', 0), {id: 'w', rect: [100, 0, 50, 50], legacy: null}, tile('t2', 'ORG.chromium.chromium', 200), tile('t3', 'org.gimp.GIMP', 400)]};
  const streams = new Map([['a', {app: 'org.chromium.Chromium'}], ['b', {app: 'org.chromium.Chromium'}], ['c', {app: 'org.libreoffice.LibreOffice'}]]);
  const first = matchStreams(snapshot, streams);
  assert.deepEqual([...first], [['a', {tab: 't1', content: [0, 30, 100, 70]}], ['b', {tab: 't2', content: [200, 30, 100, 70]}]]);
  const swapped = matchStreams(snapshot, new Map([['b', streams.get('b')], ['a', streams.get('a')]]), first);
  assert.equal(swapped.get('a').tab, 't1', 'an assignment survives a reordered index');
  assert.equal(matchStreams({...snapshot, covered: true}, streams).size, 0, 'hidden under a desktop activity');
  assert.equal(matchStreams(null, streams).size, 0);
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

// --- minimal DOM and live client --------------------------------------------------
function fakeDocument() {
  function element(tag) {
    return {
      tag, children: [], parent: null, dataset: {}, style: {}, width: 0, height: 0, drawn: 0,
      append(...nodes) { for (const child of nodes) { child.parent = this; this.children.push(child); } },
      remove() { if (this.parent) this.parent.children = this.parent.children.filter(child => child !== this); this.parent = null; },
      getContext() { const node = this; return {drawImage() { node.drawn++; }}; },
    };
  }
  return {createElement: element};
}
function fakeLive() {
  const watches = [];
  return {
    watches,
    watchRecord(record, onFrame) { const entry = {record, onFrame, active: true}; watches.push(entry); return () => { entry.active = false; }; },
    deliver(record, publisher, bytes) { for (const entry of watches) if (entry.active && entry.record.record === record) entry.onFrame(publisher, bytes); },
  };
}

test('a listed stream is subscribed, decoded and drawn over its tab until the index drops it', () => {
  const doc = fakeDocument();
  const surface = doc.createElement('div');
  const live = fakeLive();
  const codecs = fakeCodecs();
  let clock = 0;
  const frames = [];
  let snapshot = {covered: false, groups: [{id: 'g', rect: [10, 0, 400, 300], legacy: {app: 'org.chromium.Chromium', tab: 't', content: [10, 36, 400, 264]}}]};
  let presented = 1;
  let poll;
  const layer = createLegacyWindows({
    surface, document: doc, live: () => live, snapshot: () => JSON.stringify(snapshot), presented: () => presented,
    ...codecs, now: () => clock,
    requestFrame: callback => { frames.push(callback); return frames.length; }, cancelFrame: () => {},
    setInterval: callback => { poll = callback; return 1; }, clearInterval: () => {},
  });
  const run = () => { for (const callback of frames.splice(0)) callback(); };
  assert.deepEqual(live.watches.map(w => w.record), [INDEX_RECORD]);
  const shell = 'ab'.repeat(32);
  live.deliver(INDEX_RECORD.record, shell, json(1, [STREAM]));
  assert.deepEqual(live.watches[1].record, {app: LEGACY_APP, record: STREAM.record});
  live.deliver(STREAM.record, 'cd'.repeat(32), chunk(CHUNK_KEY | CHUNK_FIRST | CHUNK_LAST, 0, SPS_IDR));
  assert.equal(codecs.decoders.length, 0, 'another namespace cannot feed the stream');
  live.deliver(STREAM.record, shell, json(2, STREAM));
  live.deliver(STREAM.record, shell, chunk(CHUNK_KEY | CHUNK_FIRST, 0, SPS_IDR.slice(0, 6), SPS_IDR.length));
  live.deliver(STREAM.record, shell, chunk(CHUNK_KEY | CHUNK_LAST, 0, SPS_IDR.slice(6), SPS_IDR.length));
  assert.equal(codecs.decoders[0].chunks.length, 1);
  codecs.decoders[0].output({displayWidth: 1280, displayHeight: 720, close() { this.closed = true; }});
  run();
  const canvas = layer.layer.children[0];
  assert.equal(canvas.drawn, 1);
  assert.equal(canvas.style.display, 'block');
  assert.equal(canvas.style.transform, 'translate3d(10.0px,36.0px,0)');
  assert.equal(canvas.style.width, '400.0px');
  assert.equal(canvas.dataset.tab, 't');
  assert.deepEqual(layer.stats().visible, 1);
  // The tab goes away: the stream stays subscribed but is not drawn.
  snapshot = {covered: false, groups: []};
  presented++;
  run();
  assert.equal(canvas.style.display, 'none');
  assert.deepEqual(layer.streams().map(stream => stream.tab), [null]);
  live.deliver(INDEX_RECORD.record, shell, json(1, []));
  assert.equal(layer.stats().streams, 0);
  assert.equal(live.watches[1].active, false, 'an unlisted stream is unsubscribed');
  assert.equal(codecs.decoders[0].state, 'closed');
  assert.equal(layer.layer.children.length, 0);
  // An index that stops arriving expires its streams.
  live.deliver(INDEX_RECORD.record, shell, json(1, [STREAM]));
  assert.equal(layer.stats().streams, 1);
  clock += 4000;
  poll();
  assert.equal(layer.stats().streams, 1, 'still within the index lifetime');
  clock += 1001;
  poll();
  assert.equal(layer.stats().streams, 0);
  layer.close();
  assert.equal(surface.children.length, 0);
});

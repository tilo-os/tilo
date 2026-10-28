import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {launch} from '../../../../tools/web/chrome.mjs';
import {CODECS, frameAt, frameCentre, muxMatroska, pickCodec, vint} from './tilo-media-edit.mjs';

test('a frame is counted by the interval that holds the time, and sought at its middle', () => {
  assert.equal(frameAt(2.066, 30), 61);
  assert.equal(frameAt(62 / 30, 30), 62);
  assert.equal(frameAt(0.1 * 3, 10), 3, 'a boundary a hair under in floating point');
  assert.equal(frameAt(-1, 30), 0);
  // The middle of a frame is inside it, whatever the rounding.
  for (const fps of [24, 25, 29.97, 30, 60]) {
    for (const frame of [0, 1, 61, 1799]) assert.equal(frameAt(frameCentre(frame, fps), fps), frame);
  }
});

test('element sizes are the shortest EBML integers, never the reserved all-ones', () => {
  assert.deepEqual([...vint(0)], [0x80]);
  assert.deepEqual([...vint(126)], [0xFE]);
  // 127 in one byte would be all ones, which means "unknown size".
  assert.deepEqual([...vint(127)], [0x40, 0x7F]);
  assert.deepEqual([...vint(300)], [0x41, 0x2C]);
  assert.equal(vint(2 ** 30).length, 5);
});

// Reads an EBML element at `at`: its id, payload start and end.
function readElement(bytes, at) {
  const first = bytes[at];
  let idLength = 1;
  while (!(first & (0x80 >> (idLength - 1)))) idLength++;
  let id = 0;
  for (let i = 0; i < idLength; i++) id = id * 256 + bytes[at + i];
  const sizeAt = at + idLength;
  let sizeLength = 1;
  while (!(bytes[sizeAt] & (0x80 >> (sizeLength - 1)))) sizeLength++;
  let size = bytes[sizeAt] & (0xFF >> sizeLength);
  for (let i = 1; i < sizeLength; i++) size = size * 256 + bytes[sizeAt + i];
  const start = sizeAt + sizeLength;
  return {id, at, start, end: start + size};
}

function children(bytes, start, end) {
  const out = [];
  for (let at = start; at < end;) {
    const element = readElement(bytes, at);
    out.push(element);
    at = element.end;
  }
  return out;
}

test('a muxed file is a WebM header and one segment whose cues point at key-frame clusters', () => {
  const chunks = [];
  for (let i = 0; i < 90; i++) {
    chunks.push({data: Uint8Array.of(i, i, i), timestamp: Math.round(i * 1e6 / 30), key: i % 60 === 0});
  }
  const file = muxMatroska({doc: 'webm', codecId: 'V_VP9', codecPrivate: null, width: 64, height: 36, fps: 30, chunks});
  const top = children(file, 0, file.length);
  assert.deepEqual(top.map(e => e.id), [0x1A45DFA3, 0x18538067]);
  assert.equal(top[1].end, file.length, 'the segment runs to the end of the file');
  const segment = children(file, top[1].start, top[1].end);
  const ids = segment.map(e => e.id);
  assert.deepEqual(ids.slice(0, 3), [0x1549A966, 0x1654AE6B, 0x1C53BB6B], 'info, tracks, then cues');
  const clusters = segment.filter(e => e.id === 0x1F43B675);
  assert.equal(clusters.length, 2, 'a cluster per key frame here');
  const blocks = clusters.flatMap(c => children(file, c.start, c.end).filter(e => e.id === 0xA3));
  assert.equal(blocks.length, 90, 'every chunk is written once');
  // Each cue's position is the offset of a cluster from the segment's data.
  const cues = children(file, segment[2].start, segment[2].end);
  assert.equal(cues.length, 2);
  for (const [i, cue] of cues.entries()) {
    const positions = children(file, cue.start, cue.end).find(e => e.id === 0xB7);
    const position = children(file, positions.start, positions.end).find(e => e.id === 0xF1);
    let value = 0;
    for (let at = position.start; at < position.end; at++) value = value * 256 + file[at];
    assert.equal(top[1].start + value, clusters[i].at);
  }
});

test('the codec is the first the browser encodes, H.264 only when it encodes nothing royalty-free', async () => {
  const only = supported => ({
    VideoEncoder: Object.assign(function VideoEncoder() {}, {
      isConfigSupported: async config => ({supported: supported.includes(config.codec)}),
    }),
  });
  const vp9 = await pickCodec(1280, 720, 30, 8e6, only(CODECS.map(c => c.codec)));
  assert.equal(vp9.choice.id, 'V_VP9');
  const avc = await pickCodec(1280, 720, 30, 8e6, only(['avc1.640028']));
  assert.equal(avc.choice.id, 'V_MPEG4/ISO/AVC');
  assert.equal(avc.choice.doc, 'matroska', 'H.264 is not a WebM codec');
  assert.deepEqual(avc.config.avc, {format: 'avc'}, 'the decoder description rides in the file, not the stream');
  assert.equal(await pickCodec(1280, 720, 30, 8e6, only([])), null);
  assert.equal(await pickCodec(1280, 720, 30, 8e6, {}), null, 'no WebCodecs at all');
});

// In a real browser: encode a clip whose every frame says its number in its
// colour, play the file back, and seek it frame by frame.
test('a clip encoded on the page plays back, and every seek lands on the exact frame', async t => {
  const module = await readFile(new URL('./tilo-media-edit.mjs', import.meta.url));
  const server = createServer((request, response) => {
    if (request.url === '/edit.mjs') {
      response.setHeader('content-type', 'text/javascript');
      response.end(module);
    } else {
      response.setHeader('content-type', 'text/html');
      response.end('<!doctype html><body></body>');
    }
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  let browser;
  try {
    try {
      browser = await launch({args: process.getuid?.() === 0 ? ['--no-sandbox'] : []});
    } catch (error) {
      if (String(error.message).startsWith('no Chrome')) {
        t.skip('no headless Chrome on this machine');
        return;
      }
      throw error;
    }
    const tab = await browser.tab();
    await tab.navigate(`http://127.0.0.1:${server.address().port}/`);
    await tab.waitFor('document.readyState === "complete"');
    const result = await tab.evaluate(`(async () => {
      const {TiloClipEncoder, TiloFrameSeeker} = await import('/edit.mjs');
      const W = 64, H = 48, FPS = 10, FRAMES = 30;
      const encoder = new TiloClipEncoder(W, H, FPS, 2e6);
      while (encoder.state() === 'opening') await new Promise(r => setTimeout(r, 10));
      if (encoder.state() !== 'ready') return {error: encoder.takeReason()};
      for (let i = 0; i < FRAMES; i++) {
        const rgba = new Uint8Array(W * H * 4);
        for (let p = 0; p < W * H; p++) rgba.set([i * 8, 255 - i * 8, 128, 255], p * 4);
        encoder.encode(rgba, i, i % 20 === 0);
      }
      encoder.finish();
      while (encoder.state() === 'finishing') await new Promise(r => setTimeout(r, 10));
      if (encoder.state() !== 'done') return {error: encoder.takeReason()};
      const bytes = encoder.takeBytes();
      const url = URL.createObjectURL(new Blob([bytes], {type: 'video/webm'}));
      const seeker = new TiloFrameSeeker();
      seeker.open(url);
      const landed = [];
      for (const frame of [0, 7, 13, 29, 3, 3, 21]) {
        // Somewhere inside the frame, not at its middle.
        seeker.seek((frame + 0.2) / FPS, FPS);
        const deadline = Date.now() + 10000;
        while (seeker.shownFrame() !== frame && Date.now() < deadline) await new Promise(r => setTimeout(r, 5));
        const picture = seeker.picture(16, 16);
        landed.push({frame, shown: seeker.shownFrame(), red: picture[0], width: seeker.pictureWidth()});
      }
      return {codec: encoder.codec(), size: bytes.length, duration: seeker.element().duration, landed, error: seeker.takeError()};
    })()`);
    assert.equal(result.error ?? null, null, JSON.stringify(result));
    assert.ok(result.size > 100, 'the file has the frames in it');
    assert.ok(Math.abs(result.duration - 3) < 0.15, `the file plays for its three seconds: ${result.duration}`);
    for (const {frame, shown, red, width} of result.landed) {
      assert.equal(shown, frame);
      assert.equal(width, 16, 'the picture is scaled to fit');
      // Lossy, but nowhere near the neighbouring frames' 8 apart.
      assert.ok(Math.abs(red - frame * 8) <= 3, `frame ${frame} shows ${red}, not ${frame * 8} (${result.codec})`);
    }
    await tab.close();
  } finally {
    await browser?.close();
    await new Promise(done => server.close(done));
  }
});

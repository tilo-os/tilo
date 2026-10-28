import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {launch} from '../../../../tools/web/chrome.mjs';
import {
  CODECS, TiloPcmDecoder, frameAt, frameCentre, interleaveStereo, muxMatroska, opusHead, opusTrack,
  pickCodec, playbackRate, vint,
} from './tilo-media-edit.mjs';

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
  const file = muxMatroska({doc: 'webm', video: {codecId: 'V_VP9', codecPrivate: null, width: 64, height: 36, fps: 30}, chunks});
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

test('sound rides in a second track, its blocks interleaved with the pictures by time', () => {
  const chunks = [];
  for (let i = 0; i < 30; i++) chunks.push({track: 1, data: Uint8Array.of(1), timestamp: Math.round(i * 1e6 / 30), key: i === 0});
  // Opus packets of 20 ms, handed over after the pictures.
  for (let i = 0; i < 50; i++) chunks.push({track: 2, data: Uint8Array.of(2), timestamp: i * 20000, duration: 20000, key: true});
  const file = muxMatroska({
    doc: 'webm',
    video: {codecId: 'V_VP9', width: 64, height: 36, fps: 30},
    sound: opusTrack(null),
    chunks,
  });
  const segment = children(file, children(file, 0, file.length)[1].start, file.length);
  const tracks = children(file, segment[1].start, segment[1].end);
  assert.equal(tracks.length, 2);
  const sound = children(file, tracks[1].start, tracks[1].end);
  const codec = sound.find(e => e.id === 0x86);
  assert.equal(new TextDecoder().decode(file.subarray(codec.start, codec.end)), 'A_OPUS');
  const head = sound.find(e => e.id === 0x63A2);
  assert.equal(new TextDecoder().decode(file.subarray(head.start, head.start + 8)), 'OpusHead');
  assert.ok(sound.some(e => e.id === 0xE1), 'the sound track says its rate and channels');
  const blocks = segment.filter(e => e.id === 0x1F43B675)
    .flatMap(c => children(file, c.start, c.end).filter(e => e.id === 0xA3))
    .map(b => ({track: file[b.start] & 0x7F, payload: file[b.start + 4]}));
  assert.equal(blocks.filter(b => b.track === 2).length, 50);
  assert.ok(blocks.every(b => b.payload === b.track), 'each block is in its own track');
  // Interleaved: the sound does not all sit after the pictures.
  const firstSound = blocks.findIndex(b => b.track === 2);
  assert.ok(firstSound < 3, `sound starts with the pictures, at block ${firstSound}`);
});

test('an Opus header says its channels, rate and pre-skip; one the encoder wrote is kept', () => {
  const head = opusHead(2, 48000, 312);
  assert.equal(head.length, 19);
  assert.equal(head[9], 2);
  assert.equal(head[10] | (head[11] << 8), 312);
  const track = opusTrack(opusHead(1, 48000, 480));
  assert.equal(track.channels, 1, 'the encoder\'s own header wins');
  assert.equal(track.codecDelayNs, 10000000, '480 samples at 48 kHz');
});

test('a playhead\'s rate becomes an element rate browsers play, steady under jitter', () => {
  assert.equal(playbackRate(1), 1);
  assert.equal(playbackRate(1.07), 1, 'a measured 1x is 1x');
  assert.equal(playbackRate(7.6), 8);
  assert.equal(playbackRate(40), 16, 'browsers play at most 16x');
  assert.equal(playbackRate(1.5), 1.5, 'a clip at 150% plays at that');
  assert.equal(playbackRate(-2), 1, 'reverse is not something an element plays');
});

test('sound decodes to interleaved stereo, a mono source in both channels', async () => {
  const mono = {numberOfChannels: 1, length: 3, getChannelData: () => Float32Array.of(0.1, 0.2, 0.3)};
  assert.deepEqual([...interleaveStereo(mono)].map(v => Math.round(v * 10)), [1, 1, 2, 2, 3, 3]);
  const scope = {
    fetch: async () => ({arrayBuffer: async () => new ArrayBuffer(4)}),
    OfflineAudioContext: function OfflineAudioContext(channels, length, rate) {
      assert.equal(rate, 48000, 'decoded straight to the mixing rate');
      this.decodeAudioData = async () => mono;
    },
  };
  const decoder = new TiloPcmDecoder('blob:x', scope);
  assert.equal(decoder.state(), 'decoding');
  while (decoder.state() === 'decoding') await new Promise(r => setTimeout(r, 1));
  assert.equal(decoder.state(), 'ready');
  assert.equal(decoder.frames(), 3);
  assert.equal(decoder.samples(1, 5).length, 4, 'short at the end');
  const silent = new TiloPcmDecoder('blob:y', {...scope, OfflineAudioContext: function OfflineAudioContext() {
    this.decodeAudioData = async () => { throw new Error('no audio'); };
  }});
  while (silent.state() === 'decoding') await new Promise(r => setTimeout(r, 1));
  assert.equal(silent.state(), 'none', 'a source without sound mixes as silence');
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
      const {TiloClipEncoder, TiloFrameSeeker, TiloPcmDecoder} = await import('/edit.mjs');
      const W = 64, H = 48, FPS = 10, FRAMES = 30;
      const encoder = new TiloClipEncoder(W, H, FPS, 2e6, true);
      while (encoder.state() === 'opening') await new Promise(r => setTimeout(r, 10));
      if (encoder.state() !== 'ready') return {error: encoder.takeReason()};
      for (let i = 0; i < FRAMES; i++) {
        const rgba = new Uint8Array(W * H * 4);
        for (let p = 0; p < W * H; p++) rgba.set([i * 8, 255 - i * 8, 128, 255], p * 4);
        encoder.encode(rgba, i, i % 20 === 0);
        // A frame's worth of a 440 Hz tone, as the export hands it over.
        const from = i * 4800, sound = new Float32Array(4800 * 2);
        for (let s = 0; s < 4800; s++) sound[s * 2] = sound[s * 2 + 1] = 0.5 * Math.sin(2 * Math.PI * 440 * (from + s) / 48000);
        encoder.encodeSound(sound, from);
      }
      encoder.finish();
      while (encoder.state() === 'finishing') await new Promise(r => setTimeout(r, 10));
      if (encoder.state() !== 'done') return {error: encoder.takeReason()};
      const bytes = encoder.takeBytes();
      const url = URL.createObjectURL(new Blob([bytes], {type: 'video/webm'}));
      // The sound, read back by the page's own decoder from the file.
      const pcm = new TiloPcmDecoder(url);
      while (pcm.state() === 'decoding') await new Promise(r => setTimeout(r, 10));
      const mid = pcm.samples(48000, 4800);
      let energy = 0;
      for (const v of mid) energy += v * v;
      const sound = {hasSound: encoder.hasSound(), state: pcm.state(), seconds: pcm.frames() / 48000, rms: Math.sqrt(energy / Math.max(1, mid.length))};
      // A still picture shows as frame 0 of a seeker, whatever time is asked.
      const canvas = new OffscreenCanvas(8, 4);
      const paint = canvas.getContext('2d');
      paint.fillStyle = 'rgb(200, 10, 20)';
      paint.fillRect(0, 0, 8, 4);
      const still = new TiloFrameSeeker();
      still.openImage(URL.createObjectURL(await canvas.convertToBlob({type: 'image/png'})));
      still.seek(12.3, 30);
      for (let n = 0; n < 500 && still.shownFrame() !== 0; n++) await new Promise(r => setTimeout(r, 5));
      const stillPicture = still.picture(64, 64);
      const stillResult = {shown: still.shownFrame(), width: still.pictureWidth(), red: stillPicture[0]};
      still.close();
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
      // Played along at twice the speed, the element plays at twice the speed.
      seeker.follow(0.5, FPS, 2.03);
      const followRate = seeker.element().playbackRate;
      return {codec: encoder.codec(), size: bytes.length, duration: seeker.element().duration, landed, sound, still: stillResult, followRate, error: seeker.takeError()};
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
    assert.ok(result.sound.hasSound, 'the browser encodes Opus');
    assert.equal(result.sound.state, 'ready', 'the file\'s sound decodes');
    assert.ok(Math.abs(result.sound.seconds - 3) < 0.1, `the sound runs the clip's three seconds: ${result.sound.seconds}`);
    // A 0.5 sine is 0.354 RMS; Opus keeps it close.
    assert.ok(Math.abs(result.sound.rms - 0.354) < 0.05, `the tone survives: ${result.sound.rms}`);
    assert.deepEqual(result.still, {shown: 0, width: 8, red: 200});
    assert.equal(result.followRate, 2);
    await tab.close();
  } finally {
    await browser?.close();
    await new Promise(done => server.close(done));
  }
});

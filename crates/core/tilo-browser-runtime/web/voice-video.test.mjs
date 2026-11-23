import test from 'node:test';
import assert from 'node:assert/strict';

import {LAYER_CODES, SOURCE_CODES, createPictureEncoder, createPictureInbox, layerOfCode, pictureObject, sourceOfCode} from './voice-video.mjs';
import {decodeVideoObject} from './video-profile.mjs';

const drain = async () => { for (let index = 0; index < 20; index++) await Promise.resolve(); };

/** A browser stand-in: a camera that yields frames, and a WebCodecs encoder
 * that codes each one as a keyframe or not. */
function fakeBrowser() {
  const encoders = [];
  let push;
  class VideoFrame {
    constructor(source, {timestamp} = {}) { this.displayWidth = source.width ?? source.displayWidth; this.displayHeight = source.height ?? source.displayHeight; this.timestamp = timestamp ?? 0; }
    close() { this.closed = true; }
  }
  class VideoEncoder {
    constructor({output}) { this.output = output; this.encodeQueueSize = 0; this.state = 'unconfigured'; encoders.push(this); }
    configure(config) { this.config = config; this.state = 'configured'; }
    encode(frame, {keyFrame}) {
      const data = Uint8Array.of(0, 0, 0, 1, keyFrame ? 0x65 : 0x41);
      this.output({type: keyFrame ? 'key' : 'delta', timestamp: frame.timestamp, byteLength: data.byteLength, copyTo: target => target.set(data)});
    }
    close() { this.state = 'closed'; }
  }
  class OffscreenCanvas {
    constructor(width, height) { this.width = width; this.height = height; }
    getContext() { return {drawImage() {}}; }
  }
  class MediaStreamTrackProcessor {
    constructor() {
      this.readable = new ReadableStream({start(controller) { push = frame => controller.enqueue(frame); }});
    }
  }
  const stopped = [];
  const stream = {getVideoTracks: () => [{addEventListener() {}}], getTracks: () => [{stop() { stopped.push(true); }}]};
  const camera = (n, width = 1280, height = 720) => new VideoFrame({width, height}, {timestamp: n * 33_333});
  return {environment: {VideoFrame, VideoEncoder, OffscreenCanvas, MediaStreamTrackProcessor}, encoders, stream, stopped, frame: (n, width, height) => push(camera(n, width, height))};
}

test('a layer is encoded only while a device shows it, at the layer size, and a device that fell behind gets a keyframe', async () => {
  const browser = fakeBrowser();
  const wanted = new Set(['high']);
  const pictures = [];
  const encoder = createPictureEncoder({source: 'camera', stream: browser.stream, environment: browser.environment, wanted: layer => wanted.has(layer), onPicture: picture => pictures.push(picture)});
  browser.frame(0);
  await drain();
  assert.equal(browser.encoders.length, 1, 'only the shown layer is encoded');
  assert.deepEqual([browser.encoders[0].config.width, browser.encoders[0].config.height], [1280, 720]);
  assert.equal(pictures.length, 1);
  assert.equal(pictures[0].keyframe, true, 'a layer starts at a keyframe');
  assert.equal(pictures[0].layer, 'high');
  assert.equal(pictures[0].sequence, 0);
  assert.equal(pictures[0].timestampUs, 0);
  assert.deepEqual([...pictures[0].payload], [0, 0, 0, 1, 0x65]);

  browser.frame(1);
  await drain();
  assert.equal(pictures.length, 2);
  assert.equal(pictures[1].keyframe, false);
  assert.equal(pictures[1].sequence, 1);

  // A device falls behind and asks for a keyframe: the next picture is one.
  encoder.keyframe();
  browser.frame(2);
  await drain();
  assert.equal(pictures.at(-1).keyframe, true, 'the asked-for picture starts over');

  wanted.clear();
  browser.frame(3);
  await drain();
  assert.equal(browser.encoders[0].state, 'closed', 'nobody shows the layer any more');
  encoder.stop();
});

test('a screen fits its shown layer and a source this profile has none of is refused', async () => {
  const browser = fakeBrowser();
  const pictures = [];
  const encoder = createPictureEncoder({source: 'screen', stream: browser.stream, environment: browser.environment, wanted: layer => layer === 'low', onPicture: picture => pictures.push(picture)});
  browser.frame(0, 1920, 1080);
  await drain();
  assert.deepEqual([browser.encoders[0].config.width, browser.encoders[0].config.height], [320, 180]);
  assert.throws(() => createPictureEncoder({source: 'hologram', stream: browser.stream}), RangeError);
  encoder.stop();
});

test('stopping an encoder releases the capture unless it moves to another call', async () => {
  const browser = fakeBrowser();
  const kept = createPictureEncoder({source: 'camera', stream: browser.stream, environment: browser.environment, wanted: () => true});
  kept.stop({keepStream: true});
  assert.deepEqual(browser.stopped, []);
  const ended = createPictureEncoder({source: 'camera', stream: browser.stream, environment: browser.environment, wanted: () => true});
  ended.stop();
  assert.deepEqual(browser.stopped, [true]);
});

test('the page speaks the call\'s codes: camera and screen, and the layers in their order', () => {
  assert.deepEqual(SOURCE_CODES, {camera: 0, screen: 1});
  assert.deepEqual(LAYER_CODES, {low: 0, mid: 1, high: 2});
  assert.equal(sourceOfCode(1), 'screen');
  assert.equal(sourceOfCode(7), null);
  assert.equal(layerOfCode(2), 'high');
  assert.equal(layerOfCode(9), null);
});

test('a picture this page keeps is the tilo-video/1 object the call would seal', () => {
  const object = pictureObject({source: 'camera', layer: 'mid', sequence: 7, keyframe: true, timestampUs: 1234, width: 640, height: 360, payload: Uint8Array.of(9, 8, 7)});
  const decoded = decodeVideoObject(object);
  assert.equal(decoded.keyframe, true);
  assert.equal(decoded.layer, 'mid');
  assert.equal(decoded.sequence, 7n);
  assert.equal(decoded.timestampUs, 1234n);
  assert.deepEqual([...decoded.payload], [9, 8, 7]);
});

test('the inbox holds pictures by cursor, starts a fallen-behind stream at its next keyframe, and bounds itself', () => {
  const inbox = createPictureInbox({maxBytes: 8});
  const picture = (sequence, keyframe, size = 4) => ({person: 'p'.repeat(64), stream: 'cam', source: 'camera', sequence, object: new Uint8Array(size).fill(sequence), keyframe});
  inbox.admit(picture(1, true), true);
  inbox.admit(picture(2, false), false);
  const taken = inbox.take(0);
  assert.equal(taken.pictures.length, 2);
  assert.equal(taken.cursor, 2);
  inbox.behind('cam', 'camera');
  inbox.admit(picture(3, false), false);
  assert.equal(inbox.take(taken.cursor).pictures.length, 0, 'nothing before a keyframe is drawn');
  inbox.admit(picture(4, true), true);
  assert.equal(inbox.take(taken.cursor).pictures[0].sequence, 4);
  // Beyond the byte budget the oldest picture goes, and its stream waits for
  // a keyframe: what the app decodes never skips one.
  const other = key => ({person: 'q'.repeat(64), stream: key, source: 'camera', object: new Uint8Array(4)});
  inbox.admit(other('a'), true);
  inbox.admit(other('b'), true);
  inbox.admit(other('c'), true);
  const bounded = inbox.take(0, 1000);
  assert.deepEqual(bounded.pictures.map(entry => entry.stream), ['b', 'c']);
  assert.equal(typeof bounded.pictures[0].object, 'string', 'objects travel as base64');
});

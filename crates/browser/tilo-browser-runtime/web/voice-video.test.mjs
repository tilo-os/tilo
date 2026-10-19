import test from 'node:test';
import assert from 'node:assert/strict';

import {createPictureInbox, publishPictures} from './voice-video.mjs';
import {decodeVideoObject} from './video-profile.mjs';

const drain = async () => { for (let index = 0; index < 20; index++) await Promise.resolve(); };

/** A `@moq/net` stand-in: tracks whose demand the test sets, groups that record frames. */
function fakeMoq() {
  const tracks = new Map();
  let used = new Set();
  const moq = {Time: {Timescale: {MICRO: 'us'}, Timestamp: class { constructor(value) { this.value = value; } }}};
  const broadcast = {
    createTrack(name) {
      const track = {
        name,
        groups: [],
        used: {peek: () => used.has(name)},
        appendGroup() { const group = {frames: [], writeFrame(frame) { group.frames.push(frame); }, close() {}}; track.groups.push(group); return group; },
        close() { track.closed = true; },
      };
      tracks.set(name, track);
      return track;
    },
    removeTrack(name) { tracks.get(name).removed = true; },
  };
  return {moq, broadcast, tracks, demand(names) { used = new Set(names); }};
}

/** A browser stand-in: a camera that yields `frames`, and a WebCodecs encoder that keys on demand. */
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
  const camera = (n) => new VideoFrame({width: 1280, height: 720}, {timestamp: n * 33_333});
  return {environment: {VideoFrame, VideoEncoder, OffscreenCanvas, MediaStreamTrackProcessor}, encoders, stream, stopped, frame: n => push(camera(n))};
}

test('a camera layer is encoded only while someone shows it, and each group starts at a keyframe', async () => {
  const {moq, broadcast, tracks, demand} = fakeMoq();
  const browser = fakeBrowser();
  const published = publishPictures({moq, broadcast, source: 'camera', stream: browser.stream, environment: browser.environment});
  assert.deepEqual([...tracks.keys()], ['camera/low', 'camera/mid', 'camera/high']);
  browser.frame(0);
  await drain();
  assert.equal(browser.encoders.length, 0, 'nobody shows the camera: nothing is encoded');

  demand(['camera/mid']);
  browser.frame(1);
  browser.frame(2);
  await drain();
  assert.equal(browser.encoders.length, 1);
  assert.deepEqual([browser.encoders[0].config.width, browser.encoders[0].config.height], [640, 360]);
  const [group] = tracks.get('camera/mid').groups;
  const objects = group.frames.map(frame => decodeVideoObject(frame.payload));
  assert.equal(objects[0].keyframe, true, 'the layer starts at a keyframe');
  assert.equal(objects[1].keyframe, false);
  assert.deepEqual(objects.map(object => object.layer), ['mid', 'mid']);
  assert.equal(tracks.get('camera/low').groups.length, 0);

  // Nobody shows it any more: its encoder closes.
  demand([]);
  browser.frame(3);
  await drain();
  assert.equal(browser.encoders[0].state, 'closed');

  published.stop();
  assert.ok([...tracks.values()].every(track => track.removed && track.closed));
  assert.equal(browser.stopped.length, 1, 'the camera stops');
});

test('this person sees their own camera at their tile\'s layer without anyone else showing it', async () => {
  const {moq, broadcast, tracks} = fakeMoq();
  const browser = fakeBrowser();
  const own = [];
  const published = publishPictures({moq, broadcast, source: 'camera', stream: browser.stream, environment: browser.environment,
    ownLayer: () => 'low', own: (layer, object, keyframe) => own.push({layer, object, keyframe})});
  browser.frame(0);
  await drain();
  assert.equal(own.length, 1);
  assert.equal(own[0].keyframe, true);
  assert.deepEqual([decodeVideoObject(own[0].object).width, decodeVideoObject(own[0].object).height], [320, 180]);
  assert.equal(tracks.get('camera/low').groups.length, 0, 'nothing went out for it');
  published.stop({keepStream: true});
  assert.equal(browser.stopped.length, 0, 'a new carrier publishes the same camera again');
});

test('the inbox hands pictures over by cursor, in base64, and a stream that falls behind waits for a keyframe', () => {
  const inbox = createPictureInbox({maxBytes: 100});
  const picture = (stream, size) => ({person: 'ab'.repeat(32), stream, source: 'camera', object: new Uint8Array(size).fill(1)});
  inbox.admit(picture('slow', 40), true);
  inbox.admit(picture('slow', 40), false);
  inbox.admit(picture('fast', 40), true);
  const {pictures, cursor} = inbox.take(0);
  assert.deepEqual(pictures.map(p => p.stream), ['fast'], 'the oldest stream gave way whole');
  assert.equal(typeof pictures[0].object, 'string');
  assert.equal(inbox.take(cursor).pictures.length, 0);
  inbox.admit(picture('slow', 10), false);
  assert.equal(inbox.take(cursor).pictures.length, 0, 'not without its keyframe');
  inbox.admit(picture('slow', 10), true);
  assert.equal(inbox.take(cursor).pictures.length, 1);
});

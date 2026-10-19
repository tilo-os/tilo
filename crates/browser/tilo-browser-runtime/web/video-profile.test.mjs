import test from 'node:test';
import assert from 'node:assert/strict';

import {SOURCES, base64Of, decodeVideoObject, encodeVideoObject, fitted, videoTrack} from './video-profile.mjs';

test('video objects match the tilo-video/1 layout the native carrier reads', () => {
  const encoded = encodeVideoObject({keyframe: true, layer: 'high', sequence: 7, timestampUs: 1000, width: 640, height: 360, payload: Uint8Array.of(0, 0, 1)});
  // The same bytes the native runtime's own test frames (runtime-collab
  // `a_picture_names_its_coded_unit_and_refuses_one_that_does_not_hold_together`).
  const expected = Uint8Array.from([
    1, 1, 1, 2,
    7, 0, 0, 0, 0, 0, 0, 0,
    0xe8, 0x03, 0, 0, 0, 0, 0, 0,
    0x80, 0x02, 0x68, 0x01,
    3, 0, 0, 0,
    0, 0, 0, 0,
    0, 0, 1,
  ]);
  assert.deepEqual(encoded, expected);
  const decoded = decodeVideoObject(encoded);
  assert.equal(decoded.keyframe, true);
  assert.equal(decoded.codec, 'h264');
  assert.equal(decoded.layer, 'high');
  assert.equal(decoded.sequence, 7n);
  assert.deepEqual([decoded.width, decoded.height], [640, 360]);
  assert.deepEqual(decoded.payload, Uint8Array.of(0, 0, 1));
  const truncated = encoded.slice(0, -1);
  assert.throws(() => decodeVideoObject(truncated), 'the length names the payload');
  const versioned = encoded.slice();
  versioned[0] = 2;
  assert.throws(() => decodeVideoObject(versioned));
});

test('a layer keeps its source shape within its box, and a screen has no middle layer', () => {
  assert.deepEqual(fitted(1280, 720, SOURCES.camera.low), [320, 180]);
  assert.deepEqual(fitted(1920, 1200, SOURCES.screen.high), [1728, 1080]);
  assert.deepEqual(fitted(1366, 768, SOURCES.camera.low), [320, 178]);
  assert.deepEqual(fitted(800, 600, SOURCES.screen.high), [800, 600]);
  assert.equal(videoTrack('camera', 'mid'), 'camera/mid');
  assert.throws(() => videoTrack('screen', 'mid'));
});

test('pictures travel the local media lane in base64', () => {
  assert.equal(base64Of(Uint8Array.of(1, 0, 1, 1, 0xff, 0x80)), 'AQABAf+A');
});

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  VOICE_FRAME_SAMPLES,
  VOICE_HEADER_BYTES,
  VOICE_MAX_OBJECT_BYTES,
  VOICE_PROFILE_VERSION,
  decodeVoiceObject,
  encodeVoiceObject,
} from './voice-profile.mjs';

test('voice objects encode to and decode from the tilo-voice/1 wire fixture', () => {
  const expected = Uint8Array.from([
    0x01, 0x00, 0x00, 0x00,
    0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
    0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11,
    0xc0, 0x03,
    0x80, 0xbb, 0x00, 0x00,
    0x01, 0x00,
    0x03, 0x00, 0x00, 0x00,
    0xf8, 0xff, 0xfe,
  ]);

  const encoded = encodeVoiceObject({
    sequence: 0x0102030405060708n,
    timestamp: 0x1112131415161718n,
    duration: 960,
    payload: Uint8Array.of(0xf8, 0xff, 0xfe),
  });

  assert.deepEqual(encoded, expected);
  assert.deepEqual(decodeVoiceObject(expected), {
    sequence: 0x0102030405060708n,
    timestamp: 0x1112131415161718n,
    duration: 960,
    payload: Uint8Array.of(0xf8, 0xff, 0xfe),
  });
  assert.equal(VOICE_PROFILE_VERSION, 1);
  assert.equal(VOICE_HEADER_BYTES, 32);
  assert.equal(VOICE_MAX_OBJECT_BYTES, 4096);
  assert.equal(VOICE_FRAME_SAMPLES, 960);
});

test('voice object encoding enforces u64 fields and the fixed mono 48 kHz frame profile', () => {
  const payload = Uint8Array.of(0xf8);
  const valid = {sequence: 0n, timestamp: (1n << 64n) - 1n, duration: 960, payload};
  const decoded = decodeVoiceObject(encodeVoiceObject(valid));
  assert.equal(decoded.sequence, 0n);
  assert.equal(decoded.timestamp, (1n << 64n) - 1n);

  for (const sequence of [-1n, 1n << 64n, 1]) {
    assert.throws(() => encodeVoiceObject({...valid, sequence}), /sequence/i);
  }
  for (const timestamp of [-1n, 1n << 64n, 1]) {
    assert.throws(() => encodeVoiceObject({...valid, timestamp}), /timestamp/i);
  }
  for (const duration of [0, 959, 961, 960n]) {
    assert.throws(() => encodeVoiceObject({...valid, duration}), /duration/i);
  }
  assert.throws(() => encodeVoiceObject({...valid, payload: new ArrayBuffer(1)}), /payload/i);
});

test('voice object encoding requires a nonempty payload within the total object limit', () => {
  const base = {sequence: 1n, timestamp: 2n, duration: 960};
  assert.throws(() => encodeVoiceObject({...base, payload: new Uint8Array()}), /payload/i);
  assert.equal(
    encodeVoiceObject({...base, payload: new Uint8Array(VOICE_MAX_OBJECT_BYTES - VOICE_HEADER_BYTES)}).byteLength,
    VOICE_MAX_OBJECT_BYTES,
  );
  assert.throws(
    () => encodeVoiceObject({...base, payload: new Uint8Array(VOICE_MAX_OBJECT_BYTES - VOICE_HEADER_BYTES + 1)}),
    /4096|size|large/i,
  );
});

test('voice object decoding rejects unsupported and nonzero reserved header fields', () => {
  const valid = encodeVoiceObject({
    sequence: 1n,
    timestamp: 2n,
    duration: 960,
    payload: Uint8Array.of(0xf8),
  });
  for (const [offset, value, pattern] of [
    [0, 2, /version/i],
    [1, 1, /flags/i],
    [2, 1, /reserved/i],
    [3, 1, /reserved/i],
    [27, 1, /reserved/i],
  ]) {
    const malformed = valid.slice();
    malformed[offset] = value;
    assert.throws(() => decodeVoiceObject(malformed), pattern);
  }
});

test('voice object decoding enforces the fixed frame, sample rate, and channel profile', () => {
  const valid = encodeVoiceObject({
    sequence: 1n,
    timestamp: 2n,
    duration: 960,
    payload: Uint8Array.of(0xf8),
  });
  for (const [offset, write, pattern] of [
    [20, view => view.setUint16(20, 959, true), /duration/i],
    [22, view => view.setUint32(22, 44100, true), /sample rate/i],
    [26, view => view.setUint8(26, 2), /channels/i],
  ]) {
    const malformed = valid.slice();
    const view = new DataView(malformed.buffer, malformed.byteOffset, malformed.byteLength);
    write(view);
    assert.throws(() => decodeVoiceObject(malformed), pattern, `field at byte ${offset}`);
  }
});

test('voice object decoding rejects invalid buffers, empty payloads, and inexact lengths', () => {
  const valid = encodeVoiceObject({
    sequence: 1n,
    timestamp: 2n,
    duration: 960,
    payload: Uint8Array.of(0xf8, 0xff, 0xfe),
  });
  assert.throws(() => decodeVoiceObject(valid.buffer), /Uint8Array/i);
  assert.throws(() => decodeVoiceObject(valid.subarray(0, VOICE_HEADER_BYTES - 1)), /header|short/i);
  assert.throws(() => decodeVoiceObject(new Uint8Array(VOICE_MAX_OBJECT_BYTES + 1)), /4096|size|large/i);

  const empty = valid.slice(0, VOICE_HEADER_BYTES);
  new DataView(empty.buffer).setUint32(28, 0, true);
  assert.throws(() => decodeVoiceObject(empty), /payload|empty/i);

  for (const declaredLength of [2, 4]) {
    const malformed = valid.slice();
    new DataView(malformed.buffer).setUint32(28, declaredLength, true);
    assert.throws(() => decodeVoiceObject(malformed), /length/i);
  }
  const trailing = new Uint8Array(valid.byteLength + 1);
  trailing.set(valid);
  assert.throws(() => decodeVoiceObject(trailing), /length/i);
});

test('voice object decoding accepts the maximum exact object size', () => {
  const encoded = encodeVoiceObject({
    sequence: (1n << 64n) - 1n,
    timestamp: 0n,
    duration: 960,
    payload: new Uint8Array(VOICE_MAX_OBJECT_BYTES - VOICE_HEADER_BYTES),
  });
  const decoded = decodeVoiceObject(encoded);
  assert.equal(decoded.sequence, (1n << 64n) - 1n);
  assert.equal(decoded.payload.byteLength, VOICE_MAX_OBJECT_BYTES - VOICE_HEADER_BYTES);
});

test('voice object encode and decode results own their payload bytes', () => {
  const payload = Uint8Array.of(0xf8, 0xff);
  const encoded = encodeVoiceObject({sequence: 1n, timestamp: 2n, duration: 960, payload});
  payload[0] = 0;
  assert.equal(encoded[VOICE_HEADER_BYTES], 0xf8);

  const source = Buffer.from(encoded);
  const decoded = decodeVoiceObject(source);
  source[VOICE_HEADER_BYTES] = 0;
  assert.equal(decoded.payload[0], 0xf8);
  decoded.payload[1] = 0;
  assert.equal(source[VOICE_HEADER_BYTES + 1], 0xff);
  assert.equal(decoded.payload.constructor, Uint8Array);
});

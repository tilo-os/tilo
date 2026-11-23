import test from 'node:test';
import assert from 'node:assert/strict';
import {hex, unhex} from './hex.mjs';

test('hex and unhex round-trip every byte value and accept mixed case', () => {
  const all = Uint8Array.from({length: 256}, (_, index) => index);
  const encoded = hex(all);
  assert.equal(encoded.length, 512);
  assert.deepEqual([...unhex(encoded)], [...all]);
  assert.deepEqual([...unhex(encoded.toUpperCase())], [...all]);
  assert.deepEqual([...unhex('')], []);
  assert.deepEqual([...unhex('00ff10')], [0, 255, 16]);
  assert.deepEqual([...unhex('0aBcDeF9')], [0x0a, 0xbc, 0xde, 0xf9]);
});

test('unhex refuses odd lengths, non-hex digits and non-strings', () => {
  for (const bad of ['abc', 'zz', '1g', '0', '0g', ' 00', '00 ', '00\n', '0x01', null, undefined, 123]) {
    assert.throws(() => unhex(bad), RangeError, String(bad));
  }
});

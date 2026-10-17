import test from 'node:test';
import assert from 'node:assert/strict';
import {qrMatrix, qrSvg} from './qr.mjs';

const rows = matrix => matrix.map(row => row.map(dark => (dark ? '1' : '0')).join(''));

test('a short text encodes as the reference encoders do it, module for module', () => {
  // "HELLO", byte mode, level M, mask 0: the matrix python-qrcode makes.
  assert.deepEqual(rows(qrMatrix('HELLO', {mask: 0})), [
    '111111100111001111111', '100000101011001000001', '101110100010001011101', '101110100011001011101',
    '101110101010101011101', '100000100110101000001', '111111101010101111111', '000000000101100000000',
    '101010100101000010010', '101010000110001000010', '000001100110100011111', '101011010110001000010',
    '011001110110101010100', '000000001011010100110', '111111100001011100111', '100000100011110110000',
    '101110101011011100111', '101110100100001100110', '101110101010100010101', '100000100110001010010',
    '111111101010101100111',
  ]);
});

test('a pairing link takes the smallest version that holds it, with all three finders', () => {
  const link = `https://tilo.coffee/#/@eu1.tilo.coffee/w/0123456789abcdef?pair=${'ab'.repeat(48)}`;
  const matrix = qrMatrix(link);
  // 159 bytes: version 9 at level M (version 8 holds 150), 53 modules a side.
  assert.equal(matrix.length, 53);
  const finder = ['1111111', '1000001', '1011101', '1011101', '1011101', '1000001', '1111111'];
  const at = (x, y) => rows(matrix).slice(y, y + 7).map(row => row.slice(x, x + 7));
  for (const [x, y] of [[0, 0], [46, 0], [0, 46]]) assert.deepEqual(at(x, y), finder);
  assert.throws(() => qrMatrix('x'.repeat(3000)), RangeError);
});

test('the SVG has the quiet zone and one dark path', () => {
  const svg = qrSvg('HELLO');
  assert.match(svg, /^<svg [^>]*viewBox="0 0 29 29"/);
  assert.equal(svg.match(/<path /g).length, 1);
});

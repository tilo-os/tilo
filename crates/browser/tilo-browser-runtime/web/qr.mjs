/**
 * A QR code of a short text (a link), for a phone's camera to read: byte
 * mode, error correction level M, the smallest version that holds it. The
 * page draws it as an SVG (`qrSvg`), so a pairing link shows as a code
 * without the runtime, which the native shell's `widget::qr` paints.
 *
 * Follows ISO/IEC 18004 as Project Nayuki's reference encoder lays it out:
 * data and Reed-Solomon codewords in interleaved blocks, function patterns,
 * then the mask with the lowest penalty.
 */

// Per version 1..40 (index 0 unused), level M.
const ECC_PER_BLOCK = [-1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28,
  28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28];
const BLOCKS = [-1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28,
  29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49];
/** Level M's two format bits. */
const LEVEL_M = 0;

const rawModules = version => {
  let result = (16 * version + 128) * version + 64;
  if (version >= 2) {
    const align = Math.floor(version / 7) + 2;
    result -= (25 * align - 10) * align - 55;
    if (version >= 7) result -= 36;
  }
  return result;
};
const dataCodewords = version => Math.floor(rawModules(version) / 8) - ECC_PER_BLOCK[version] * BLOCKS[version];

const gfMultiply = (x, y) => {
  let z = 0;
  for (let i = 7; i >= 0; i--) {
    z = (z << 1) ^ ((z >>> 7) * 0x11d);
    z ^= ((y >>> i) & 1) * x;
  }
  return z;
};
function rsDivisor(degree) {
  const result = new Array(degree).fill(0);
  result[degree - 1] = 1;
  let root = 1;
  for (let i = 0; i < degree; i++) {
    for (let j = 0; j < degree; j++) {
      result[j] = gfMultiply(result[j], root);
      if (j + 1 < degree) result[j] ^= result[j + 1];
    }
    root = gfMultiply(root, 0x02);
  }
  return result;
}
function rsRemainder(data, divisor) {
  const result = divisor.map(() => 0);
  for (const byte of data) {
    const factor = byte ^ result.shift();
    result.push(0);
    divisor.forEach((coefficient, i) => { result[i] ^= gfMultiply(coefficient, factor); });
  }
  return result;
}

/** The data codewords of `bytes` at `version`: mode, length, data, padding. */
function codewords(bytes, version) {
  const bits = [];
  const put = (value, length) => { for (let i = length - 1; i >= 0; i--) bits.push((value >>> i) & 1); };
  put(0b0100, 4);
  put(bytes.length, version <= 9 ? 8 : 16);
  for (const byte of bytes) put(byte, 8);
  const capacity = dataCodewords(version) * 8;
  put(0, Math.min(4, capacity - bits.length));
  put(0, (8 - bits.length % 8) % 8);
  for (let pad = 0xec; bits.length < capacity; pad ^= 0xec ^ 0x11) put(pad, 8);
  const result = [];
  for (let i = 0; i < bits.length; i += 8) result.push(bits.slice(i, i + 8).reduce((byte, bit) => (byte << 1) | bit, 0));
  return result;
}

/** `data` split into blocks, each with its error correction, interleaved. */
function interleaved(data, version) {
  const blocks = BLOCKS[version];
  const eccLength = ECC_PER_BLOCK[version];
  const raw = Math.floor(rawModules(version) / 8);
  const shortBlocks = blocks - raw % blocks;
  const shortLength = Math.floor(raw / blocks);
  const divisor = rsDivisor(eccLength);
  const all = [];
  for (let i = 0, k = 0; i < blocks; i++) {
    const block = data.slice(k, k + shortLength - eccLength + (i < shortBlocks ? 0 : 1));
    k += block.length;
    const ecc = rsRemainder(block, divisor);
    if (i < shortBlocks) block.push(0);
    all.push(block.concat(ecc));
  }
  const result = [];
  for (let i = 0; i < all[0].length; i++) {
    all.forEach((block, j) => { if (i !== shortLength - eccLength || j >= shortBlocks) result.push(block[i]); });
  }
  return result;
}

const MASKS = [
  (x, y) => (x + y) % 2 === 0,
  (_x, y) => y % 2 === 0,
  (x, _y) => x % 3 === 0,
  (x, y) => (x + y) % 3 === 0,
  (x, y) => (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0,
  (x, y) => x * y % 2 + x * y % 3 === 0,
  (x, y) => (x * y % 2 + x * y % 3) % 2 === 0,
  (x, y) => ((x + y) % 2 + x * y % 3) % 2 === 0,
];

/** How badly a matrix reads: runs, blocks, finder look-alikes, balance. */
function penalty(grid) {
  const size = grid.length;
  let result = 0;
  const lines = [];
  for (let i = 0; i < size; i++) lines.push(grid[i], grid.map(row => row[i]));
  const finderLike = [true, false, true, true, true, false, true];
  for (const line of lines) {
    for (let start = 0; start < size;) {
      let end = start;
      while (end < size && line[end] === line[start]) end++;
      if (end - start >= 5) result += 3 + end - start - 5;
      start = end;
    }
    // A finder's 1:1:3:1:1 with four light modules on either side
    // (outside the code counts as light).
    const at = i => i >= 0 && i < size && line[i];
    for (let i = 0; i + 7 <= size; i++) {
      if (!finderLike.every((dark, k) => line[i + k] === dark)) continue;
      const lightBefore = [1, 2, 3, 4].every(k => !at(i - k));
      const lightAfter = [7, 8, 9, 10].every(k => !at(i + k));
      if (lightBefore || lightAfter) result += 40;
    }
  }
  let dark = 0;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (grid[y][x]) dark++;
      if (x + 1 < size && y + 1 < size) {
        const color = grid[y][x];
        if (color === grid[y][x + 1] && color === grid[y + 1][x] && color === grid[y + 1][x + 1]) result += 3;
      }
    }
  }
  const total = size * size;
  result += (Math.ceil(Math.abs(dark * 20 - total * 10) / total) - 1) * 10;
  return result;
}

/**
 * The QR code of `text` as rows of booleans (dark is `true`), without the
 * quiet zone. `mask` forces one of the eight masks (else the best).
 * Throws when the text does not fit a version 40 code.
 */
export function qrMatrix(text, {mask = null} = {}) {
  const bytes = [...new TextEncoder().encode(text)];
  let version = 1;
  while (version <= 40 && 4 + (version <= 9 ? 8 : 16) + bytes.length * 8 > dataCodewords(version) * 8) version++;
  if (version > 40) throw new RangeError('too long for a QR code');
  const size = version * 4 + 17;
  const modules = Array.from({length: size}, () => new Array(size).fill(false));
  const reserved = Array.from({length: size}, () => new Array(size).fill(false));
  const set = (x, y, dark) => { modules[y][x] = dark; reserved[y][x] = true; };

  for (let i = 0; i < size; i++) { set(6, i, i % 2 === 0); set(i, 6, i % 2 === 0); }
  for (const [cx, cy] of [[3, 3], [size - 4, 3], [3, size - 4]]) {
    for (let dy = -4; dy <= 4; dy++) {
      for (let dx = -4; dx <= 4; dx++) {
        const distance = Math.max(Math.abs(dx), Math.abs(dy));
        const x = cx + dx;
        const y = cy + dy;
        if (x >= 0 && x < size && y >= 0 && y < size) set(x, y, distance !== 2 && distance !== 4);
      }
    }
  }
  const aligns = [];
  if (version > 1) {
    const count = Math.floor(version / 7) + 2;
    const step = Math.floor((version * 8 + count * 3 + 5) / (count * 4 - 4)) * 2;
    aligns.push(6);
    for (let position = size - 7; aligns.length < count; position -= step) aligns.splice(1, 0, position);
  }
  aligns.forEach((ax, i) => aligns.forEach((ay, j) => {
    if ((i === 0 && j === 0) || (i === 0 && j === aligns.length - 1) || (i === aligns.length - 1 && j === 0)) return;
    for (let dy = -2; dy <= 2; dy++) {
      for (let dx = -2; dx <= 2; dx++) set(ax + dx, ay + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
    }
  }));
  const drawFormat = chosen => {
    const data = (LEVEL_M << 3) | chosen;
    let remainder = data;
    for (let i = 0; i < 10; i++) remainder = (remainder << 1) ^ ((remainder >>> 9) * 0x537);
    const bits = ((data << 10) | remainder) ^ 0x5412;
    const bit = i => ((bits >>> i) & 1) === 1;
    for (let i = 0; i <= 5; i++) set(8, i, bit(i));
    set(8, 7, bit(6));
    set(8, 8, bit(7));
    set(7, 8, bit(8));
    for (let i = 9; i < 15; i++) set(14 - i, 8, bit(i));
    for (let i = 0; i < 8; i++) set(size - 1 - i, 8, bit(i));
    for (let i = 8; i < 15; i++) set(8, size - 15 + i, bit(i));
    set(8, size - 8, true);
  };
  drawFormat(0);
  if (version >= 7) {
    let remainder = version;
    for (let i = 0; i < 12; i++) remainder = (remainder << 1) ^ ((remainder >>> 11) * 0x1f25);
    const bits = (version << 12) | remainder;
    for (let i = 0; i < 18; i++) {
      const dark = ((bits >>> i) & 1) === 1;
      const a = size - 11 + i % 3;
      const b = Math.floor(i / 3);
      set(a, b, dark);
      set(b, a, dark);
    }
  }

  const data = interleaved(codewords(bytes, version), version);
  let i = 0;
  for (let right = size - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5;
    for (let vertical = 0; vertical < size; vertical++) {
      for (let j = 0; j < 2; j++) {
        const x = right - j;
        const y = ((right + 1) & 2) === 0 ? size - 1 - vertical : vertical;
        if (!reserved[y][x] && i < data.length * 8) {
          modules[y][x] = ((data[i >>> 3] >>> (7 - (i & 7))) & 1) === 1;
          i++;
        }
      }
    }
  }

  const masked = chosen => {
    drawFormat(chosen);
    return modules.map((row, y) => row.map((dark, x) => (reserved[y][x] ? dark : dark !== MASKS[chosen](x, y))));
  };
  if (mask !== null) return masked(mask);
  let best = null;
  let bestPenalty = Infinity;
  for (let chosen = 0; chosen < 8; chosen++) {
    const candidate = masked(chosen);
    const score = penalty(candidate);
    if (score < bestPenalty) { best = candidate; bestPenalty = score; }
  }
  return best;
}

/** The QR code of `text` as SVG markup: one path, dark on light, with the
 * four-module quiet zone scanners need. Scales to any size. */
export function qrSvg(text, {dark = '#121214', light = '#ffffff'} = {}) {
  const matrix = qrMatrix(text);
  const size = matrix.length + 8;
  let path = '';
  matrix.forEach((row, y) => row.forEach((on, x) => { if (on) path += `M${x + 4} ${y + 4}h1v1h-1z`; }));
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" shape-rendering="crispEdges">`
    + `<rect width="${size}" height="${size}" fill="${light}"/><path d="${path}" fill="${dark}"/></svg>`;
}

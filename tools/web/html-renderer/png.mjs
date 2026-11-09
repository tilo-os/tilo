// Just enough PNG to compare two screenshots: decode 8-bit RGB or RGBA
// (what Chrome's screenshots are), encode RGBA, and diff two images.
import {deflateSync, inflateSync} from 'node:zlib';

export function decode(buffer) {
  let at = 8, width = 0, height = 0, channels = 4;
  const data = [];
  while (at < buffer.length) {
    const length = buffer.readUInt32BE(at);
    const type = buffer.toString('ascii', at + 4, at + 8);
    const chunk = buffer.subarray(at + 8, at + 8 + length);
    if (type === 'IHDR') {
      width = chunk.readUInt32BE(0);
      height = chunk.readUInt32BE(4);
      if (chunk[8] !== 8 || chunk[12] !== 0) throw Error('png: only 8-bit, non-interlaced images');
      channels = {2: 3, 6: 4}[chunk[9]];
      if (!channels) throw Error(`png: colour type ${chunk[9]}`);
    } else if (type === 'IDAT') data.push(chunk);
    else if (type === 'IEND') break;
    at += 12 + length;
  }
  const raw = inflateSync(Buffer.concat(data));
  const stride = width * channels;
  const pixels = new Uint8Array(width * height * 4);
  let previous = new Uint8Array(stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    const row = new Uint8Array(stride);
    for (let x = 0; x < stride; x++) {
      const a = x >= channels ? row[x - channels] : 0, b = previous[x], c = x >= channels ? previous[x - channels] : 0;
      const predictor = filter === 1 ? a : filter === 2 ? b : filter === 3 ? (a + b) >> 1
        : filter === 4 ? (() => { const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c); return pa <= pb && pa <= pc ? a : pb <= pc ? b : c; })() : 0;
      row[x] = (line[x] + predictor) & 255;
    }
    for (let x = 0; x < width; x++) {
      for (let k = 0; k < 4; k++) pixels[(y * width + x) * 4 + k] = k < channels ? row[x * channels + k] : 255;
    }
    previous = row;
  }
  return {width, height, pixels};
}

export function encode({width, height, pixels}) {
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (width * 4 + 1)] = 0;
    raw.set(pixels.subarray(y * width * 4, (y + 1) * width * 4), y * (width * 4 + 1) + 1);
  }
  const chunk = (type, data) => {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(data.length, 0);
    head.write(type, 4, 'ascii');
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])), 0);
    return Buffer.concat([head, data, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 6, 0, 0, 0], 8);
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', header), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}

const table = Array.from({length: 256}, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(bytes) {
  let c = 0xffffffff;
  for (const byte of bytes) c = table[(c ^ byte) & 255] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

// How two images of one size differ: the share of pixels whose largest
// channel difference is above `threshold`, the mean difference over all
// channels, and an image that shows those pixels red over a faded copy of `a`.
export function diff(a, b, threshold = 48) {
  if (a.width !== b.width || a.height !== b.height) return {error: `sizes differ: ${a.width}x${a.height} and ${b.width}x${b.height}`};
  const out = new Uint8Array(a.pixels.length);
  let differing = 0, total = 0;
  for (let i = 0; i < a.pixels.length; i += 4) {
    let most = 0;
    for (let k = 0; k < 3; k++) { const d = Math.abs(a.pixels[i + k] - b.pixels[i + k]); most = Math.max(most, d); total += d; }
    const over = most > threshold;
    if (over) differing++;
    const grey = (a.pixels[i] + a.pixels[i + 1] + a.pixels[i + 2]) / 3;
    out.set(over ? [255, 0, 0, 255] : [grey * 0.3 + 178, grey * 0.3 + 178, grey * 0.3 + 178, 255], i);
  }
  const n = a.pixels.length / 4;
  return {differingShare: differing / n, differing, meanDifference: total / (n * 3), image: encode({width: a.width, height: a.height, pixels: out})};
}

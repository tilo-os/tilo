/**
 * The tilo-video/1 object profile (tilo-moq `video`): a call's camera and
 * screen pictures, each source in resolution layers, each layer its own
 * track (`camera/low`, `camera/mid`, `camera/high`, `screen/low`,
 * `screen/high`). A group starts at a keyframe. Each object is one coded
 * picture behind a 32-byte little-endian header: version, flags (bit 0
 * keyframe), codec, layer, sequence (u64), capture time in microseconds
 * (u64), width (u16), height (u16), payload length (u32), reserved (u32).
 */
export const VIDEO_PROFILE_VERSION = 1;
export const VIDEO_HEADER_BYTES = 32;
export const VIDEO_MAX_OBJECT_BYTES = 512 * 1024;
export const VIDEO_MAX_SIDE = 4096;

export const CODECS = Object.freeze({h264: 1, vp8: 2, vp9: 3, av1: 4});
export const LAYERS = Object.freeze({low: 0, mid: 1, high: 2});
/** The layers each source is published in, and their sizes and bitrates. */
export const SOURCES = Object.freeze({
  camera: Object.freeze({
    low: {width: 320, height: 180, bitrate: 150_000},
    mid: {width: 640, height: 360, bitrate: 500_000},
    high: {width: 1280, height: 720, bitrate: 1_500_000},
  }),
  screen: Object.freeze({
    low: {width: 320, height: 180, bitrate: 150_000},
    high: {width: 1920, height: 1080, bitrate: 2_500_000},
  }),
});

/** The track a source's layer is published on, e.g. `camera/mid`. */
export function videoTrack(source, layer) {
  if (!SOURCES[source]?.[layer]) throw new RangeError(`no ${source} ${layer} layer`);
  return `${source}/${layer}`;
}

/** The largest size within `layer` that keeps the source's shape, even,
 * never larger than the source (shell `video::fitted`). */
export function fitted(width, height, box) {
  let w = width;
  let h = height;
  if (width > box.width || height > box.height) {
    if (width * box.height >= height * box.width) {
      w = box.width;
      h = Math.floor(height * box.width / width);
    } else {
      w = Math.floor(width * box.height / height);
      h = box.height;
    }
  }
  return [Math.max(2, w & ~1), Math.max(2, h & ~1)];
}

export function encodeVideoObject({keyframe, codec = 'h264', layer, sequence, timestampUs, width, height, payload}) {
  if (!(payload instanceof Uint8Array) || payload.byteLength === 0) throw new TypeError('payload must be a non-empty Uint8Array');
  if (VIDEO_HEADER_BYTES + payload.byteLength > VIDEO_MAX_OBJECT_BYTES) throw new RangeError('video object too large');
  if (!(width > 0 && height > 0 && width <= VIDEO_MAX_SIDE && height <= VIDEO_MAX_SIDE)) throw new RangeError('invalid picture size');
  if (!(codec in CODECS) || !(layer in LAYERS)) throw new RangeError('unknown codec or layer');
  const encoded = new Uint8Array(VIDEO_HEADER_BYTES + payload.byteLength);
  const header = new DataView(encoded.buffer);
  header.setUint8(0, VIDEO_PROFILE_VERSION);
  header.setUint8(1, keyframe ? 1 : 0);
  header.setUint8(2, CODECS[codec]);
  header.setUint8(3, LAYERS[layer]);
  header.setBigUint64(4, BigInt(sequence), true);
  header.setBigUint64(12, BigInt(timestampUs), true);
  header.setUint16(20, width, true);
  header.setUint16(22, height, true);
  header.setUint32(24, payload.byteLength, true);
  encoded.set(payload, VIDEO_HEADER_BYTES);
  return encoded;
}

export function decodeVideoObject(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.byteLength <= VIDEO_HEADER_BYTES || bytes.byteLength > VIDEO_MAX_OBJECT_BYTES) throw new RangeError('invalid video object');
  const header = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const flags = header.getUint8(1);
  if (header.getUint8(0) !== VIDEO_PROFILE_VERSION || (flags & ~1) !== 0 || header.getUint32(28, true) !== 0) throw new RangeError('invalid video object');
  const codec = Object.keys(CODECS).find(name => CODECS[name] === header.getUint8(2));
  const layer = Object.keys(LAYERS).find(name => LAYERS[name] === header.getUint8(3));
  const width = header.getUint16(20, true);
  const height = header.getUint16(22, true);
  if (!codec || !layer || !width || !height || width > VIDEO_MAX_SIDE || height > VIDEO_MAX_SIDE || header.getUint32(24, true) !== bytes.byteLength - VIDEO_HEADER_BYTES) throw new RangeError('invalid video object');
  return {
    keyframe: (flags & 1) === 1,
    codec,
    layer,
    sequence: header.getBigUint64(4, true),
    timestampUs: header.getBigUint64(12, true),
    width,
    height,
    payload: bytes.subarray(VIDEO_HEADER_BYTES),
  };
}

/** Base64 of `bytes`, as the local media lane carries a picture. */
export function base64Of(bytes) {
  let text = '';
  for (let index = 0; index < bytes.length; index += 0x8000) text += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
  return btoa(text);
}

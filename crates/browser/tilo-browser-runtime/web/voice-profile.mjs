/**
 * The tilo-voice/1 object profile (tilo-moq `voice`): a 32-byte little-endian
 * header (version, sequence, 48 kHz timestamp, 960-sample duration, rate,
 * channels, payload length) and one 20 ms mono Opus packet. A datagram
 * carries a bundle: the newest object and up to two earlier ones.
 */
export const VOICE_PROFILE_VERSION = 1;
export const VOICE_HEADER_BYTES = 32;
export const VOICE_MAX_OBJECT_BYTES = 4096;
export const VOICE_FRAME_SAMPLES = 960;

const VOICE_SAMPLE_RATE = 48000;
const VOICE_CHANNELS = 1;
const U64_MAX = (1n << 64n) - 1n;

function assertU64(name, value) {
  if (typeof value !== 'bigint' || value < 0n || value > U64_MAX) {
    throw new RangeError(`${name} must be a BigInt in the unsigned 64-bit range`);
  }
}

export function encodeVoiceObject({sequence, timestamp, duration, payload}) {
  assertU64('sequence', sequence);
  assertU64('timestamp', timestamp);
  if (duration !== VOICE_FRAME_SAMPLES) {
    throw new RangeError(`duration must be ${VOICE_FRAME_SAMPLES} samples`);
  }
  if (!(payload instanceof Uint8Array)) {
    throw new TypeError('payload must be a Uint8Array');
  }
  if (payload.byteLength === 0) {
    throw new RangeError('payload must not be empty');
  }
  if (VOICE_HEADER_BYTES + payload.byteLength > VOICE_MAX_OBJECT_BYTES) {
    throw new RangeError(`encoded voice object exceeds ${VOICE_MAX_OBJECT_BYTES} bytes`);
  }

  const encoded = new Uint8Array(VOICE_HEADER_BYTES + payload.byteLength);
  const header = new DataView(encoded.buffer, encoded.byteOffset, encoded.byteLength);

  header.setUint8(0, VOICE_PROFILE_VERSION);
  header.setBigUint64(4, sequence, true);
  header.setBigUint64(12, timestamp, true);
  header.setUint16(20, duration, true);
  header.setUint32(22, VOICE_SAMPLE_RATE, true);
  header.setUint8(26, VOICE_CHANNELS);
  header.setUint32(28, payload.byteLength, true);
  encoded.set(payload, VOICE_HEADER_BYTES);
  return encoded;
}

export function decodeVoiceObject(bytes) {
  if (!(bytes instanceof Uint8Array)) {
    throw new TypeError('voice object must be a Uint8Array');
  }
  if (bytes.byteLength < VOICE_HEADER_BYTES) {
    throw new RangeError('voice object is shorter than its header');
  }
  if (bytes.byteLength > VOICE_MAX_OBJECT_BYTES) {
    throw new RangeError(`voice object exceeds ${VOICE_MAX_OBJECT_BYTES} bytes`);
  }

  const header = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (header.getUint8(0) !== VOICE_PROFILE_VERSION) {
    throw new RangeError('unsupported voice object version');
  }
  if (header.getUint8(1) !== 0) {
    throw new RangeError('unknown voice object flags');
  }
  if (header.getUint16(2, true) !== 0 || header.getUint8(27) !== 0) {
    throw new RangeError('voice object reserved fields must be zero');
  }

  const duration = header.getUint16(20, true);
  if (duration !== VOICE_FRAME_SAMPLES) {
    throw new RangeError(`voice object duration must be ${VOICE_FRAME_SAMPLES} samples`);
  }
  if (header.getUint32(22, true) !== VOICE_SAMPLE_RATE) {
    throw new RangeError(`voice object sample rate must be ${VOICE_SAMPLE_RATE} Hz`);
  }
  if (header.getUint8(26) !== VOICE_CHANNELS) {
    throw new RangeError(`voice object channels must be ${VOICE_CHANNELS}`);
  }

  const payloadLength = header.getUint32(28, true);
  if (payloadLength === 0) {
    throw new RangeError('voice object payload must not be empty');
  }
  if (VOICE_HEADER_BYTES + payloadLength !== bytes.byteLength) {
    throw new RangeError('voice object payload length does not match its bytes');
  }

  // Return a plain owned Uint8Array even when the input is a Uint8Array subclass.
  const payload = new Uint8Array(payloadLength);
  payload.set(bytes.subarray(VOICE_HEADER_BYTES));
  return {
    sequence: header.getBigUint64(4, true),
    timestamp: header.getBigUint64(12, true),
    duration,
    payload,
  };
}

// Datagram bundle (redundancy): one QUIC datagram carries the newest voice
// object followed by up to two earlier objects, each a complete tilo-voice/1
// object (self-delimiting through its payload length). A receiver that lost
// the datagram carrying frame N recovers it from the next one. Objects stay
// byte-identical to the stream form; only the datagram payload concatenates.
export const VOICE_MAX_BUNDLE_OBJECTS = 3;

export function encodeVoiceBundle(objects) {
  if (!Array.isArray(objects) || objects.length === 0 || objects.length > VOICE_MAX_BUNDLE_OBJECTS) {
    throw new RangeError(`a voice bundle carries 1 to ${VOICE_MAX_BUNDLE_OBJECTS} objects`);
  }
  let size = 0;
  for (const object of objects) {
    if (!(object instanceof Uint8Array)) throw new TypeError('bundle objects must be encoded voice objects');
    size += object.byteLength;
  }
  if (size > VOICE_MAX_OBJECT_BYTES) throw new RangeError(`voice bundle exceeds ${VOICE_MAX_OBJECT_BYTES} bytes`);
  const bundle = new Uint8Array(size);
  let offset = 0;
  for (const object of objects) {
    bundle.set(object, offset);
    offset += object.byteLength;
  }
  return bundle;
}

// Split and fully validate a bundle; returns decoded objects in wire order.
export function decodeVoiceBundle(bytes) {
  if (!(bytes instanceof Uint8Array)) throw new TypeError('voice bundle must be a Uint8Array');
  if (bytes.byteLength > VOICE_MAX_OBJECT_BYTES) throw new RangeError(`voice bundle exceeds ${VOICE_MAX_OBJECT_BYTES} bytes`);
  const objects = [];
  let offset = 0;
  while (offset < bytes.byteLength) {
    if (objects.length === VOICE_MAX_BUNDLE_OBJECTS) throw new RangeError('voice bundle carries too many objects');
    if (bytes.byteLength - offset < VOICE_HEADER_BYTES) throw new RangeError('voice bundle is truncated');
    const view = new DataView(bytes.buffer, bytes.byteOffset + offset, VOICE_HEADER_BYTES);
    const end = offset + VOICE_HEADER_BYTES + view.getUint32(28, true);
    if (end > bytes.byteLength) throw new RangeError('voice bundle is truncated');
    objects.push(decodeVoiceObject(bytes.subarray(offset, end)));
    offset = end;
  }
  if (objects.length === 0) throw new RangeError('voice bundle is empty');
  return objects;
}

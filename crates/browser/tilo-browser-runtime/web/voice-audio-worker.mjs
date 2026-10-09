const MAX_CODEC_QUEUE = 4;
const MAX_REMOTE_PUBLISHERS = 7;
const SAMPLE_RATE = 48_000n;
const MICROSECONDS_PER_SECOND = 1_000_000n;

let encoder;
// Encoder input uses its own continuous 20 ms codec clock, like the decoder
// path: WebCodecs Opus implementations derive output chunk timestamps from the
// first input plus the encoded duration, so a capture discontinuity (credit
// drop, mute/unmute, input switch) must not break output correlation. The
// original capture sample timestamp travels in the pending metadata.
let nextEncodeTimestamp = 0;
let playbackConfig;
let stopped = false;
// Capture metadata of the frames handed to the encoder, in encode order.
const pendingEncodes = [];
const decoders = new Map();
// Opus DTX (usedtx): a silent frame that needs no transmission produces no
// output at all (Chromium) or a packet of at most two bytes (libopus), and
// Chromium's output timestamps then count only emitted frames. Outputs still
// follow encode order, and an encoder finishes a 20 ms frame within
// milliseconds of receiving it, so when an output arrives every pending frame
// that waited longer than this, with a newer one behind it, was a DTX frame.
// A slower encoder only shifts a capture timestamp by one frame.
const DTX_SETTLED_MS = 15;
// Bounds the metadata of frames the encoder silently consumed: 20 DTX
// frames per comfort-noise update, plus a short backlog.
const MAX_PENDING_ENCODES = 64;
const DTX_MAX_BYTES = 2;
const now = () => (self.performance ?? globalThis.performance).now();

function mediaTimestamp(sampleTimestamp) {
  return Number(sampleTimestamp * MICROSECONDS_PER_SECOND / SAMPLE_RATE);
}

function appendPending(table, timestamp, value) {
  const pending = table.get(timestamp) ?? [];
  pending.push(value);
  table.set(timestamp, pending);
}

function takePending(table, timestamp) {
  const pending = table.get(timestamp);
  if (!pending?.length) return undefined;
  const value = pending.shift();
  if (!pending.length) table.delete(timestamp);
  return value;
}

function takeOldest(table) {
  const first = table.keys().next();
  return first.done ? undefined : takePending(table, first.value);
}

function postCaptureConsumed() {
  self.postMessage({type: 'capture-consumed'});
}

function encoderError(error) {
  self.postMessage({type: 'error', stage: 'encoder', error: error?.message ?? String(error)});
  // Capture credits went back when each frame reached the encoder.
  pendingEncodes.length = 0;
}

/** The capture metadata of the frame an encoder output belongs to. */
function takeEncoded() {
  const at = now();
  while (pendingEncodes.length > 1 && at - pendingEncodes[0].submittedAt > DTX_SETTLED_MS) pendingEncodes.shift();
  return pendingEncodes.shift();
}

function postPlaybackRelease(state, samples) {
  self.postMessage({
    type: 'playback-released',
    publisherId: state.publisherId,
    generation: state.generation,
    samples,
  });
}

function decoderError(state, error) {
  let samples = 0;
  for (const pending of state.pending.values()) {
    for (const metadata of pending) samples += metadata.duration;
  }
  state.pending.clear();
  if (samples > 0) postPlaybackRelease(state, samples);
  self.postMessage({
    type: 'error',
    stage: 'decoder',
    publisherId: state.publisherId,
    generation: state.generation,
    error: error?.message ?? String(error),
  });
}

function closeDecoder(state) {
  try { state.decoder.close(); } catch {}
  state.pending.clear();
  if (decoders.get(state.publisherId) === state) decoders.delete(state.publisherId);
}

function createDecoder(message) {
  const existing = decoders.get(message.publisherId);
  if (existing?.generation === message.generation) return existing;
  if (existing) closeDecoder(existing);
  if (decoders.size >= MAX_REMOTE_PUBLISHERS) return null;
  const state = {
    publisherId: message.publisherId,
    generation: message.generation,
    pending: new Map(),
    nextCodecTimestamp: 0,
    decoder: null,
  };
  state.decoder = new self.AudioDecoder({
    output(frame) {
      // Correlate by decode order: one Opus packet decodes to one frame, in
      // order, but decoder implementations may shift output timestamps (codec
      // delay, concealment), so a timestamp match could pair the wrong chunk.
      const metadata = takeOldest(state.pending);
      if (!metadata) {
        frame.close();
        self.postMessage({type: 'error', stage: 'decoder-output', publisherId: state.publisherId, generation: state.generation, error: 'Decoded frame has no voice metadata'});
        return;
      }
      if (frame.numberOfFrames !== metadata.duration
        || frame.sampleRate !== Number(SAMPLE_RATE)
        || frame.numberOfChannels !== 1) {
        frame.close();
        postPlaybackRelease(state, metadata.duration);
        self.postMessage({type: 'error', stage: 'decoder-output', publisherId: state.publisherId, generation: state.generation, error: 'Decoded audio does not match the fixed voice profile'});
        return;
      }
      const samples = new Float32Array(frame.numberOfFrames);
      frame.copyTo(samples, {planeIndex: 0, format: 'f32-planar'});
      frame.close();
      self.postMessage({
        type: 'decoded',
        publisherId: state.publisherId,
        generation: state.generation,
        samples: samples.buffer,
        timestamp: metadata.timestamp,
        duration: metadata.duration,
        reservedSamples: metadata.duration,
      }, [samples.buffer]);
    },
    error(error) { decoderError(state, error); },
  });
  state.decoder.configure(playbackConfig);
  decoders.set(state.publisherId, state);
  return state;
}

function initialize({encoderConfig, decoderConfig}) {
  try {
    encoder = new self.AudioEncoder({
      output(chunk) {
        // Correlate by encode order: one 960-sample input produces at most
        // one Opus packet, in order; output timestamps are
        // implementation-derived (and skip DTX frames).
        const metadata = takeEncoded();
        if (!metadata) {
          self.postMessage({type: 'error', stage: 'encoder-output', error: 'Encoded frame has no capture metadata'});
          return;
        }
        // A DTX frame: nothing to transmit.
        if (chunk.byteLength <= DTX_MAX_BYTES) return;
        const payload = new Uint8Array(chunk.byteLength);
        chunk.copyTo(payload);
        self.postMessage({
          type: 'encoded',
          payload: payload.buffer,
          timestamp: metadata.timestamp,
          duration: metadata.duration,
          ...(metadata.generation === undefined ? {} : {generation: metadata.generation}),
        }, [payload.buffer]);
      },
      error(error) { encoderError(error); },
    });
    encoder.configure(encoderConfig);
    playbackConfig = decoderConfig;
    self.postMessage({type: 'ready'});
  } catch (error) {
    encoder?.close?.();
    self.postMessage({type: 'init-error', error: error?.message ?? String(error)});
  }
}

function encode(message) {
  if (!encoder || encoder.encodeQueueSize >= MAX_CODEC_QUEUE) {
    self.postMessage({type: 'drop', stage: 'encoder-queue', count: 1});
    postCaptureConsumed();
    return;
  }
  const timestamp = nextEncodeTimestamp;
  nextEncodeTimestamp += mediaTimestamp(BigInt(message.duration));
  const frame = new self.AudioData({
    format: 'f32-planar',
    sampleRate: Number(SAMPLE_RATE),
    numberOfFrames: message.duration,
    numberOfChannels: 1,
    timestamp,
    data: new Float32Array(message.samples),
  });
  const metadata = {timestamp: message.timestamp, duration: message.duration, generation: message.generation, submittedAt: now()};
  pendingEncodes.push(metadata);
  while (pendingEncodes.length > MAX_PENDING_ENCODES) pendingEncodes.shift();
  try {
    encoder.encode(frame);
  } catch (error) {
    const index = pendingEncodes.indexOf(metadata);
    if (index >= 0) pendingEncodes.splice(index, 1);
    self.postMessage({type: 'error', stage: 'encoder', error: error?.message ?? String(error)});
  } finally {
    frame.close();
  }
  // The frame left the capture path: the encoder's own queue is bounded by
  // the admission check above, and a DTX frame may never produce an output
  // to return its credit with.
  postCaptureConsumed();
}

function decode(message) {
  let state;
  try { state = createDecoder(message); }
  catch (error) {
    self.postMessage({type: 'playback-released', publisherId: message.publisherId, generation: message.generation, samples: message.duration});
    self.postMessage({type: 'error', stage: 'decoder', publisherId: message.publisherId, generation: message.generation, error: error?.message ?? String(error)});
    return;
  }
  if (!state) {
    self.postMessage({type: 'playback-released', publisherId: message.publisherId, generation: message.generation, samples: message.duration});
    self.postMessage({type: 'drop', stage: 'decoder-publisher-limit', publisherId: message.publisherId, count: 1});
    return;
  }
  if (state.decoder.decodeQueueSize >= MAX_CODEC_QUEUE) {
    postPlaybackRelease(state, message.duration);
    self.postMessage({type: 'drop', stage: 'decoder-queue', publisherId: state.publisherId, count: 1});
    return;
  }
  const timestamp = state.nextCodecTimestamp;
  state.nextCodecTimestamp += 20_000;
  appendPending(state.pending, timestamp, {timestamp: message.timestamp, duration: message.duration});
  try {
    state.decoder.decode(new self.EncodedAudioChunk({
      type: 'key',
      timestamp,
      duration: Number(BigInt(message.duration) * MICROSECONDS_PER_SECOND / SAMPLE_RATE),
      data: new Uint8Array(message.payload),
    }));
  } catch (error) {
    takePending(state.pending, timestamp);
    postPlaybackRelease(state, message.duration);
    self.postMessage({type: 'error', stage: 'decoder', publisherId: state.publisherId, generation: state.generation, error: error?.message ?? String(error)});
  }
}

function removePublisher(message) {
  const state = decoders.get(message.publisherId);
  if (!state || state.generation !== message.generation) return;
  closeDecoder(state);
}

function stop() {
  if (stopped) return;
  stopped = true;
  encoder?.close?.();
  for (const state of decoders.values()) {
    try { state.decoder.close(); } catch {}
  }
  decoders.clear();
  pendingEncodes.length = 0;
  self.close?.();
}

self.onmessage = event => {
  if (stopped) return;
  const message = event.data;
  if (message.type === 'init') initialize(message);
  else if (message.type === 'encode') encode(message);
  else if (message.type === 'decode') decode(message);
  else if (message.type === 'remove-publisher') removePublisher(message);
  else if (message.type === 'stop') stop();
};

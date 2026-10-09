import test from 'node:test';
import assert from 'node:assert/strict';

const drain = async () => {
  for (let index = 0; index < 8; index++) await Promise.resolve();
};
const PUB_A = 'a'.repeat(64);
const PUB_B = 'b'.repeat(64);

test('codec worker emits raw Opus with sample timestamps and bounds encoder admission', async t => {
  const previousSelf = globalThis.self;
  const posted = [];
  class AudioData {
    constructor(init) { Object.assign(this, init); }
    close() { this.closed = true; }
  }
  class EncodedAudioChunk {
    constructor(init) { Object.assign(this, init); this.byteLength = init.data.byteLength; }
    copyTo(target) { target.set(this.data); }
  }
  class AudioEncoder {
    constructor(callbacks) { Object.assign(this, callbacks); this.encodeQueueSize = 0; AudioEncoder.instance = this; }
    configure(config) { this.config = config; }
    encode(frame) { (this.pending ??= []).push(frame); this.encodeQueueSize++; }
    close() { this.closed = true; }
    emit(bytes = Uint8Array.of(4, 5)) {
      this.encodeQueueSize--;
      const frame = this.pending.shift();
      this.output(new EncodedAudioChunk({type: 'key', timestamp: frame.timestamp, duration: 20_000, data: bytes}));
    }
  }
  class AudioDecoder {
    constructor(callbacks) { Object.assign(this, callbacks); }
    configure(config) { this.config = config; }
    close() { this.closed = true; }
  }
  globalThis.self = {
    AudioData, AudioEncoder, AudioDecoder, EncodedAudioChunk,
    postMessage(message) { posted.push(message); },
    close() { this.closed = true; },
  };
  t.after(() => { globalThis.self = previousSelf; });
  await import(`./voice-audio-worker.mjs?worker-contract=${Date.now()}`);

  self.onmessage({data: {type: 'init', encoderConfig: {codec: 'opus'}, decoderConfig: {codec: 'opus'}}});
  assert.deepEqual(posted.shift(), {type: 'ready'});
  for (let index = 0; index < 5; index++) {
    self.onmessage({data: {type: 'encode', samples: new Float32Array(960).buffer, timestamp: BigInt(index * 960), duration: 960}});
  }
  // Each frame returns its capture credit once the encoder holds it (a DTX
  // frame may never produce an output); the fifth is refused at admission.
  assert.deepEqual(posted, [
    {type: 'capture-consumed'},
    {type: 'capture-consumed'},
    {type: 'capture-consumed'},
    {type: 'capture-consumed'},
    {type: 'drop', stage: 'encoder-queue', count: 1},
    {type: 'capture-consumed'},
  ]);
  posted.length = 0;
  AudioEncoder.instance.emit(Uint8Array.of(4, 5, 6));
  assert.deepEqual(posted, [
    {type: 'encoded', payload: Uint8Array.of(4, 5, 6).buffer, timestamp: 0n, duration: 960},
  ]);
});

test('codec worker sends nothing for Opus DTX frames and keeps capture timestamps exact around them', async t => {
  // Chromium with usedtx: a frame that needs no transmission produces no
  // output, and output timestamps count only emitted frames. libopus itself
  // returns a packet of at most two bytes instead.
  const previousSelf = globalThis.self;
  const posted = [];
  let clock = 0;
  class AudioData { constructor(init) { Object.assign(this, init); } close() {} }
  class EncodedAudioChunk {
    constructor(init) { Object.assign(this, init); this.byteLength = init.data.byteLength; }
    copyTo(target) { target.set(this.data); }
  }
  class AudioEncoder {
    constructor(callbacks) { Object.assign(this, callbacks); this.encodeQueueSize = 0; this.emitted = 0; AudioEncoder.instance = this; }
    configure(config) { this.config = config; }
    encode(frame) { (this.pending ??= []).push(frame); }
    close() {}
    // Finish the oldest input a millisecond later: emit it, or not.
    finish(bytes) {
      this.pending.shift();
      clock += 1;
      if (!bytes) return;
      this.output(new EncodedAudioChunk({type: 'key', timestamp: 20_000 * this.emitted++, duration: 20_000, data: bytes}));
    }
  }
  class AudioDecoder { constructor() {} configure() {} close() {} }
  globalThis.self = {AudioData, AudioEncoder, AudioDecoder, EncodedAudioChunk, performance: {now: () => clock},
    postMessage(message) { posted.push(message); }, close() {}};
  t.after(() => { globalThis.self = previousSelf; });
  await import(`./voice-audio-worker.mjs?dtx=${Date.now()}`);
  self.onmessage({data: {type: 'init', encoderConfig: {codec: 'opus', opus: {usedtx: true}}, decoderConfig: {codec: 'opus'}}});
  posted.length = 0;
  // 3 speech frames, 40 silent ones (one comfort-noise update after 20, the
  // rest suppressed or 1-byte DTX packets), then 3 speech frames again.
  const speech = Uint8Array.of(0x78, 1, 2, 3);
  const update = Uint8Array.of(0x08, 9, 9);
  const frames = [
    ...Array(3).fill(speech),
    ...Array.from({length: 40}, (_, index) => index === 20 ? update : index % 2 ? undefined : Uint8Array.of(0x08)),
    ...Array(3).fill(speech),
  ];
  frames.forEach((bytes, index) => {
    self.onmessage({data: {type: 'encode', samples: new Float32Array(960).buffer, timestamp: BigInt(index * 960), duration: 960}});
    AudioEncoder.instance.finish(bytes);
    clock += 19;
  });
  assert.equal(posted.filter(message => message.type === 'error').length, 0);
  assert.equal(posted.filter(message => message.type === 'capture-consumed').length, frames.length, 'every frame returns its credit');
  const encoded = posted.filter(message => message.type === 'encoded');
  assert.deepEqual(encoded.map(message => message.timestamp), [0, 1, 2, 23, 43, 44, 45].map(frame => BigInt(frame * 960)));
  assert.deepEqual(new Uint8Array(encoded[3].payload), update);
});

test('codec worker owns an independently bounded decoder for each remote publisher', async t => {
  const previousSelf = globalThis.self;
  const posted = [];
  class AudioEncoder { constructor() {} configure() {} close() {} }
  class EncodedAudioChunk { constructor(init) { Object.assign(this, init); } }
  class AudioData {
    constructor(timestamp) { this.numberOfFrames = 960; this.numberOfChannels = 1; this.sampleRate = 48_000; this.timestamp = timestamp; }
    copyTo(target) { target.fill(0.25); }
    close() { this.closed = true; }
  }
  class AudioDecoder {
    static instances = [];
    constructor(callbacks) { Object.assign(this, callbacks); this.decodeQueueSize = 0; AudioDecoder.instances.push(this); }
    configure(config) { this.config = config; }
    decode(chunk) { (this.pending ??= []).push(chunk); this.decodeQueueSize++; }
    close() { this.closed = true; }
    emit() { this.decodeQueueSize--; const chunk = this.pending.shift(); this.output(new AudioData(chunk.timestamp)); }
    fail(error) { this.error(error); }
  }
  globalThis.self = {AudioData, AudioEncoder, AudioDecoder, EncodedAudioChunk, postMessage(message) { posted.push(message); }};
  t.after(() => { globalThis.self = previousSelf; });
  await import(`./voice-audio-worker.mjs?decode-contract=${Date.now()}`);
  self.onmessage({data: {type: 'init', encoderConfig: {}, decoderConfig: {}}});
  posted.length = 0;
  for (let index = 0; index < 5; index++) {
    self.onmessage({data: {type: 'decode', publisherId: PUB_A, generation: 1, payload: Uint8Array.of(9, 8).buffer, timestamp: BigInt(1920 + index * 960), duration: 960}});
  }
  assert.deepEqual(posted, [
    {type: 'playback-released', publisherId: PUB_A, generation: 1, samples: 960},
    {type: 'drop', stage: 'decoder-queue', publisherId: PUB_A, count: 1},
  ]);
  self.onmessage({data: {type: 'decode', publisherId: PUB_B, generation: 2, payload: Uint8Array.of(7).buffer, timestamp: 9600n, duration: 960}});
  assert.equal(AudioDecoder.instances.length, 2);
  assert.notEqual(AudioDecoder.instances[0], AudioDecoder.instances[1]);
  posted.length = 0;
  AudioDecoder.instances[0].emit();
  AudioDecoder.instances[1].emit();
  assert.equal(posted[0].type, 'decoded');
  assert.equal(posted[0].publisherId, PUB_A);
  assert.equal(posted[0].generation, 1);
  assert.equal(posted[0].timestamp, 1920n);
  assert.equal(posted[0].duration, 960);
  assert.equal(new Float32Array(posted[0].samples)[0], 0.25);
  assert.equal(posted[1].publisherId, PUB_B);
  self.onmessage({data: {type: 'remove-publisher', publisherId: PUB_A, generation: 1}});
  assert.equal(AudioDecoder.instances[0].closed, true);
  self.onmessage({data: {type: 'decode', publisherId: PUB_A, generation: 3, payload: Uint8Array.of(6).buffer, timestamp: 10_560n, duration: 960}});
  posted.length = 0;
  AudioDecoder.instances[0].fail(new Error('late decoder failure'));
  assert.deepEqual(posted.at(-1), {type: 'error', stage: 'decoder', publisherId: PUB_A, generation: 1, error: 'late decoder failure'});
});

test('worklet publishes only credited unmuted 20 ms frames and advances sample time while muted', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (name, implementation) => { assert.equal(name, 'tilo-voice-audio'); Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?capture-contract=${Date.now()}`);
  const processor = new Processor({processorOptions: {frameSamples: 960, captureCredits: 1, captureGeneration: 0, muted: true}});
  const input = [[new Float32Array(128).fill(0.5)]];
  const output = [[new Float32Array(128)]];
  for (let index = 0; index < 8; index++) processor.process(input, output);
  assert.deepEqual(processor.port.sent, []);
  processor.port.receive({type: 'muted', value: false, generation: 1});
  for (let index = 0; index < 8; index++) processor.process(input, output);
  assert.equal(processor.port.sent[0].type, 'capture');
  assert.equal(processor.port.sent[0].timestamp, 1024n);
  assert.equal(processor.port.sent[0].duration, 960);
  assert.equal(processor.port.sent[0].generation, 1);
  assert.equal(new Float32Array(processor.port.sent[0].samples).length, 960);
  processor.port.receive({type: 'muted', value: true, generation: 2});
  processor.port.receive({type: 'capture-credit'});
  processor.port.receive({type: 'muted', value: false, generation: 3});
  const replacementInput = [[new Float32Array(128).fill(0.75)]];
  for (let index = 0; index < 8; index++) processor.process(replacementInput, output);
  const replacement = processor.port.sent.findLast(message => message.type === 'capture');
  assert.equal(replacement.generation, 3);
  assert(new Float32Array(replacement.samples).every(sample => sample === 0.75), 'mute/input generation reset excludes partial samples from the old source');
});

test('worklet coalesces capture overload until a transfer credit returns', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?capture-overload=${Date.now()}`);
  const processor = new Processor({processorOptions: {captureCredits: 1, captureGeneration: 1, muted: false}});
  const input = [[new Float32Array(128).fill(0.25)]];
  for (let index = 0; index < 750; index++) processor.process(input, [[new Float32Array(128)]]);
  const levels = processor.port.sent.filter(message => message.type === 'levels');
  processor.port.sent = processor.port.sent.filter(message => message.type !== 'levels');
  assert.equal(processor.port.sent.length, 1, 'stalled main thread gets one transferable capture and no per-frame control tasks');
  assert.equal(processor.port.sent[0].type, 'capture');
  assert.equal(levels.length, 20, 'speaking levels every 100 ms of the 2 s rendered');
  assert.ok(Math.abs(levels[0].capture - 0.25) < 1e-6, 'the level is the captured RMS');
  processor.port.receive({type: 'capture-credit'});
  assert.deepEqual(processor.port.sent.at(-1), {type: 'drop', stage: 'capture-worklet', count: 99});
});

test('worklet caps playback at 200 ms of PCM and drains accepted samples in order', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?playback-contract=${Date.now()}`);
  const processor = new Processor({processorOptions: {muted: true}});
  for (let index = 0; index < 11; index++) {
    processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).fill(0.25).buffer, timestamp: BigInt(index * 960), duration: 960, reservedSamples: 960});
  }
  assert.deepEqual(processor.port.sent, [
    {type: 'drop', stage: 'playback-buffer', publisherId: PUB_A, count: 1},
    {type: 'playback-released', publisherId: PUB_A, generation: 1, samples: 960},
  ]);
  for (let index = 0; index < 22; index++) processor.process([], [[new Float32Array(128)]]);
  const channel = new Float32Array(128);
  processor.process([], [[channel]]);
  assert(channel.subarray(0, 64).every(sample => sample === 0));
  assert(channel.subarray(64).every(sample => sample === 0.25));
  assert.deepEqual(processor.port.sent.at(-1), {type: 'playback-consumed', publisherId: PUB_A, generation: 1, samples: 64});
  await drain();
});

test('worklet mixes simultaneous publishers additively without capture input or track-count normalization', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?mix-contract=${Date.now()}`);
  const processor = new Processor({processorOptions: {muted: false}});
  processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).fill(0.25).buffer, timestamp: 0n, duration: 960, reservedSamples: 960});
  processor.port.receive({type: 'play', publisherId: PUB_B, generation: 2, samples: new Float32Array(960).fill(0.5).buffer, timestamp: 0n, duration: 960, reservedSamples: 960});
  const captureInput = [[new Float32Array(128).fill(1)]];
  for (let index = 0; index < 22; index++) processor.process(captureInput, [[new Float32Array(128)]]);
  const channel = new Float32Array(128);
  processor.process(captureInput, [[channel]]);
  assert(channel.subarray(0, 64).every(sample => sample === 0));
  assert(channel.subarray(64).every(sample => sample === 0.75), '0.25 + 0.5 is mixed; mic input is neither looped nor used to normalize output');
});

test('worklet removal expires queued publisher audio and stale packets cannot replay', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?remove-contract=${Date.now()}`);
  const processor = new Processor({processorOptions: {muted: true}});
  processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).fill(0.25).buffer, timestamp: 0n, duration: 960, reservedSamples: 960});
  processor.port.receive({type: 'remove-publisher', publisherId: PUB_A, generation: 1});
  assert.deepEqual(processor.port.sent.at(-1), {type: 'playback-released', publisherId: PUB_A, generation: 1, samples: 960});
  for (let index = 0; index < 24; index++) processor.process([], [[new Float32Array(128)]]);
  processor.port.receive({type: 'play', publisherId: PUB_B, generation: 2, samples: new Float32Array(960).fill(0.5).buffer, timestamp: 0n, duration: 960, reservedSamples: 960});
  for (let index = 0; index < 23; index++) processor.process([], [[new Float32Array(128)]]);
  processor.port.receive({type: 'play', publisherId: PUB_B, generation: 2, samples: new Float32Array(960).fill(0.5).buffer, timestamp: 0n, duration: 960, reservedSamples: 960});
  assert(processor.port.sent.some(message => message.type === 'drop' && message.stage === 'playback-replay' && message.publisherId === PUB_B));
  assert(processor.port.sent.some(message => message.type === 'playback-released' && message.publisherId === PUB_B));
  for (let index = 0; index < 64; index++) processor.process([], [[new Float32Array(128)]]);
  processor.port.receive({type: 'play', publisherId: PUB_B, generation: 2, samples: new Float32Array(960).fill(0.5).buffer, timestamp: 960n, duration: 960, reservedSamples: 960});
  assert(processor.port.sent.some(message => message.type === 'drop' && message.stage === 'playback-expired' && message.publisherId === PUB_B));
  processor.port.receive({type: 'play', publisherId: PUB_B, generation: 2, samples: new Float32Array(960).fill(0.5).buffer, timestamp: 1_000_000n, duration: 960, reservedSamples: 960});
  for (let index = 0; index < 22; index++) processor.process([], [[new Float32Array(128)]]);
  const rebuffered = new Float32Array(128);
  processor.process([], [[rebuffered]]);
  assert(rebuffered.subarray(0, 64).every(sample => sample === 0));
  assert(rebuffered.subarray(64).every(sample => sample === 0.5), 'forward discontinuity is rebased to the 60 ms target instead of queued as a gap');
});

test('worklet expires every far-future frame by live arrival age even behind a queued current frame', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?future-expiry=${Date.now()}`);
  const processor = new Processor({processorOptions: {muted: true}});
  processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).buffer, timestamp: 0n, duration: 960, reservedSamples: 960});
  for (let index = 0; index < 9; index++) {
    processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).buffer, timestamp: 48_000_000n + BigInt(index * 960), duration: 960, reservedSamples: 960});
  }
  for (let index = 0; index < 750; index++) processor.process([], [[new Float32Array(128)]]);
  assert.equal(processor.publishers.get(PUB_A).queue.length, 0);
  assert.equal(processor.port.sent.filter(message => message.type === 'drop' && message.stage === 'playback-expired').length, 9);
  assert.equal(processor.port.sent.filter(message => message.type === 'playback-released' && message.publisherId === PUB_A).reduce((sum, message) => sum + message.samples, 0), 9 * 960);
  const reportsBeforeCurrent = processor.port.sent.length;
  processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).buffer, timestamp: 96_000n, duration: 960, reservedSamples: 960});
  assert.equal(processor.publishers.get(PUB_A).queue.length, 1, 'expired future timestamps cannot poison the replay frontier for current speech');
  assert.equal(processor.port.sent.slice(reportsBeforeCurrent).some(message => message.stage === 'playback-replay'), false);
});

test('worklet sweeps live expiry independently of media timestamp queue order', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?ordered-expiry=${Date.now()}`);
  const processor = new Processor({processorOptions: {muted: true}});
  processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).buffer, timestamp: 0n, duration: 960, reservedSamples: 960});
  processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).buffer, timestamp: 48_000_000n, duration: 960, reservedSamples: 960});
  for (let index = 0; index < 38; index++) processor.process([], [[new Float32Array(128)]]);
  processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).buffer, timestamp: 48_000n, duration: 960, reservedSamples: 960});
  for (let index = 0; index < 56; index++) processor.process([], [[new Float32Array(128)]]);
  const queue = processor.publishers.get(PUB_A).queue;
  assert.equal(queue.some(entry => entry.timestamp === 48_000_000n), false, 'older arrival expires even when sorted behind a newer arrival');
  assert.equal(queue.some(entry => entry.timestamp === 48_000n), true);
});

test('codec worker correlates Opus output across capture discontinuities like WebCodecs does', async t => {
  // Chromium's Opus AudioEncoder stamps each output as first input timestamp
  // plus encoded duration; a gap in capture sample timestamps (dropped credit,
  // unmute, input switch) must not turn into a fatal "no capture metadata".
  const previousSelf = globalThis.self;
  const posted = [];
  class AudioData { constructor(init) { Object.assign(this, init); } close() {} }
  class EncodedAudioChunk {
    constructor(init) { Object.assign(this, init); this.byteLength = init.data.byteLength; }
    copyTo(target) { target.set(this.data); }
  }
  class AudioEncoder {
    constructor(callbacks) { Object.assign(this, callbacks); this.encodeQueueSize = 0; AudioEncoder.instance = this; }
    configure() {}
    encode(frame) {
      this.first ??= frame.timestamp;
      const timestamp = this.first + 20_000 * (this.count = (this.count ?? -1) + 1);
      this.output(new EncodedAudioChunk({type: 'key', timestamp, duration: 20_000, data: Uint8Array.of(0x78, this.count, 0)}));
    }
    close() {}
  }
  class AudioDecoder { constructor() {} configure() {} close() {} }
  globalThis.self = {AudioData, AudioEncoder, AudioDecoder, EncodedAudioChunk, postMessage(message) { posted.push(message); }, close() {}};
  t.after(() => { globalThis.self = previousSelf; });
  await import(`./voice-audio-worker.mjs?discontinuity=${Date.now()}`);
  self.onmessage({data: {type: 'init', encoderConfig: {codec: 'opus'}, decoderConfig: {codec: 'opus'}}});
  posted.length = 0;
  // Sample timestamps with a one-frame gap after the second frame.
  for (const sample of [12_345n, 13_305n, 15_225n, 16_185n]) {
    self.onmessage({data: {type: 'encode', samples: new Float32Array(960).buffer, timestamp: sample, duration: 960}});
  }
  assert.equal(posted.filter(message => message.type === 'error').length, 0);
  assert.deepEqual(posted.filter(message => message.type === 'encoded').map(message => message.timestamp), [12_345n, 13_305n, 15_225n, 16_185n]);
});

test('codec worker keeps decoding when the decoder shifts output timestamps', async t => {
  // Some WebCodecs Opus decoders stamp outputs from their own clock (codec
  // delay/concealment); correlation must fall back to arrival order instead of
  // dropping the publisher every few hundred milliseconds.
  const previousSelf = globalThis.self;
  const posted = [];
  class AudioData {
    constructor(init) { Object.assign(this, init); }
    copyTo(target) { target.fill(0); }
    close() {}
  }
  class EncodedAudioChunk { constructor(init) { Object.assign(this, init); } }
  class AudioEncoder { constructor() {} configure() {} close() {} }
  class AudioDecoder {
    constructor(callbacks) { Object.assign(this, callbacks); this.decodeQueueSize = 0; }
    configure() {}
    decode(chunk) { this.output(new AudioData({timestamp: chunk.timestamp + 6_500, numberOfFrames: 960, sampleRate: 48_000, numberOfChannels: 1})); }
    close() {}
  }
  globalThis.self = {AudioData, AudioEncoder, AudioDecoder, EncodedAudioChunk, postMessage(message) { posted.push(message); }, close() {}};
  t.after(() => { globalThis.self = previousSelf; });
  await import(`./voice-audio-worker.mjs?decoder-shift=${Date.now()}`);
  self.onmessage({data: {type: 'init', encoderConfig: {codec: 'opus'}, decoderConfig: {codec: 'opus'}}});
  posted.length = 0;
  for (let index = 0; index < 4; index++) {
    self.onmessage({data: {type: 'decode', publisherId: PUB_A, generation: 1, payload: Uint8Array.of(1, 2).buffer, timestamp: BigInt(index * 960), duration: 960}});
  }
  assert.equal(posted.filter(message => message.type === 'error').length, 0);
  assert.deepEqual(posted.filter(message => message.type === 'decoded').map(message => message.timestamp), [0n, 960n, 1920n, 2880n]);
});

test('adaptive worklet playout shrinks to 20 ms on a steady link and grows on a jittery one', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?adaptive-jitter=${Date.now()}`);
  const jitter = {adaptive: true, initial: 1920, min: 960, max: 7680};
  const run = (arrivalOffsets) => {
    const processor = new Processor({processorOptions: {muted: true, jitter}});
    let quanta = 0;
    const advanceTo = sample => { while (quanta * 128 < sample) { processor.process([], [[new Float32Array(128)]]); quanta++; } };
    arrivalOffsets.forEach((offset, index) => {
      advanceTo(index * 960 + offset);
      processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).buffer, timestamp: BigInt(index * 960), duration: 960, reservedSamples: 960});
    });
    // A forward media-clock discontinuity (sender restart) after the queue
    // drained forces a resynchronisation at the current adaptive target.
    advanceTo(arrivalOffsets.length * 960 + 48_000);
    processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).buffer, timestamp: BigInt(arrivalOffsets.length * 960 + 96_000), duration: 960, reservedSamples: 960});
    return processor.publishers.get(PUB_A).target;
  };
  assert.equal(run(Array.from({length: 100}, () => 0)), 960, 'steady arrivals retarget to the 20 ms floor');
  const jittery = run(Array.from({length: 100}, (_, index) => (index * 7919) % 2880));
  // The target is the 95th percentile of the delay spread (render-quantum
  // arrival granularity) plus half a frame: ~50-70 ms for 0-60 ms uniform jitter.
  assert.ok(jittery >= 2400 && jittery <= 3360, `60 ms uniform jitter retargets to its 95th percentile (${jittery})`);
});

test('worklet conceals a single missing frame with the previous frame instead of silence', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?plc=${Date.now()}`);
  const processor = new Processor({processorOptions: {muted: true, jitter: {adaptive: true, initial: 1920, min: 960, max: 7680}}});
  const tone = () => Float32Array.from({length: 960}, (_, index) => 0.5 * Math.sin(2 * Math.PI * 700 * index / 48_000));
  const outputs = [];
  let quanta = 0;
  const advanceTo = sample => {
    while (quanta * 128 < sample) {
      const block = new Float32Array(128);
      processor.process([], [[block]]);
      outputs.push(block);
      quanta++;
    }
  };
  // Frames 0..9 arrive on time except frame 5, which is lost.
  for (let index = 0; index < 10; index++) {
    advanceTo(index * 960);
    if (index === 5) continue;
    processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: tone().buffer, timestamp: BigInt(index * 960), duration: 960, reservedSamples: 960});
  }
  advanceTo(20 * 960);
  const rendered = new Float32Array(outputs.length * 128);
  outputs.forEach((block, index) => rendered.set(block, index * 128));
  // Find the longest run of exact silence between the first and last audible sample.
  const first = rendered.findIndex(value => value !== 0);
  let last = rendered.length - 1;
  while (last > 0 && rendered[last] === 0) last--;
  let run = 0, longest = 0;
  for (let index = first; index <= last; index++) {
    if (Math.abs(rendered[index]) < 1e-6) { run++; longest = Math.max(longest, run); } else run = 0;
  }
  assert.ok(longest < 96, `a lost frame must not leave a hard dropout (longest silent run ${longest} samples)`);
});

test('worklet keeps a DTX publisher\'s comfort noise between its 400 ms updates and a loud gap still fades', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?cng=${Date.now()}`);
  const render = frames => {
    const processor = new Processor({processorOptions: {muted: true, jitter: {adaptive: true, initial: 1920, min: 960, max: 7680}}});
    const outputs = [];
    let quanta = 0;
    const advanceTo = sample => {
      while (quanta * 128 < sample) {
        const block = new Float32Array(128);
        processor.process([], [[block]]);
        outputs.push(block);
        quanta++;
      }
    };
    for (const [frame, samples] of frames) {
      advanceTo(frame * 960);
      processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: samples.buffer, timestamp: BigInt(frame * 960), duration: 960, reservedSamples: 960});
    }
    advanceTo((frames.at(-1)[0] + 10) * 960);
    const rendered = new Float32Array(outputs.length * 128);
    outputs.forEach((block, index) => rendered.set(block, index * 128));
    return rendered;
  };
  const rms = (samples, from, to) => Math.sqrt(samples.subarray(from, to).reduce((sum, value) => sum + value * value, 0) / (to - from));
  // Low-level noise, as a comfort-noise update decodes to.
  let seed = 7;
  const noise = () => Float32Array.from({length: 960}, () => { seed = (seed * 1103515245 + 12345) % 2147483648; return 0.004 * (seed / 2147483648 - 0.5); });
  const level = rms(noise(), 0, 960);
  // Updates at frames 0, 20 and 40 only, as a silent DTX publisher sends.
  const quiet = render([[0, noise()], [20, noise()], [40, noise()]]);
  // Between updates the noise floor holds (no drop to digital silence).
  for (const frame of [5, 10, 15, 25, 35]) {
    const from = (frame + 2) * 960;
    assert.ok(rms(quiet, from, from + 960) > level * 0.5, `comfort noise holds at frame ${frame}`);
  }
  // A loud frame followed by a gap still fades out within two frames.
  const tone = Float32Array.from({length: 960}, (_, index) => 0.5 * Math.sin(2 * Math.PI * 700 * index / 48_000));
  const loud = render([[0, tone], [1, tone], [30, tone]]);
  assert.equal(rms(loud, 10 * 960, 20 * 960), 0, 'speech is concealed briefly, never looped');
});

test('worklet trims excess playout delay by dropping a quiet frame', async t => {
  const previousProcessor = globalThis.AudioWorkletProcessor;
  const previousRegister = globalThis.registerProcessor;
  let Processor;
  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    receive(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
  }
  globalThis.AudioWorkletProcessor = class { constructor() { this.port = new Port(); } };
  globalThis.registerProcessor = (_name, implementation) => { Processor = implementation; };
  t.after(() => { globalThis.AudioWorkletProcessor = previousProcessor; globalThis.registerProcessor = previousRegister; });
  await import(`./voice-audio-worklet.mjs?trim=${Date.now()}`);
  const processor = new Processor({processorOptions: {muted: true, jitter: {adaptive: true, initial: 7680, min: 960, max: 7680}}});
  let quanta = 0;
  const advanceTo = sample => { while (quanta * 128 < sample) { processor.process([], [[new Float32Array(128)]]); quanta++; } };
  // A steady link but a 160 ms startup mapping: continuous quiet frames must
  // bring the offset down to the adaptive target without the queue draining.
  for (let index = 0; index < 150; index++) {
    advanceTo(index * 960);
    processor.port.receive({type: 'play', publisherId: PUB_A, generation: 1, samples: new Float32Array(960).buffer, timestamp: BigInt(index * 960), duration: 960, reservedSamples: 960});
  }
  const publisher = processor.publishers.get(PUB_A);
  const offset = Number(publisher.baseLocalTimestamp - publisher.baseRemoteTimestamp);
  assert.ok(offset <= 960 * 2.5, `offset trimmed toward the 20 ms target (${offset} samples)`);
  assert.ok(processor.port.sent.some(message => message.stage === 'playback-latency-trim'));
});

import test from 'node:test';
import assert from 'node:assert/strict';

import {createBrowserAudio} from './voice-audio.mjs';

const PUB_A = 'a'.repeat(64);
const PUB_B = 'b'.repeat(64);

const drain = async () => {
  for (let index = 0; index < 8; index++) await Promise.resolve();
};

function codecEnvironment(overrides = {}) {
  class AudioData {
    constructor(init) {
      Object.assign(this, init);
      this.numberOfFrames = init.numberOfFrames;
      this.numberOfChannels = init.numberOfChannels;
      this._data = Float32Array.from(init.data);
    }
    copyTo(target) { target.set(this._data); }
    close() { this.closed = true; }
  }
  class EncodedAudioChunk {
    constructor(init) { Object.assign(this, init); this.byteLength = init.data.byteLength; }
    copyTo(target) { target.set(this.data); }
  }
  class AudioEncoder {
    static async isConfigSupported(config) { return {supported: true, config}; }
    constructor(callbacks) { Object.assign(this, callbacks); this.encodeQueueSize = 0; }
    configure(config) { this.config = config; AudioEncoder.lastConfig = config; }
    encode(frame) {
      this.output(new EncodedAudioChunk({type: 'key', timestamp: frame.timestamp, duration: 20_000, data: Uint8Array.of(0xf8, 0xff, 0xfe)}));
    }
    async flush() {}
    close() { this.closed = true; }
  }
  class AudioDecoder {
    static async isConfigSupported(config) { return {supported: true, config}; }
    constructor(callbacks) { Object.assign(this, callbacks); this.decodeQueueSize = 0; }
    configure(config) { this.config = config; }
    decode(chunk) {
      this.output(new AudioData({format: 'f32-planar', sampleRate: 48_000, numberOfFrames: 960, numberOfChannels: 1, timestamp: chunk.timestamp, data: new Float32Array(960)}));
    }
    async flush() {}
    close() { this.closed = true; }
  }
  return {AudioData, EncodedAudioChunk, AudioEncoder, AudioDecoder, ...overrides};
}

function lifecycleEnvironment({holdEncoding = false, holdDecoding = false} = {}) {
  const events = [], contexts = [], workers = [], worklets = [], streams = [];

  class Port extends EventTarget {
    sent = [];
    postMessage(message) { this.sent.push(message); }
    emit(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
    start() {}
    close() { this.closed = true; }
  }
  class Node extends EventTarget {
    constructor(kind) { super(); this.kind = kind; this.connections = []; }
    connect(target) { this.connections.push(target); return target; }
    disconnect() { this.disconnected = true; this.connections.length = 0; }
  }
  class AudioWorkletNode extends Node {
    constructor(context, name, options) {
      super('worklet'); this.context = context; this.name = name; this.options = options; this.port = new Port(); worklets.push(this);
    }
  }
  class AudioContext {
    constructor(options) {
      this.options = options; this.destination = new Node('destination');
      this.audioWorklet = {addModule: async url => { this.moduleUrl = String(url); }};
      contexts.push(this); events.push('context');
    }
    resume() { events.push('resume'); this.resumed = true; return Promise.resolve(); }
    createMediaStreamSource(stream) { const node = new Node('source'); node.stream = stream; return node; }
    createGain() { const node = new Node('gain'); node.gain = {value: 1}; return node; }
    close() { this.closed = true; return Promise.resolve(); }
  }
  class Worker extends EventTarget {
    constructor(url, options) { super(); this.url = String(url); this.options = options; this.sent = []; workers.push(this); }
    postMessage(message) {
      this.sent.push(message);
      if (message.type === 'init') queueMicrotask(() => this.emit({type: 'ready'}));
      if (message.type === 'encode' && !holdEncoding) queueMicrotask(() => {
        this.emit({type: 'encoded', payload: Uint8Array.of(1, 2, 3).buffer, timestamp: message.timestamp, duration: 960, generation: message.generation});
        this.emit({type: 'capture-consumed'});
      });
      if (message.type === 'decode' && !holdDecoding) queueMicrotask(() => this.emit({type: 'decoded', samples: new Float32Array(960).buffer, publisherId: message.publisherId, generation: message.generation, timestamp: message.timestamp, duration: message.duration, reservedSamples: message.duration}));
    }
    emit(data) { this.dispatchEvent(Object.assign(new Event('message'), {data})); }
    fail(error) { this.dispatchEvent(Object.assign(new Event('error'), {error, message: error.message})); }
    terminate() { this.terminated = true; }
  }
  function stream(id) {
    class Track extends EventTarget {
      constructor() { super(); this.id = id; }
      stop() { this.stopped = true; }
      getSettings() { return {deviceId: id, sampleRate: 48_000, channelCount: 1}; }
    }
    const track = new Track();
    const value = {id, track, getTracks: () => [track]}; streams.push(value); return value;
  }
  const environment = codecEnvironment({
    AudioContext,
    AudioWorkletNode,
    Worker,
    navigator: {mediaDevices: {getUserMedia: async constraints => {
      events.push(`media:${constraints.audio.deviceId?.exact ?? 'default'}`);
      return stream(constraints.audio.deviceId?.exact ?? 'default');
    }}},
  });
  return {environment, events, contexts, workers, worklets, streams};
}

test('capabilities report unsupported instead of simulating a codec', async () => {
  const audio = createBrowserAudio({environment: {}});
  assert.deepEqual(await audio.capabilities(), {
    supported: false,
    capture: false,
    playback: false,
    qualified: false,
    codec: 'opus',
    sampleRate: 48_000,
    channels: 1,
    frameSamples: 960,
    bitrate: 32_000,
    dtx: false,
    reason: 'WebCodecs Opus audio is unavailable',
  });
});

test('start rejects missing WebCodecs with an explicit unsupported result', async () => {
  await assert.rejects(
    createBrowserAudio({environment: {}}).start({muted: true, onFrame() {}}),
    /WebCodecs Opus audio is unavailable/,
  );
});

test('capabilities qualify a real Opus encode and decode without opening input', async () => {
  let mediaCalls = 0;
  const f = lifecycleEnvironment();
  f.environment.navigator.mediaDevices.getUserMedia = async () => { mediaCalls++; throw Error('must not request permission'); };
  const result = await createBrowserAudio({environment: f.environment}).capabilities();
  assert.deepEqual(result, {
    supported: true,
    capture: true,
    playback: true,
    qualified: true,
    codec: 'opus',
    sampleRate: 48_000,
    channels: 1,
    frameSamples: 960,
    bitrate: 32_000,
    dtx: true,
    reason: null,
  });
  assert.deepEqual(f.environment.AudioEncoder.lastConfig.opus, {format: 'opus', frameDuration: 20_000});
  assert.equal(mediaCalls, 0);
});

test('the call encoder uses Opus DTX only where the browser confirms it', async () => {
  for (const [echo, dtx] of [
    [config => config, true],
    // An encoder that does not know usedtx leaves it out of its answer.
    [config => ({...config, opus: {format: config.opus.format, frameDuration: config.opus.frameDuration}}), false],
  ]) {
    const f = lifecycleEnvironment();
    f.environment.AudioEncoder.isConfigSupported = async config => ({supported: true, config: echo(config)});
    const audio = createBrowserAudio({environment: f.environment});
    assert.equal((await audio.capabilities()).dtx, dtx);
    const session = await audio.start({muted: true, onFrame() {}});
    const init = f.workers[0].sent.find(message => message.type === 'init');
    assert.equal(init.encoderConfig.opus.usedtx === true, dtx);
    assert.equal(init.encoderConfig.bitrate, 32_000);
    await session.stop();
  }
});

test('capabilities fail closed when the advertised decoder cannot complete qualification', async () => {
  const f = lifecycleEnvironment();
  f.environment.AudioDecoder = class {
    static async isConfigSupported(config) { return {supported: true, config}; }
    configure() {}
    decode() {}
    async flush() {}
    close() {}
  };
  const result = await createBrowserAudio({environment: f.environment}).capabilities();
  assert.equal(result.supported, false);
  assert.equal(result.capture, false);
  assert.equal(result.playback, false);
  assert.equal(result.qualified, false);
  assert.match(result.reason, /qualification failed: Opus decoder produced no audio/);
});

test('capabilities reject decoded audio that does not exactly match the fixed voice profile', async () => {
  const f = lifecycleEnvironment();
  f.environment.AudioDecoder = class {
    static async isConfigSupported(config) { return {supported: true, config}; }
    constructor(callbacks) { Object.assign(this, callbacks); }
    configure() {}
    decode() { this.output({numberOfFrames: 480, sampleRate: 48_000, numberOfChannels: 1, close() {}}); }
    async flush() {}
    close() {}
  };
  const result = await createBrowserAudio({environment: f.environment}).capabilities();
  assert.equal(result.supported, false);
  assert.match(result.reason, /qualification failed: Decoded Opus did not match 48 kHz mono 960-sample audio/);
});

test('start preserves gesture resume, listening-only mute skips microphone consent, and unmute captures raw Opus', async () => {
  const f = lifecycleEnvironment(), frames = [];
  const session = await createBrowserAudio({environment: f.environment}).start({
    inputId: 'mic-a', muted: true, onFrame: frame => frames.push(frame),
  });
  assert.deepEqual(f.events, ['context', 'resume']);
  assert.equal(f.streams.length, 0);
  const pcm = new Float32Array(960);
  f.worklets[0].port.emit({type: 'capture', samples: pcm.buffer, timestamp: 0n, duration: 960});
  await drain();
  assert.deepEqual(frames, []);
  assert.equal(f.workers[0].sent.filter(message => message.type === 'encode').length, 0);

  await session.setMuted(false);
  assert.deepEqual(f.events, ['context', 'resume', 'media:mic-a']);
  const generation = f.worklets[0].port.sent.findLast(message => message.type === 'muted').generation;
  f.worklets[0].port.emit({type: 'capture', samples: new Float32Array(960).buffer, timestamp: 960n, duration: 960, generation});
  await drain();
  assert.deepEqual(frames, [{payload: Uint8Array.of(1, 2, 3), timestamp: 960n, duration: 960}]);
  await session.stop();
});

test('start defaults to muted and does not publish capture until explicitly unmuted', async () => {
  const f = lifecycleEnvironment(), frames = [];
  const session = await createBrowserAudio({environment: f.environment}).start({onFrame: frame => frames.push(frame)});
  assert.equal(f.streams.length, 0);
  assert.equal(f.events.some(event => event.startsWith('media:')), false);
  f.worklets[0].port.emit({type: 'capture', samples: new Float32Array(960).buffer, timestamp: 0n, duration: 960});
  await drain();
  assert.deepEqual(frames, []);
  assert.equal(f.workers[0].sent.some(message => message.type === 'encode'), false);
  await session.stop();
});

test('mute cancels a pending unmute before microphone acquisition can attach', async () => {
  const f = lifecycleEnvironment();
  const acquire = f.environment.navigator.mediaDevices.getUserMedia;
  let releaseInput;
  f.environment.navigator.mediaDevices.getUserMedia = constraints => new Promise(resolve => {
    releaseInput = async () => resolve(await acquire(constraints));
  });
  const session = await createBrowserAudio({environment: f.environment}).start({muted: true, onFrame() {}});
  const unmuting = session.setMuted(false);
  for (let index = 0; index < 16 && !releaseInput; index++) await Promise.resolve();
  await session.setMuted(true);
  await releaseInput();
  await unmuting;
  assert.equal(f.streams[0].track.stopped, true);
  assert.equal(f.worklets[0].port.sent.some(message => message.type === 'muted' && message.value === false), false);
  await session.stop();
});

test('selection releases the replaced input and stop immediately releases every owned resource', async () => {
  const f = lifecycleEnvironment(), abort = new AbortController();
  const session = await createBrowserAudio({environment: f.environment}).start({inputId: 'mic-a', muted: false, onFrame() {}, signal: abort.signal});
  await session.selectInput('mic-b');
  assert.equal(f.streams[0].track.stopped, true);
  assert.equal(f.streams[1].track.stopped, undefined);
  const stopping = session.stop();
  assert.equal(f.streams[1].track.stopped, true, 'track closes before asynchronous context shutdown settles');
  assert.equal(f.workers[0].terminated, true);
  assert.equal(f.worklets[0].disconnected, true);
  await stopping;
  assert.equal(f.contexts[0].closed, true);
  await session.stop();
  abort.abort();
  assert.equal(f.workers.length, 1);
});

test('failed input graph replacement closes the new stream and retains the current input', async () => {
  const f = lifecycleEnvironment(), reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({inputId: 'mic-a', muted: false, onFrame() {}});
  const createSource = f.contexts[0].createMediaStreamSource.bind(f.contexts[0]);
  f.contexts[0].createMediaStreamSource = stream => {
    if (stream.id === 'mic-b') throw new Error('input graph rejected');
    return createSource(stream);
  };
  await assert.rejects(session.selectInput('mic-b'), /input graph rejected/);
  assert.equal(f.streams[0].track.stopped, undefined);
  assert.equal(f.streams[1].track.stopped, true);
  f.streams[0].track.dispatchEvent(new Event('ended'));
  assert.equal(f.streams[0].track.stopped, true);
  assert(reports.some(report => report.stage === 'capture-track-ended'), 'retained input remains observed after a failed replacement');
  await session.stop();
});

test('abort owns the same idempotent immediate stop path', async () => {
  const f = lifecycleEnvironment(), abort = new AbortController();
  await createBrowserAudio({environment: f.environment}).start({muted: false, onFrame() {}, signal: abort.signal});
  abort.abort();
  assert.equal(f.streams[0].track.stopped, true);
  assert.equal(f.workers[0].terminated, true);
  await drain();
  assert.equal(f.contexts[0].closed, true);
});

test('abort during input acquisition settles start and closes allocated resources immediately', async () => {
  const f = lifecycleEnvironment(), abort = new AbortController();
  const acquire = f.environment.navigator.mediaDevices.getUserMedia;
  let releaseInput;
  f.environment.navigator.mediaDevices.getUserMedia = constraints => new Promise(resolve => {
    releaseInput = async () => resolve(await acquire(constraints));
  });
  const starting = createBrowserAudio({environment: f.environment}).start({muted: false, onFrame() {}, signal: abort.signal});
  for (let index = 0; index < 32 && !releaseInput; index++) await Promise.resolve();
  assert.equal(typeof releaseInput, 'function');
  abort.abort();
  let settled = false;
  starting.catch(() => { settled = true; });
  await drain();
  const settledBeforeInput = settled;
  await releaseInput();
  await assert.rejects(starting);
  assert.equal(settledBeforeInput, true);
  assert.equal(f.workers[0].terminated, true);
  assert.equal(f.contexts[0].closed, true);
  assert.equal(f.streams[0].track.stopped, true);
});

test('play transfers decoded voice payload through the worker into the worklet buffer', async () => {
  const f = lifecycleEnvironment();
  const session = await createBrowserAudio({environment: f.environment}).start({muted: true, onFrame() {}});
  assert.equal(f.worklets[0].connections[0], f.contexts[0].destination, 'worklet playout must reach the audible destination');
  session.play({publisherId: PUB_A, sequence: 7n, timestamp: 1920n, duration: 960, payload: Uint8Array.of(9, 8, 7)});
  await drain();
  const decode = f.workers[0].sent.find(message => message.type === 'decode');
  assert.deepEqual(new Uint8Array(decode.payload), Uint8Array.of(9, 8, 7));
  assert.equal(decode.publisherId, PUB_A);
  assert.equal(decode.timestamp, 1920n);
  const playback = f.worklets[0].port.sent.find(message => message.type === 'play');
  assert.equal(playback.publisherId, PUB_A);
  assert.equal(playback.timestamp, 1920n);
  assert.equal(new Float32Array(playback.samples).length, 960);
  await session.stop();
});

test('play reserves at most 200 ms across the entire decode and playout pipeline', async () => {
  const f = lifecycleEnvironment({holdDecoding: true}), reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({muted: true, onFrame() {}});
  for (let index = 0; index < 10; index++) {
    assert.equal(session.play({publisherId: PUB_A, timestamp: BigInt(index * 960), duration: 960, payload: Uint8Array.of(index)}), true);
  }
  assert.equal(session.play({publisherId: PUB_A, timestamp: 9600n, duration: 960, payload: Uint8Array.of(10)}), false);
  assert.equal(session.play({publisherId: PUB_B, timestamp: 0n, duration: 960, payload: Uint8Array.of(11)}), true, 'another publisher owns an independent 200 ms reservation');
  assert.equal(f.workers[0].sent.filter(message => message.type === 'decode').length, 11);
  assert.deepEqual(reports.at(-1), {type: 'drop', stage: 'playback-admission', count: 1});
  f.worklets[0].port.emit({type: 'playback-consumed', publisherId: PUB_A, generation: 1, samples: 960});
  assert.equal(session.play({publisherId: PUB_A, timestamp: 9600n, duration: 960, payload: Uint8Array.of(10)}), true);
  await session.stop();
});

test('remote publisher admission is capped at seven and removal reclaims one isolated track', async () => {
  const f = lifecycleEnvironment({holdDecoding: true}), reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({onFrame() {}});
  const publishers = 'abcdef0'.split('').map(value => value.repeat(64));
  for (const publisherId of publishers) {
    assert.equal(session.play({publisherId, timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}), true);
  }
  const eighth = '8'.repeat(64);
  assert.equal(session.play({publisherId: eighth, timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}), false);
  assert.deepEqual(reports.at(-1), {type: 'drop', stage: 'playback-publisher-limit', count: 1});
  const removedDecode = f.workers[0].sent.find(message => message.type === 'decode' && message.publisherId === publishers[0]);
  session.removePublisher(publishers[0]);
  assert(f.workers[0].sent.some(message => message.type === 'remove-publisher' && message.publisherId === publishers[0]));
  assert(f.worklets[0].port.sent.some(message => message.type === 'remove-publisher' && message.publisherId === publishers[0]));
  assert.equal(session.play({publisherId: eighth, timestamp: 0n, duration: 960, payload: Uint8Array.of(2)}), true);
  const before = f.worklets[0].port.sent.length;
  f.workers[0].emit({type: 'decoded', publisherId: publishers[0], generation: removedDecode.generation, samples: new Float32Array(960).buffer, timestamp: 0n, duration: 960, reservedSamples: 960});
  assert.equal(f.worklets[0].port.sent.length, before, 'removed publisher cannot deliver late PCM');
  await session.stop();
});

test('play rejects unvalidated publisher identities without allocating a decoder track', async () => {
  const f = lifecycleEnvironment(), reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({onFrame() {}});
  assert.equal(session.play({publisherId: 'A'.repeat(64), timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}), false);
  assert.equal(f.workers[0].sent.some(message => message.type === 'decode'), false);
  assert.deepEqual(reports.at(-1), {type: 'drop', stage: 'playback-invalid', count: 1});
  assert.equal(session.play({publisherId: PUB_A, timestamp: -1n, duration: 960, payload: Uint8Array.of(1)}), false);
  assert.equal(session.play({publisherId: PUB_A, timestamp: 0x1_0000_0000_0000_0000n, duration: 960, payload: Uint8Array.of(1)}), false);
  await session.stop();
});

test('worker failure after start is terminal and immediately releases audio resources', async () => {
  const f = lifecycleEnvironment(), reports = [];
  await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({muted: false, onFrame() {}});
  f.workers[0].fail(new Error('codec worker crashed'));
  assert.equal(f.streams[0].track.stopped, true);
  assert.equal(f.workers[0].terminated, true);
  await drain();
  assert.equal(f.contexts[0].closed, true);
  assert.deepEqual(reports.at(-1), {type: 'error', stage: 'codec-worker', error: 'codec worker crashed'});
});

test('one publisher decoder failure removes only that remote track', async () => {
  const f = lifecycleEnvironment({holdDecoding: true}), reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({muted: false, onFrame() {}});
  assert.equal(session.play({publisherId: PUB_A, timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}), true);
  assert.equal(session.play({publisherId: PUB_B, timestamp: 0n, duration: 960, payload: Uint8Array.of(2)}), true);
  const generation = f.workers[0].sent.find(message => message.type === 'decode' && message.publisherId === PUB_A).generation;
  f.workers[0].emit({type: 'error', stage: 'decoder', publisherId: PUB_A, generation, error: 'bad remote packet'});
  assert.equal(f.streams[0].track.stopped, undefined);
  assert.equal(f.workers[0].terminated, undefined);
  assert(f.workers[0].sent.some(message => message.type === 'remove-publisher' && message.publisherId === PUB_A));
  assert.equal(session.play({publisherId: PUB_B, timestamp: 960n, duration: 960, payload: Uint8Array.of(3)}), true);
  assert.deepEqual(reports.at(-1), {type: 'error', stage: 'decoder', publisherId: PUB_A, generation, error: 'bad remote packet'});
  await session.stop();
});

test('stale decoder error cannot remove a recreated publisher generation', async () => {
  const f = lifecycleEnvironment({holdDecoding: true}), reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({onFrame() {}});
  session.play({publisherId: PUB_A, timestamp: 0n, duration: 960, payload: Uint8Array.of(1)});
  const oldGeneration = f.workers[0].sent.find(message => message.type === 'decode' && message.publisherId === PUB_A).generation;
  session.removePublisher(PUB_A);
  session.play({publisherId: PUB_A, timestamp: 960n, duration: 960, payload: Uint8Array.of(2)});
  const currentGeneration = f.workers[0].sent.findLast(message => message.type === 'decode' && message.publisherId === PUB_A).generation;
  f.workers[0].emit({type: 'error', stage: 'decoder', publisherId: PUB_A, generation: oldGeneration, error: 'late old decoder error'});
  assert.notEqual(oldGeneration, currentGeneration);
  assert.equal(f.workers[0].sent.some(message => message.type === 'remove-publisher' && message.generation === currentGeneration), false);
  assert.equal(reports.some(report => report.error === 'late old decoder error'), false);
  assert.equal(session.play({publisherId: PUB_A, timestamp: 1920n, duration: 960, payload: Uint8Array.of(3)}), true);
  await session.stop();
});

test('track end disables only local capture and processor failure closes the full audio graph', async () => {
  const f = lifecycleEnvironment(), reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({muted: false, onFrame() {}});
  f.streams[0].track.dispatchEvent(new Event('ended'));
  assert.equal(f.streams[0].track.stopped, true);
  assert.equal(f.workers[0].terminated, undefined, 'remote playback remains available after local device loss');
  assert(reports.some(report => report.stage === 'capture-track-ended'));
  await session.setMuted(false);
  assert.equal(f.streams.length, 2, 'explicit unmute can reacquire a replacement device');
  f.worklets[0].dispatchEvent(new Event('processorerror'));
  assert.equal(f.streams[1].track.stopped, true);
  assert.equal(f.workers[0].terminated, true);
  await drain();
  assert.equal(f.contexts[0].closed, true);
  assert(reports.some(report => report.stage === 'audio-worklet'));
});

test('capture transfer queue is bounded and reports overload drops', async () => {
  const f = lifecycleEnvironment({holdEncoding: true}), reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({muted: false, onFrame() {}});
  const generation = f.worklets[0].port.sent.findLast(message => message.type === 'capture-generation').generation;
  for (let index = 0; index < 8; index++) {
    f.worklets[0].port.emit({type: 'capture', samples: new Float32Array(960).buffer, timestamp: BigInt(index * 960), duration: 960, generation});
  }
  assert.equal(f.workers[0].sent.filter(message => message.type === 'encode').length, 4);
  assert.equal(reports.filter(report => report.type === 'drop' && report.stage === 'capture-transfer').length, 4);
  await session.stop();
});

test('mute generations discard already queued capture even after unmuting again', async () => {
  const f = lifecycleEnvironment({holdEncoding: true}), frames = [];
  const session = await createBrowserAudio({environment: f.environment}).start({muted: false, onFrame: frame => frames.push(frame)});
  const oldGeneration = f.worklets[0].port.sent.findLast(message => message.type === 'capture-generation').generation;
  f.worklets[0].port.emit({type: 'capture', samples: new Float32Array(960).buffer, timestamp: 0n, duration: 960, generation: oldGeneration});
  const queued = f.workers[0].sent.find(message => message.type === 'encode');
  await session.setMuted(true);
  await session.setMuted(false);
  f.workers[0].emit({type: 'encoded', payload: Uint8Array.of(1).buffer, timestamp: 0n, duration: 960, generation: queued.generation});
  f.workers[0].emit({type: 'capture-consumed'});
  assert.deepEqual(frames, []);
  const encodesBeforeLateCapture = f.workers[0].sent.filter(message => message.type === 'encode').length;
  f.worklets[0].port.emit({type: 'capture', samples: new Float32Array(960).buffer, timestamp: 960n, duration: 960, generation: oldGeneration});
  assert.equal(f.workers[0].sent.filter(message => message.type === 'encode').length, encodesBeforeLateCapture, 'pre-mute worklet PCM cannot be relabeled after unmute');
  await session.stop();
});

test('stop suppresses codec messages that were already queued for delivery', async () => {
  const f = lifecycleEnvironment({holdEncoding: true}), frames = [], reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({muted: false, onFrame: frame => frames.push(frame)});
  const generation = f.worklets[0].port.sent.findLast(message => message.type === 'capture-generation').generation;
  f.worklets[0].port.emit({type: 'capture', samples: new Float32Array(960).buffer, timestamp: 0n, duration: 960, generation});
  const queued = f.workers[0].sent.find(message => message.type === 'encode');
  reports.length = 0;
  await session.stop();
  f.workers[0].emit({type: 'encoded', payload: Uint8Array.of(1).buffer, timestamp: 0n, duration: 960, generation: queued.generation});
  f.workers[0].fail(new Error('late worker failure'));
  f.worklets[0].port.emit({type: 'error', stage: 'late-worklet', error: 'already stopped'});
  assert.deepEqual(frames, []);
  assert.deepEqual(reports, []);
});

test('worklet drop reports are surfaced without being presented as successful playback', async () => {
  const f = lifecycleEnvironment(), reports = [];
  const session = await createBrowserAudio({environment: f.environment, onReport: report => reports.push(report)}).start({muted: true, onFrame() {}});
  f.worklets[0].port.emit({type: 'drop', stage: 'playback-buffer', count: 3});
  assert.deepEqual(reports, [{type: 'drop', stage: 'playback-buffer', count: 3}]);
  await session.stop();
});

test('a microphone that goes away hands capture to the default one without failing the call', async () => {
  const f = lifecycleEnvironment(), errors = [], lost = [], inputs = [];
  const session = await createBrowserAudio({environment: f.environment}).start({inputId: 'usb-mic', muted: false, onFrame() {},
    onError: error => errors.push(error), onCaptureLost: error => lost.push(error), onInputChanged: id => inputs.push(id)});
  assert.equal(f.streams[0].id, 'usb-mic');
  f.streams[0].track.dispatchEvent(new Event('ended'));
  await drain();
  assert.deepEqual(errors, [], 'losing the microphone is not an audio failure');
  assert.deepEqual(inputs, [undefined]);
  assert.equal(f.streams[1].id, 'default', 'the default microphone took its place');
  assert.equal(session.captureActive, true);
  assert.equal(session.inputId, undefined);
  // The default one ends at once as well: no microphone is left.
  f.streams[1].track.dispatchEvent(new Event('ended'));
  await drain();
  assert.equal(lost.length, 1);
  assert.equal(lost[0].name, 'NotFoundError');
  assert.equal(session.captureActive, false);
  assert.equal(f.streams.length, 2, 'no endless reacquiring');
  assert.deepEqual(errors, []);
  assert.equal(f.workers[0].terminated, undefined, 'the others are still heard');
  await session.stop();
});

test('a microphone that goes away while muted asks for nothing', async () => {
  const f = lifecycleEnvironment(), lost = [];
  const session = await createBrowserAudio({environment: f.environment}).start({muted: false, onFrame() {}, onCaptureLost: error => lost.push(error)});
  await session.setMuted(true);
  f.streams[0].track.dispatchEvent(new Event('ended'));
  await drain();
  assert.equal(f.streams.length, 1);
  assert.deepEqual(lost, []);
  await session.stop();
});

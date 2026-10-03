// Page backend for tilo-capture: getUserMedia / getDisplayMedia.
//
// Video: the stream plays into a hidden <video>, and pictures reach Rust as
// RGBA through a 2D canvas (the native backends' frame contract). Audio: an
// AudioWorklet posts mono Float32 blocks, buffered here up to a bound.
// Every request is asynchronous (the browser asks the user first); Rust
// polls `state()`.

const MAX_BUFFERED_SAMPLES = 96000;
const MAX_WIDTH = 4096;
const MAX_HEIGHT = 2160;

const cache = {camera: [], microphone: []};
let enumerating = false;

function mediaDevices() {
  return globalThis.navigator?.mediaDevices;
}

function refreshDevices() {
  const md = mediaDevices();
  if (enumerating || typeof md?.enumerateDevices !== 'function') return;
  enumerating = true;
  md.enumerateDevices().then(list => {
    cache.camera = list.filter(d => d.kind === 'videoinput').map(d => ({id: d.deviceId, label: d.label}));
    cache.microphone = list.filter(d => d.kind === 'audioinput').map(d => ({id: d.deviceId, label: d.label}));
  }).catch(() => {}).finally(() => { enumerating = false; });
}

// Newline-separated `id\tlabel` rows from the last enumeration; starts a new one.
export function devices(kind) {
  refreshDevices();
  return (cache[kind] ?? []).map(d => `${d.id}\t${d.label.replace(/[\t\n]/g, ' ')}`).join('\n');
}

function errorKind(error) {
  switch (error?.name) {
    case 'NotAllowedError':
    case 'SecurityError':
      return 'permission';
    case 'NotFoundError':
    case 'OverconstrainedError':
      return 'notfound';
    case 'AbortError':
      return 'cancelled';
    default:
      return 'failed';
  }
}

class Capture {
  constructor() {
    this.status = 'starting';
    this.kind = '';
    this.message = '';
    this.stream = null;
  }
  fail(error) {
    this.status = 'failed';
    this.kind = errorKind(error);
    this.message = error?.message || String(error);
  }
  watchEnd(stream) {
    for (const track of stream.getTracks()) {
      track.addEventListener('ended', () => { if (this.status === 'live') this.status = 'ended'; });
    }
  }
  state() { return this.status; }
  errorKind() { return this.kind; }
  errorMessage() { return this.message; }
  stopStream() {
    for (const track of this.stream?.getTracks() ?? []) track.stop();
    this.stream = null;
  }
}

export class TiloCaptureVideo extends Capture {
  constructor(screen, deviceId, width, height, cursor) {
    super();
    this.video = null;
    this.canvas = null;
    this.context = null;
    this.width = 0;
    this.height = 0;
    this.dirty = false;
    const md = mediaDevices();
    const request = screen
      ? md?.getDisplayMedia?.({video: {cursor: cursor ? 'always' : 'never'}, audio: false})
      : md?.getUserMedia?.({
        video: {
          width: {ideal: width},
          height: {ideal: height},
          ...(deviceId ? {deviceId: {exact: deviceId}} : {}),
        },
        audio: false,
      });
    if (!request) {
      this.fail(Object.assign(new Error('this browser offers no media devices here (a secure context is required)'), {name: 'NotSupportedError'}));
      return;
    }
    request.then(stream => this.attach(stream)).catch(error => this.fail(error));
  }

  attach(stream) {
    if (this.status === 'closed') {
      for (const track of stream.getTracks()) track.stop();
      return;
    }
    this.stream = stream;
    this.watchEnd(stream);
    refreshDevices();
    const video = document.createElement('video');
    video.muted = true;
    video.playsInline = true;
    video.style.display = 'none';
    (document.body ?? document.documentElement).appendChild(video);
    video.srcObject = stream;
    this.video = video;
    const tick = () => {
      if (!this.video) return;
      this.dirty = true;
      if (typeof video.requestVideoFrameCallback === 'function') video.requestVideoFrameCallback(tick);
    };
    video.addEventListener('loadeddata', tick, {once: true});
    video.play().then(() => { if (this.status === 'starting') this.status = 'live'; }).catch(error => this.fail(error));
  }

  frame() {
    const video = this.video;
    if (!video || video.readyState < 2 || !video.videoWidth || !video.videoHeight) return undefined;
    if (!this.dirty && typeof video.requestVideoFrameCallback === 'function') return undefined;
    this.dirty = false;
    const scale = Math.min(1, MAX_WIDTH / video.videoWidth, MAX_HEIGHT / video.videoHeight);
    const width = Math.max(1, Math.round(video.videoWidth * scale));
    const height = Math.max(1, Math.round(video.videoHeight * scale));
    if (!this.canvas || this.width !== width || this.height !== height) {
      this.canvas = typeof OffscreenCanvas === 'function' ? new OffscreenCanvas(width, height) : Object.assign(document.createElement('canvas'), {width, height});
      this.context = this.canvas.getContext('2d', {willReadFrequently: true, alpha: false});
      this.width = width;
      this.height = height;
    }
    this.context.drawImage(video, 0, 0, width, height);
    const pixels = this.context.getImageData(0, 0, width, height).data;
    return new Uint8Array(pixels.buffer, pixels.byteOffset, pixels.byteLength);
  }
  frameWidth() { return this.width; }
  frameHeight() { return this.height; }

  close() {
    this.status = 'closed';
    this.stopStream();
    if (this.video) {
      this.video.srcObject = null;
      this.video.remove();
      this.video = null;
    }
    this.canvas = null;
    this.context = null;
  }
}

const WORKLET = `
class TiloCaptureTap extends AudioWorkletProcessor {
  process(inputs) {
    const channel = inputs[0]?.[0];
    if (channel) this.port.postMessage(channel.slice(0));
    return true;
  }
}
registerProcessor('tilo-capture-tap', TiloCaptureTap);
`;

export class TiloCaptureAudio extends Capture {
  constructor(deviceId, sampleRate) {
    super();
    this.blocks = [];
    this.buffered = 0;
    this.rate = 0;
    this.context = null;
    const md = mediaDevices();
    const request = md?.getUserMedia?.({
      audio: {
        channelCount: 1,
        ...(deviceId ? {deviceId: {exact: deviceId}} : {}),
      },
      video: false,
    });
    if (!request || typeof AudioContext !== 'function') {
      this.fail(Object.assign(new Error('this browser offers no microphone here (a secure context is required)'), {name: 'NotSupportedError'}));
      return;
    }
    request.then(stream => this.attach(stream, sampleRate)).catch(error => this.fail(error));
  }

  async attach(stream, sampleRate) {
    if (this.status === 'closed') {
      for (const track of stream.getTracks()) track.stop();
      return;
    }
    this.stream = stream;
    this.watchEnd(stream);
    refreshDevices();
    try {
      let context;
      try { context = new AudioContext({sampleRate}); } catch { context = new AudioContext(); }
      this.context = context;
      const url = URL.createObjectURL(new Blob([WORKLET], {type: 'text/javascript'}));
      try { await context.audioWorklet.addModule(url); } finally { URL.revokeObjectURL(url); }
      const source = context.createMediaStreamSource(stream);
      const tap = new AudioWorkletNode(context, 'tilo-capture-tap');
      tap.port.onmessage = event => this.push(event.data);
      source.connect(tap);
      await context.resume();
      this.rate = context.sampleRate;
      if (this.status === 'starting') this.status = 'live';
    } catch (error) {
      this.fail(error);
    }
  }

  push(block) {
    this.blocks.push(block);
    this.buffered += block.length;
    while (this.buffered > MAX_BUFFERED_SAMPLES && this.blocks.length > 1) {
      this.buffered -= this.blocks.shift().length;
    }
  }

  read() {
    const out = new Float32Array(this.buffered);
    let offset = 0;
    for (const block of this.blocks) {
      out.set(block, offset);
      offset += block.length;
    }
    this.blocks = [];
    this.buffered = 0;
    return out;
  }
  sampleRate() { return this.rate; }

  close() {
    this.status = 'closed';
    this.stopStream();
    this.context?.close?.().catch?.(() => {});
    this.context = null;
  }
}

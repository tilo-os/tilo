// Page backend for tilo-media's MediaEngine: one media element per engine.
//
// The element decodes (every codec the browser has, hardware where it has
// it) and plays the audio itself. Pictures reach Rust as RGBA through a 2D
// canvas, which is the same contract as the native engine's `VideoFrame`.
// A cross-origin source without CORS taints the canvas; audio still plays
// and the engine reports why there is no picture.

const MAX_FRAME_WIDTH = 1920;
const MAX_FRAME_HEIGHT = 1080;

let sessionOwner = null;

function newCanvas(width, height) {
  if (typeof OffscreenCanvas === 'function') return new OffscreenCanvas(width, height);
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  return canvas;
}

function fitWithin(width, height) {
  const scale = Math.min(1, MAX_FRAME_WIDTH / width, MAX_FRAME_HEIGHT / height);
  return [Math.max(1, Math.round(width * scale)), Math.max(1, Math.round(height * scale))];
}

export class TiloMediaElement {
  constructor() {
    const el = document.createElement('video');
    el.preload = 'auto';
    el.playsInline = true;
    el.crossOrigin = 'anonymous';
    el.style.display = 'none';
    (document.body ?? document.documentElement).appendChild(el);
    this.el = el;
    this.canvas = null;
    this.context = null;
    this.width = 0;
    this.height = 0;
    this.dirty = false;
    this.error = null;
    this.actions = [];
    this.metadata = null;
    this.onError = () => {
      const code = el.error?.code;
      const reasons = {1: 'playback aborted', 2: 'network error', 3: 'the browser could not decode the media', 4: 'the browser cannot play this format'};
      this.error = reasons[code] ?? (el.error?.message || 'media error');
    };
    this.onFrame = () => { this.dirty = true; };
    el.addEventListener('error', this.onError);
    el.addEventListener('seeked', this.onFrame);
    el.addEventListener('loadeddata', this.onFrame);
    el.addEventListener('play', () => this.claimSession());
    this.watchFrames();
  }

  watchFrames() {
    const el = this.el;
    if (typeof el.requestVideoFrameCallback !== 'function') return;
    const tick = () => {
      if (!this.el) return;
      this.dirty = true;
      el.requestVideoFrameCallback(tick);
    };
    el.requestVideoFrameCallback(tick);
  }

  open(url) {
    this.error = null;
    this.dirty = false;
    this.el.src = url;
    this.el.load();
  }

  play() {
    const started = this.el.play();
    started?.catch?.(error => {
      if (error?.name === 'NotAllowedError') this.error = 'the browser blocked playback until the page is interacted with';
      else if (error?.name !== 'AbortError') this.error = error?.message || String(error);
    });
  }

  pause() { this.el.pause(); }
  seek(ms) {
    this.el.currentTime = Math.max(0, ms / 1000);
    this.dirty = true;
  }
  setVolume(volume) { this.el.volume = Math.min(1, Math.max(0, volume)); }
  setMuted(muted) { this.el.muted = muted; }
  setRate(rate) { this.el.playbackRate = rate; }

  positionMs() { return Math.round((this.el.currentTime || 0) * 1000); }
  durationMs() {
    const d = this.el.duration;
    return Number.isFinite(d) ? Math.round(d * 1000) : -1;
  }
  isPlaying() { return !this.el.paused && !this.el.ended; }
  isEnded() { return this.el.ended; }
  bufferedPercent() {
    const el = this.el;
    const d = el.duration;
    if (!Number.isFinite(d) || d <= 0) return el.readyState >= 3 ? 100 : 0;
    let end = 0;
    for (let i = 0; i < el.buffered.length; i++) {
      if (el.buffered.start(i) <= el.currentTime + 0.5) end = Math.max(end, el.buffered.end(i));
    }
    return Math.min(100, Math.round((end / d) * 100));
  }
  takeError() {
    const error = this.error;
    this.error = null;
    return error ?? undefined;
  }

  // The newest picture as RGBA, or undefined when nothing changed since the
  // last call. `frameWidth`/`frameHeight` describe the returned pixels.
  frame() {
    const el = this.el;
    if (el.readyState < 2 || !el.videoWidth || !el.videoHeight) return undefined;
    if (!this.dirty && !(this.isPlaying() && typeof el.requestVideoFrameCallback !== 'function')) return undefined;
    this.dirty = false;
    const [width, height] = fitWithin(el.videoWidth, el.videoHeight);
    if (!this.canvas || this.width !== width || this.height !== height) {
      this.canvas = newCanvas(width, height);
      this.context = this.canvas.getContext('2d', {willReadFrequently: true, alpha: false});
      this.width = width;
      this.height = height;
    }
    try {
      this.context.drawImage(el, 0, 0, width, height);
      const pixels = this.context.getImageData(0, 0, width, height).data;
      return new Uint8Array(pixels.buffer, pixels.byteOffset, pixels.byteLength);
    } catch (error) {
      this.error = error?.name === 'SecurityError'
        ? 'this source does not allow its picture to be shown here (no CORS); audio only'
        : (error?.message || String(error));
      return undefined;
    }
  }
  frameWidth() { return this.width; }
  frameHeight() { return this.height; }

  setMetadata(title, artist, album, artwork) {
    this.metadata = {title, artist, album, artwork: artwork ? [{src: artwork}] : []};
    if (sessionOwner === this) this.publishMetadata();
  }

  claimSession() {
    const session = globalThis.navigator?.mediaSession;
    if (!session) return;
    sessionOwner = this;
    this.publishMetadata();
    const act = (name, handler) => {
      try { session.setActionHandler(name, details => { if (sessionOwner === this) handler(details ?? {}); }); } catch {}
    };
    act('play', () => { this.play(); this.actions.push('play'); });
    act('pause', () => { this.pause(); this.actions.push('pause'); });
    act('stop', () => { this.pause(); this.actions.push('stop'); });
    act('seekto', d => { if (Number.isFinite(d.seekTime)) this.seek(d.seekTime * 1000); });
    act('seekbackward', d => this.seek(Math.max(0, this.positionMs() - (d.seekOffset ?? 10) * 1000)));
    act('seekforward', d => this.seek(this.positionMs() + (d.seekOffset ?? 10) * 1000));
    act('previoustrack', () => this.actions.push('previous'));
    act('nexttrack', () => this.actions.push('next'));
  }

  publishMetadata() {
    const session = globalThis.navigator?.mediaSession;
    if (!session || !this.metadata || typeof MediaMetadata !== 'function') return;
    try { session.metadata = new MediaMetadata(this.metadata); } catch {}
  }

  // Keeps the system's now-playing position current; called from `poll`.
  publishState() {
    const session = globalThis.navigator?.mediaSession;
    if (!session || sessionOwner !== this) return;
    session.playbackState = this.isPlaying() ? 'playing' : 'paused';
    const d = this.el.duration;
    if (Number.isFinite(d) && d > 0 && typeof session.setPositionState === 'function') {
      try { session.setPositionState({duration: d, playbackRate: this.el.playbackRate || 1, position: Math.min(d, this.el.currentTime || 0)}); } catch {}
    }
  }

  takeActions() {
    const actions = this.actions;
    this.actions = [];
    return actions.join('\n');
  }

  close() {
    const el = this.el;
    if (!el) return;
    if (sessionOwner === this) {
      sessionOwner = null;
      try { globalThis.navigator.mediaSession.metadata = null; } catch {}
    }
    el.pause();
    el.removeAttribute('src');
    el.load();
    el.remove();
    this.el = null;
    this.canvas = null;
    this.context = null;
  }
}

export function objectUrl(bytes, mime) {
  return URL.createObjectURL(new Blob([bytes], {type: mime || 'application/octet-stream'}));
}

export function mediaElementsAvailable() {
  return typeof document === 'object' && typeof document.createElement === 'function';
}

// Real-time decode for tilo-media's RealtimeDecoder in a page: WebCodecs,
// hardware where the browser has it. One coded picture in at a time; the
// decoded pictures come out on WebCodecs' own schedule and are read back as
// RGBA through a 2D canvas, the same contract as the native decoder.
const WEB_CODECS = {h264: 'avc1.64002A', vp9: 'vp09.00.41.08', av1: 'av01.0.08M.08'};

export function webCodecsAvailable() {
  return typeof VideoDecoder === 'function' && typeof EncodedVideoChunk === 'function';
}

export class TiloVideoDecoder {
  constructor(codec) {
    const config = WEB_CODECS[codec];
    if (!config || !webCodecsAvailable()) throw new Error('this browser cannot decode pictures');
    this.latest = null;
    this.failed = false;
    this.inFlight = 0;
    this.timestamp = 0;
    this.canvas = null;
    this.context = null;
    this.takenWidth = 0;
    this.takenHeight = 0;
    this.decoder = new VideoDecoder({
      output: frame => this.picture(frame),
      error: () => { this.failed = true; },
    });
    this.decoder.configure({codec: config, optimizeForLatency: true});
  }

  picture(frame) {
    try {
      const [width, height] = fitWithin(frame.displayWidth, frame.displayHeight);
      if (!this.canvas || this.canvas.width !== width || this.canvas.height !== height) {
        this.canvas = newCanvas(width, height);
        this.context = this.canvas.getContext('2d', {willReadFrequently: true});
      }
      this.context.drawImage(frame, 0, 0, width, height);
      this.latest = {width, height, rgba: new Uint8Array(this.context.getImageData(0, 0, width, height).data.buffer)};
    } catch {
      this.failed = true;
    } finally {
      frame.close();
      this.inFlight = Math.max(0, this.inFlight - 1);
    }
  }

  decode(bytes, keyframe) {
    if (this.failed || this.decoder.state !== 'configured') return false;
    try {
      this.decoder.decode(new EncodedVideoChunk({type: keyframe ? 'key' : 'delta', timestamp: this.timestamp, data: bytes}));
    } catch {
      this.failed = true;
      return false;
    }
    this.timestamp += 33333;
    this.inFlight += 1;
    return true;
  }

  pending() { return this.inFlight > 0; }

  hasFailed() { return this.failed; }

  take() {
    const latest = this.latest;
    this.latest = null;
    if (!latest) return undefined;
    this.takenWidth = latest.width;
    this.takenHeight = latest.height;
    return latest.rgba;
  }

  takenWidthPx() { return this.takenWidth; }

  takenHeightPx() { return this.takenHeight; }

  close() {
    try { this.decoder.close(); } catch {}
  }
}

// Page backend for tilo-media's MediaEngine: one media element per engine.
//
// The element decodes (every codec the browser has, hardware where it has
// it) and plays the audio itself. Pictures reach Rust as RGBA through a 2D
// canvas, which is the same contract as the native engine's `VideoFrame`,
// or stay in the element for a renderer that copies them on the GPU
// (`takePicture`): reading every picture back costs the page's one thread
// more than a frame's time.
// A cross-origin source without CORS cannot load with the element's CORS
// request: it is loaded again through the fetch proxy of the host serving the
// page, when the page names one, else the engine reports why it failed. An
// element never shows why a proxied load failed, so the engine asks the proxy:
// a host that relayed all it allows for now says so, and the engine reports
// that (`takeLimitReached`) instead of loading the source again.

const MAX_FRAME_WIDTH = 1920;
const MAX_FRAME_HEIGHT = 1080;
// The largest picture side a renderer copies from the element on the GPU:
// a 4K picture fits, with room to spare, in the renderer's image atlas.
const MAX_PICTURE_SIDE = 4096;

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

/// How many times a source is loaded again through the fetch proxy: once
/// when a cross-origin source refuses the page, and after that when a
/// session the proxied URL carried has ended (the page reconnected).
const PROXY_ATTEMPTS = 3;

// `url` as a media element loads it through the host's fetch proxy, or null
// when the page names none or `url` needs none (same origin, not http).
// The page names the proxy in `globalThis.__tiloFetchProxy` (an endpoint, or
// a function answering `{endpoint, session}`), as for tilo-net's requests;
// an element cannot send headers, so the marker and session ride in the query
// (`tilo_net::proxy::media_url`).
export function proxiedMediaUrl(url, scope = globalThis) {
  const named = scope.__tiloFetchProxy;
  let proxy = typeof named === 'function' ? named() : named;
  if (typeof proxy === 'string') proxy = {endpoint: proxy};
  if (!proxy?.endpoint) return null;
  const page = scope.location?.href;
  let target;
  try {
    target = new URL(url, page);
  } catch {
    return null;
  }
  if (!['http:', 'https:'].includes(target.protocol)) return null;
  if (page && target.origin === new URL(page).origin) return null;
  const endpoint = new URL(proxy.endpoint, page);
  endpoint.searchParams.set('url', target.href);
  endpoint.searchParams.set('tilo-proxy', '1');
  if (proxy.session) endpoint.searchParams.set('tilo-session', proxy.session);
  return endpoint.href;
}

// Sites whose media refuses other pages (no CORS answer), so an element loads
// them through the host's fetch proxy from the start instead of making a
// request that always fails first. Seeded with the CDNs known to answer no
// page; a site that refused this page and then played through the proxy
// joins them for the rest of the page's life.
const SITES_WITHOUT_CORS = new Set(['googlevideo.com']);

// The site `url` belongs to: its host's last two labels (a CDN's hosts
// differ per file, its site does not), or null for a URL without a host.
export function siteOf(url, scope = globalThis) {
  let host;
  try {
    host = new URL(url, scope.location?.href).hostname;
  } catch {
    return null;
  }
  if (!host) return null;
  if (/^[\d.]+$/.test(host) || host.includes(':')) return host;
  return host.split('.').slice(-2).join('.');
}

// Whether `url`'s site is known to refuse the page.
export function lacksCors(url, scope = globalThis) {
  const site = siteOf(url, scope);
  return site !== null && SITES_WITHOUT_CORS.has(site);
}

/// The code the host's fetch proxy refuses with once it relayed all it allows
/// this visitor or room for now (`tilo_net::proxy::ProxyFailure::OverBudget`).
export const OVER_BUDGET = 'over-budget';

const ELEMENT_ERRORS = {1: 'playback aborted', 2: 'network error', 3: 'the browser could not decode the media', 4: 'the browser cannot play this format'};

// Why the host's fetch proxy refuses `proxied`, a URL from `proxiedMediaUrl`:
// its `x-tilo-proxy-error` code, or null when it answers the request or
// cannot be asked. A `HEAD`, which a refusing proxy answers without reaching
// the source; the host's CORS answer exposes the header.
export async function proxyRefusal(proxied, scope = globalThis) {
  if (typeof scope.fetch !== 'function') return null;
  try {
    const response = await scope.fetch(proxied, {method: 'HEAD', cache: 'no-store', credentials: 'omit'});
    return response.headers.get('x-tilo-proxy-error');
  } catch {
    return null;
  }
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
    this.source = null;
    this.proxied = null;
    this.proxyAttempts = 0;
    this.limitReached = false;
    this.wantsPlay = false;
    this.refusedDirectly = false;
    this.onError = () => {
      const failed = el.error;
      if (this.proxied) {
        this.explainProxied(this.proxied, failed);
        return;
      }
      this.refusedDirectly = true;
      if (this.loadThroughProxy()) return;
      this.fail(failed);
    };
    this.onFrame = () => { this.dirty = true; };
    // A source that refused the page and played through the proxy: its site
    // goes through the proxy from the start from now on.
    el.addEventListener('loadedmetadata', () => {
      if (!this.proxied || !this.refusedDirectly) return;
      const site = siteOf(this.source);
      if (site) SITES_WITHOUT_CORS.add(site);
    });
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

  fail(failed) {
    this.error = ELEMENT_ERRORS[failed?.code] ?? (failed?.message || 'media error');
  }

  // A load through the proxy failed: report a spent budget as such, else
  // load it again (a session that ended) or report the element's error.
  explainProxied(proxied, failed) {
    return proxyRefusal(proxied).then(refusal => {
      if (!this.el || this.proxied !== proxied) return;
      if (refusal === OVER_BUDGET) {
        this.limitReached = true;
        this.error = 'the host streamed as much as it allows for now';
        return;
      }
      if (this.loadThroughProxy()) return;
      this.fail(failed);
    });
  }

  open(url) {
    this.error = null;
    this.limitReached = false;
    this.dirty = false;
    this.source = url;
    this.proxied = null;
    this.proxyAttempts = 0;
    this.refusedDirectly = false;
    if (lacksCors(url) && this.loadThroughProxy()) return;
    this.el.src = url;
    this.el.load();
  }

  // Load the source again through the host's fetch proxy, where it plays
  // on: true when it did.
  loadThroughProxy() {
    if (!this.source || this.proxyAttempts >= PROXY_ATTEMPTS) return false;
    const proxied = proxiedMediaUrl(this.source);
    if (!proxied) return false;
    this.proxyAttempts += 1;
    this.proxied = proxied;
    const at = this.el.currentTime || 0;
    this.el.src = proxied;
    this.el.load();
    if (at > 0) this.el.currentTime = at;
    if (this.wantsPlay) this.play();
    return true;
  }

  play() {
    this.wantsPlay = true;
    const started = this.el.play();
    started?.catch?.(error => {
      if (error?.name === 'NotAllowedError') this.error = 'the browser blocked playback until the page is interacted with';
      else if (error?.name !== 'AbortError') this.error = error?.message || String(error);
    });
  }

  pause() {
    this.wantsPlay = false;
    this.el.pause();
  }
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
  // Whether the host's proxy stopped this source because it relayed all it
  // allows for now; read before `takeError`, which says so in words.
  takeLimitReached() {
    const reached = this.limitReached;
    this.limitReached = false;
    return reached;
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

  // Whether the element shows a picture newer than the last one taken, for
  // a renderer that copies it from the element on the GPU (`pictureElement`)
  // rather than reading its pixels back here, as `frame` does.
  takePicture() {
    const el = this.el;
    if (el.readyState < 2 || !el.videoWidth || !el.videoHeight) return false;
    if (!this.dirty && !(this.isPlaying() && typeof el.requestVideoFrameCallback !== 'function')) return false;
    this.dirty = false;
    return true;
  }
  // Whether the element's pictures are small enough to copy as they are; a
  // larger one is scaled down and read back by `frame` instead.
  picturesFit() {
    return this.el.videoWidth <= MAX_PICTURE_SIDE && this.el.videoHeight <= MAX_PICTURE_SIDE;
  }
  pictureWidth() { return this.el.videoWidth; }
  pictureHeight() { return this.el.videoHeight; }
  pictureElement() { return this.el; }

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
    this.source = null;
    this.proxied = null;
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

// Page backend for editing video: a picture at an exact frame, and an
// encoder that writes a finished file.
//
// Both use the browser's own media stack, so nothing patent-encumbered ships
// with the page: the media element decodes whatever the browser plays, and
// WebCodecs encodes with whichever royalty-free codec the browser has.
//
// `TiloFrameSeeker` answers "the picture of frame N" without blocking: the
// caller asks, keeps drawing, and reads the element once `shownFrame()` says
// it holds that frame. A seek lands on the middle of the frame's interval,
// never its edge, so rounding in the element's clock cannot pick the frame
// before it.
//
// `TiloClipEncoder` takes RGBA frames, encodes them with `VideoEncoder` and
// writes them into a WebM (Matroska for H.264) file in memory.

// How far the element's clock may run from the playhead while following it
// before a seek pulls it back.
const FOLLOW_DRIFT_S = 0.25;

/// The frame of a source running at `fps` that is showing at `seconds`.
export function frameAt(seconds, fps) {
  return Math.max(0, Math.floor(seconds * fps + 1e-6));
}

/// Where to seek for frame `frame`: the middle of its interval.
export function frameCentre(frame, fps) {
  return (frame + 0.5) / fps;
}

export class TiloFrameSeeker {
  constructor() {
    const el = document.createElement('video');
    el.preload = 'auto';
    el.playsInline = true;
    el.muted = true;
    el.style.display = 'none';
    (document.body ?? document.documentElement).appendChild(el);
    this.el = el;
    this.fps = 30;
    this.target = -1;
    this.following = false;
    this.error = null;
    el.addEventListener('error', () => {
      const code = el.error?.code;
      this.error = code === 4 ? 'the browser cannot play this format'
        : code === 3 ? 'the browser could not decode the media'
          : 'the media could not be loaded';
    });
  }

  open(url) {
    this.error = null;
    this.target = -1;
    this.following = false;
    this.el.src = url;
    this.el.load();
  }

  /// Hold the picture of the frame showing at `seconds`, exactly.
  seek(seconds, fps) {
    this.fps = fps > 0 ? fps : 30;
    const frame = frameAt(seconds, this.fps);
    if (this.following) {
      this.following = false;
      this.el.pause();
    } else if (frame === this.target) {
      return;
    }
    this.target = frame;
    let at = frameCentre(frame, this.fps);
    const duration = this.el.duration;
    if (Number.isFinite(duration) && duration > 0) at = Math.min(at, Math.max(0, duration - 0.5 / this.fps));
    this.el.currentTime = at;
  }

  /// Run with a playhead moving at normal speed from `seconds`: the element
  /// plays, and is pulled back when it drifts. Pictures are the ones the
  /// element shows as it plays, which keeps playback smooth; a held frame
  /// is exact again once `seek` is asked.
  follow(seconds, fps) {
    this.fps = fps > 0 ? fps : 30;
    const el = this.el;
    if (!this.following || Math.abs(el.currentTime - seconds) > FOLLOW_DRIFT_S) {
      el.currentTime = seconds;
    }
    if (!this.following || el.paused) {
      this.following = true;
      el.play()?.catch?.(() => {});
    }
  }

  /// The frame the element shows now, or -1 while it shows none or is
  /// still on its way to the one asked for.
  shownFrame() {
    const el = this.el;
    if (this.error || el.readyState < 2 || el.seeking || !el.videoWidth) return -1;
    if (this.following) return frameAt(el.currentTime, this.fps);
    return this.target;
  }

  width() { return this.el.videoWidth; }
  height() { return this.el.videoHeight; }
  element() { return this.el; }

  /// The picture the element shows, as RGBA no larger than
  /// `maxWidth`x`maxHeight` (its aspect kept); `pictureWidth` and
  /// `pictureHeight` give its size. Empty while there is none.
  picture(maxWidth, maxHeight) {
    const el = this.el;
    if (this.shownFrame() < 0) return new Uint8Array();
    const scale = Math.min(1, maxWidth / el.videoWidth, maxHeight / el.videoHeight);
    const width = Math.max(1, Math.round(el.videoWidth * scale));
    const height = Math.max(1, Math.round(el.videoHeight * scale));
    if (!this.canvas || this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas = typeof OffscreenCanvas === 'function' ? new OffscreenCanvas(width, height) : Object.assign(document.createElement('canvas'), {width, height});
      this.context = this.canvas.getContext('2d', {willReadFrequently: true});
    }
    this.context.drawImage(el, 0, 0, width, height);
    this.pictureSize = [width, height];
    return new Uint8Array(this.context.getImageData(0, 0, width, height).data.buffer);
  }
  pictureWidth() { return this.pictureSize?.[0] ?? 0; }
  pictureHeight() { return this.pictureSize?.[1] ?? 0; }
  takeError() { const e = this.error; this.error = null; return e; }
  hasError() { return this.error !== null; }

  close() {
    this.el.pause();
    this.el.removeAttribute('src');
    this.el.load();
    this.el.remove();
  }
}

// Codecs tried in order. VP9 first: every browser that encodes it also plays
// it back from WebM, and it is royalty-free. AV1 compresses better but its
// browser encoders are slow enough to make a short export a long wait. VP8
// is the oldest WebM codec. H.264 is used only when the browser encodes none
// of them, and it is the browser's own encoder: none ships with the page.
export const CODECS = [
  {codec: 'vp09.00.40.08', id: 'V_VP9', doc: 'webm', ext: 'webm', mime: 'video/webm'},
  {codec: 'av01.0.08M.08', id: 'V_AV1', doc: 'webm', ext: 'webm', mime: 'video/webm'},
  {codec: 'vp8', id: 'V_VP8', doc: 'webm', ext: 'webm', mime: 'video/webm'},
  {codec: 'avc1.640028', id: 'V_MPEG4/ISO/AVC', doc: 'matroska', ext: 'mkv', mime: 'video/x-matroska', avc: {format: 'avc'}},
];

/// The first codec in `CODECS` this browser encodes at `width`x`height`,
/// with the config to encode it, or null.
export async function pickCodec(width, height, fps, bitrate, scope = globalThis) {
  const Encoder = scope.VideoEncoder;
  if (typeof Encoder !== 'function') return null;
  for (const choice of CODECS) {
    const config = {codec: choice.codec, width, height, bitrate, framerate: fps, latencyMode: 'quality'};
    if (choice.avc) config.avc = choice.avc;
    try {
      const {supported} = await Encoder.isConfigSupported(config);
      if (supported) return {choice, config};
    } catch {
      // A codec string this browser does not parse: try the next.
    }
  }
  return null;
}

/// Whether this page can encode video at all.
export function videoEncoderAvailable(scope = globalThis) {
  return typeof scope.VideoEncoder === 'function' && typeof scope.VideoFrame === 'function';
}

export class TiloClipEncoder {
  /// Starts at once; `state()` says when frames are taken.
  constructor(width, height, fps, bitrate) {
    this.width = width;
    this.height = height;
    this.fps = fps;
    this.stateName = 'opening';
    this.reason = null;
    this.chunks = [];
    this.description = null;
    this.choice = null;
    this.bytes = null;
    this.encoder = null;
    pickCodec(width, height, fps, bitrate).then(picked => {
      if (this.stateName !== 'opening') return;
      if (!picked) {
        this.fail('this browser cannot encode video');
        return;
      }
      this.choice = picked.choice;
      this.encoder = new VideoEncoder({
        output: (chunk, metadata) => {
          const data = new Uint8Array(chunk.byteLength);
          chunk.copyTo(data);
          this.chunks.push({data, timestamp: chunk.timestamp, key: chunk.type === 'key'});
          const description = metadata?.decoderConfig?.description;
          if (description && !this.description) {
            this.description = new Uint8Array(ArrayBuffer.isView(description) ? description.buffer.slice(description.byteOffset, description.byteOffset + description.byteLength) : description);
          }
        },
        error: error => this.fail(`the encoder stopped: ${error.message ?? error}`),
      });
      this.encoder.configure(picked.config);
      this.stateName = 'ready';
    }, error => this.fail(`the encoder could not start: ${error.message ?? error}`));
  }

  fail(reason) {
    if (this.stateName === 'failed') return;
    this.stateName = 'failed';
    this.reason = reason;
    try { this.encoder?.close(); } catch { /* already closed */ }
  }

  /// 'opening', 'ready', 'finishing', 'done' or 'failed'.
  state() { return this.stateName; }
  takeReason() { return this.reason ?? ''; }
  /// The codec it encodes with, once ready.
  codec() { return this.choice?.codec ?? ''; }
  extension() { return this.choice?.ext ?? 'webm'; }
  /// Frames handed over and not yet encoded.
  backlog() { return this.encoder?.encodeQueueSize ?? 0; }

  /// Encode frame `index` from tightly packed RGBA.
  encode(rgba, index, key) {
    if (this.stateName !== 'ready') return false;
    const timestamp = Math.round(index * 1e6 / this.fps);
    const frame = new VideoFrame(rgba, {
      format: 'RGBA',
      codedWidth: this.width,
      codedHeight: this.height,
      timestamp,
      duration: Math.round(1e6 / this.fps),
    });
    try {
      this.encoder.encode(frame, {keyFrame: key});
    } catch (error) {
      this.fail(`the encoder refused a frame: ${error.message ?? error}`);
    } finally {
      frame.close();
    }
    return this.stateName === 'ready';
  }

  /// Encode what is queued and write the file; `takeBytes` has it once
  /// `state()` is 'done'.
  finish() {
    if (this.stateName !== 'ready') return;
    this.stateName = 'finishing';
    this.encoder.flush().then(() => {
      if (this.stateName !== 'finishing') return;
      this.encoder.close();
      this.bytes = muxMatroska({
        doc: this.choice.doc,
        codecId: this.choice.id,
        codecPrivate: this.description,
        width: this.width,
        height: this.height,
        fps: this.fps,
        chunks: this.chunks,
      });
      this.chunks = [];
      this.stateName = 'done';
    }, error => this.fail(`the encoder could not finish: ${error.message ?? error}`));
  }

  takeBytes() {
    const bytes = this.bytes ?? new Uint8Array();
    this.bytes = null;
    return bytes;
  }

  close() {
    if (this.stateName === 'opening' || this.stateName === 'ready' || this.stateName === 'finishing') {
      this.fail('cancelled');
    }
    this.chunks = [];
    this.bytes = null;
  }
}

// --- Matroska / WebM writing -------------------------------------------------

const utf8 = new TextEncoder();

function concat(parts) {
  let length = 0;
  for (const part of parts) length += part.length;
  const out = new Uint8Array(length);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

function idBytes(id) {
  const bytes = [];
  for (let v = id; v > 0; v = Math.floor(v / 256)) bytes.unshift(v % 256);
  return Uint8Array.from(bytes);
}

/// An EBML element size: the shortest variable-length integer that holds it.
export function vint(size) {
  for (let length = 1; length <= 8; length++) {
    // All ones is reserved for "unknown size".
    if (size < 2 ** (7 * length) - 1) {
      const out = new Uint8Array(length);
      let v = size;
      for (let i = length - 1; i >= 0; i--) {
        out[i] = v % 256;
        v = Math.floor(v / 256);
      }
      out[0] |= 1 << (8 - length);
      return out;
    }
  }
  throw new Error(`element too large: ${size}`);
}

function element(id, payload) {
  return concat([idBytes(id), vint(payload.length), payload]);
}

function uintBytes(value, width = 0) {
  const bytes = [];
  for (let v = value; v > 0; v = Math.floor(v / 256)) bytes.unshift(v % 256);
  while (bytes.length < Math.max(width, 1)) bytes.unshift(0);
  return Uint8Array.from(bytes);
}

const uint = (id, value, width) => element(id, uintBytes(value, width));
const str = (id, value) => element(id, utf8.encode(value));
function float(id, value) {
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setFloat64(0, value);
  return element(id, bytes);
}
const master = (id, children) => element(id, concat(children));

// A cluster's blocks are timed relative to it in a signed 16-bit count of
// milliseconds; a new cluster starts well before that runs out.
const CLUSTER_SPAN_MS = 5000;

/// A whole WebM/Matroska file holding one video track of `chunks`
/// (`{data, timestamp (µs), key}`, in decode order).
export function muxMatroska({doc, codecId, codecPrivate, width, height, fps, chunks}) {
  const frameNs = Math.round(1e9 / fps);
  const lastMs = chunks.length ? Math.round(chunks[chunks.length - 1].timestamp / 1000) : 0;
  const durationMs = lastMs + 1000 / fps;
  const header = master(0x1A45DFA3, [
    uint(0x4286, 1), uint(0x42F7, 1), uint(0x42F2, 4), uint(0x42F3, 8),
    str(0x4282, doc), uint(0x4287, 4), uint(0x4285, 2),
  ]);
  const info = master(0x1549A966, [
    uint(0x2AD7B1, 1000000),
    str(0x4D80, 'tilo'),
    str(0x5741, 'tilo'),
    float(0x4489, durationMs),
  ]);
  const entry = [
    uint(0xD7, 1), uint(0x73C5, 1), uint(0x83, 1), str(0x86, codecId),
    uint(0x23E383, frameNs),
  ];
  if (codecPrivate && codecPrivate.length) entry.push(element(0x63A2, codecPrivate));
  entry.push(master(0xE0, [uint(0xB0, width), uint(0xBA, height)]));
  const tracks = master(0x1654AE6B, [master(0xAE, entry)]);

  // Clusters, each starting on a key frame once the last has run long
  // enough, so a player can seek to any cluster.
  const clusters = [];
  let current = null;
  for (const chunk of chunks) {
    const ms = Math.round(chunk.timestamp / 1000);
    const startNew = !current || (chunk.key && ms - current.ms >= 1000) || ms - current.ms >= CLUSTER_SPAN_MS;
    if (startNew) {
      current = {ms, key: chunk.key, blocks: []};
      clusters.push(current);
    }
    const head = new Uint8Array(4);
    head[0] = 0x81; // track 1
    new DataView(head.buffer).setInt16(1, ms - current.ms);
    head[3] = chunk.key ? 0x80 : 0;
    current.blocks.push(element(0xA3, concat([head, chunk.data])));
  }
  const clusterBytes = clusters.map(c => master(0x1F43B675, [uint(0xE7, c.ms), ...c.blocks]));

  // Cues before the clusters, so a player finds them reading forward. Each
  // position is written at a fixed width, which makes the cues' own size
  // known before the positions they hold are.
  const cuePoint = (ms, position) => master(0xBB, [
    uint(0xB3, ms),
    master(0xB7, [uint(0xF7, 1), uint(0xF1, position, 8)]),
  ]);
  const keyed = clusters.map((c, i) => [c, i]).filter(([c]) => c.key);
  const cuesSize = master(0x1C53BB6B, keyed.map(([c]) => cuePoint(c.ms, 0))).length;
  let position = info.length + tracks.length + cuesSize;
  const positions = clusterBytes.map(bytes => {
    const at = position;
    position += bytes.length;
    return at;
  });
  const cues = master(0x1C53BB6B, keyed.map(([c, i]) => cuePoint(c.ms, positions[i])));
  const segment = master(0x18538067, [info, tracks, cues, ...clusterBytes]);
  return concat([header, segment]);
}

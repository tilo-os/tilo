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
// `TiloClipEncoder` takes RGBA frames (and mixed sound), encodes them with
// `VideoEncoder` (and Opus through `AudioEncoder`) and writes them into a
// WebM (Matroska for H.264) file in memory.
//
// `TiloPcmDecoder` turns a source's sound into samples with the page's own
// audio decoding, and `TiloPcmPlayer` plays mixed samples at the times they
// belong to.

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
    this.rate = 1;
    this.error = null;
    // A still picture's decoded bitmap, when the source is one.
    this.still = null;
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

  /// Show a still picture instead: it has one frame, 0, held once decoded.
  openImage(url) {
    this.error = null;
    this.still = {bitmap: null};
    const still = this.still;
    fetch(url).then(response => response.blob()).then(blob => createImageBitmap(blob)).then(bitmap => {
      if (this.still === still) still.bitmap = bitmap;
      else bitmap.close?.();
    }, () => {
      if (this.still === still) this.error = 'the browser cannot decode this picture';
    });
  }

  /// Hold the picture of the frame showing at `seconds`, exactly.
  seek(seconds, fps) {
    this.fps = fps > 0 ? fps : 30;
    if (this.still) return;
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

  /// Run with a playhead moving forward at `rate` times normal speed from
  /// `seconds`: the element plays at that rate, and is pulled back when it
  /// drifts. Pictures are the ones the element shows as it plays, which
  /// keeps playback smooth; a held frame is exact again once `seek` is
  /// asked.
  follow(seconds, fps, rate = 1) {
    this.fps = fps > 0 ? fps : 30;
    if (this.still) return;
    const el = this.el;
    const wanted = playbackRate(rate);
    if (wanted !== this.rate) {
      this.rate = wanted;
      el.playbackRate = wanted;
    }
    if (!this.following || Math.abs(el.currentTime - seconds) > FOLLOW_DRIFT_S * Math.max(1, wanted)) {
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
    if (this.still) return this.still.bitmap && !this.error ? 0 : -1;
    const el = this.el;
    if (this.error || el.readyState < 2 || el.seeking || !el.videoWidth) return -1;
    if (this.following) return frameAt(el.currentTime, this.fps);
    return this.target;
  }

  width() { return this.still ? this.still.bitmap?.width ?? 0 : this.el.videoWidth; }
  height() { return this.still ? this.still.bitmap?.height ?? 0 : this.el.videoHeight; }
  element() { return this.el; }

  /// The picture the element shows, as RGBA no larger than
  /// `maxWidth`x`maxHeight` (its aspect kept); `pictureWidth` and
  /// `pictureHeight` give its size. Empty while there is none.
  picture(maxWidth, maxHeight) {
    if (this.shownFrame() < 0) return new Uint8Array();
    const el = this.still?.bitmap ?? this.el;
    const [w, h] = [this.width(), this.height()];
    const scale = Math.min(1, maxWidth / w, maxHeight / h);
    const width = Math.max(1, Math.round(w * scale));
    const height = Math.max(1, Math.round(h * scale));
    if (!this.canvas || this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas = typeof OffscreenCanvas === 'function' ? new OffscreenCanvas(width, height) : Object.assign(document.createElement('canvas'), {width, height});
      this.context = this.canvas.getContext('2d', {willReadFrequently: true});
    }
    // A picture with transparency must not keep the last one's pixels.
    this.context.clearRect(0, 0, width, height);
    this.context.drawImage(el, 0, 0, width, height);
    this.pictureSize = [width, height];
    return new Uint8Array(this.context.getImageData(0, 0, width, height).data.buffer);
  }
  pictureWidth() { return this.pictureSize?.[0] ?? 0; }
  pictureHeight() { return this.pictureSize?.[1] ?? 0; }
  takeError() { const e = this.error; this.error = null; return e; }
  hasError() { return this.error !== null; }

  close() {
    this.still?.bitmap?.close?.();
    this.still = null;
    this.el.pause();
    this.el.removeAttribute('src');
    this.el.load();
    this.el.remove();
  }
}

/// The playback rate an element is set to for a playhead running at
/// `rate`: within the range browsers play, and snapped to a power of two
/// when near one, so a rate measured from a jittery clock does not retune
/// the element every frame.
export function playbackRate(rate) {
  const clamped = Math.min(16, Math.max(0.0625, Number.isFinite(rate) && rate > 0 ? rate : 1));
  const power = 2 ** Math.round(Math.log2(clamped));
  if (Math.abs(clamped / power - 1) < 0.12) return power;
  return Math.round(clamped * 20) / 20;
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

// Sound is mixed, encoded and played at one rate, in stereo, interleaved.
export const SAMPLE_RATE = 48000;
const CHANNELS = 2;

// Opus is the WebM sound codec: royalty-free, and the one `AudioEncoder`
// encodes in every browser that has it.
export const OPUS_CONFIG = {codec: 'opus', sampleRate: SAMPLE_RATE, numberOfChannels: CHANNELS, bitrate: 160000};

/// Whether this page encodes Opus sound.
export async function opusAvailable(scope = globalThis) {
  if (typeof scope.AudioEncoder !== 'function' || typeof scope.AudioData !== 'function') return false;
  try {
    return (await scope.AudioEncoder.isConfigSupported(OPUS_CONFIG)).supported === true;
  } catch {
    return false;
  }
}

function copyChunk(chunk) {
  const data = new Uint8Array(chunk.byteLength);
  chunk.copyTo(data);
  return data;
}

function descriptionBytes(description) {
  return new Uint8Array(ArrayBuffer.isView(description)
    ? description.buffer.slice(description.byteOffset, description.byteOffset + description.byteLength)
    : description);
}

export class TiloClipEncoder {
  /// Starts at once; `state()` says when frames are taken. With `sound`, the
  /// file also carries the sound handed to `encodeSound`, when the browser
  /// encodes Opus (`hasSound()` says, once ready).
  constructor(width, height, fps, bitrate, sound = false) {
    this.width = width;
    this.height = height;
    this.fps = fps;
    this.stateName = 'opening';
    this.reason = null;
    this.chunks = [];
    this.description = null;
    this.soundDescription = null;
    this.choice = null;
    this.bytes = null;
    this.encoder = null;
    this.sound = null;
    Promise.all([pickCodec(width, height, fps, bitrate), sound ? opusAvailable() : false]).then(([picked, opus]) => {
      if (this.stateName !== 'opening') return;
      if (!picked) {
        this.fail('this browser cannot encode video');
        return;
      }
      this.choice = picked.choice;
      this.encoder = new VideoEncoder({
        output: (chunk, metadata) => {
          this.chunks.push({track: 1, data: copyChunk(chunk), timestamp: chunk.timestamp, duration: 1e6 / this.fps, key: chunk.type === 'key'});
          const description = metadata?.decoderConfig?.description;
          if (description && !this.description) this.description = descriptionBytes(description);
        },
        error: error => this.fail(`the encoder stopped: ${error.message ?? error}`),
      });
      this.encoder.configure(picked.config);
      if (opus) {
        this.sound = new AudioEncoder({
          output: (chunk, metadata) => {
            this.chunks.push({track: 2, data: copyChunk(chunk), timestamp: chunk.timestamp, duration: chunk.duration ?? 0, key: true});
            const description = metadata?.decoderConfig?.description;
            if (description && !this.soundDescription) this.soundDescription = descriptionBytes(description);
          },
          error: error => this.fail(`the sound encoder stopped: ${error.message ?? error}`),
        });
        this.sound.configure(OPUS_CONFIG);
      }
      this.stateName = 'ready';
    }, error => this.fail(`the encoder could not start: ${error.message ?? error}`));
  }

  fail(reason) {
    if (this.stateName === 'failed') return;
    this.stateName = 'failed';
    this.reason = reason;
    for (const encoder of [this.encoder, this.sound]) {
      try { encoder?.close(); } catch { /* already closed */ }
    }
  }

  /// 'opening', 'ready', 'finishing', 'done' or 'failed'.
  state() { return this.stateName; }
  takeReason() { return this.reason ?? ''; }
  /// The codec it encodes with, once ready.
  codec() { return this.choice?.codec ?? ''; }
  extension() { return this.choice?.ext ?? 'webm'; }
  /// Whether the file carries sound, once ready.
  hasSound() { return this.sound !== null; }
  /// Frames (and blocks of sound) handed over and not yet encoded.
  backlog() { return (this.encoder?.encodeQueueSize ?? 0) + (this.sound?.encodeQueueSize ?? 0); }

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

  /// Encode interleaved stereo `samples` at `SAMPLE_RATE` starting `start`
  /// samples into the file. Nothing happens when the file carries no sound.
  encodeSound(samples, start) {
    if (this.stateName !== 'ready') return false;
    const frames = Math.floor(samples.length / CHANNELS);
    if (!this.sound || frames === 0) return true;
    const data = new AudioData({
      format: 'f32',
      sampleRate: SAMPLE_RATE,
      numberOfFrames: frames,
      numberOfChannels: CHANNELS,
      timestamp: Math.round(start * 1e6 / SAMPLE_RATE),
      data: samples.length === frames * CHANNELS ? samples : samples.subarray(0, frames * CHANNELS),
    });
    try {
      this.sound.encode(data);
    } catch (error) {
      this.fail(`the sound encoder refused a block: ${error.message ?? error}`);
    } finally {
      data.close();
    }
    return this.stateName === 'ready';
  }

  /// Encode what is queued and write the file; `takeBytes` has it once
  /// `state()` is 'done'.
  finish() {
    if (this.stateName !== 'ready') return;
    this.stateName = 'finishing';
    Promise.all([this.encoder.flush(), this.sound?.flush()]).then(() => {
      if (this.stateName !== 'finishing') return;
      this.encoder.close();
      this.sound?.close();
      this.bytes = muxMatroska({
        doc: this.choice.doc,
        video: {codecId: this.choice.id, codecPrivate: this.description, width: this.width, height: this.height, fps: this.fps},
        sound: this.sound ? opusTrack(this.soundDescription) : null,
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

// The samples an Opus encoder runs ahead by at 48 kHz (libopus's look-ahead),
// for a stream whose encoder did not say.
const OPUS_PRE_SKIP = 312;

/// An Opus identification header, as Matroska carries it in the track.
export function opusHead(channels, rate, preSkip = OPUS_PRE_SKIP) {
  const head = new Uint8Array(19);
  head.set(utf8.encode('OpusHead'));
  const view = new DataView(head.buffer);
  head[8] = 1;
  head[9] = channels;
  view.setUint16(10, preSkip, true);
  view.setUint32(12, rate, true);
  return head;
}

/// The Matroska track of an Opus stream whose encoder described it with
/// `description` (an `OpusHead`), or did not.
export function opusTrack(description) {
  const isHead = description?.length >= 19 && String.fromCharCode(...description.subarray(0, 8)) === 'OpusHead';
  const head = isHead ? description : opusHead(CHANNELS, SAMPLE_RATE);
  const preSkip = head[10] | (head[11] << 8);
  return {
    codecId: 'A_OPUS',
    codecPrivate: head,
    rate: SAMPLE_RATE,
    channels: head[9],
    codecDelayNs: Math.round(preSkip * 1e9 / 48000),
    // Opus needs 80 ms decoded before a seek point to sound right.
    seekPreRollNs: 80000000,
  };
}

// --- Sound in and out ---------------------------------------------------------

/// `buffer` (an `AudioBuffer`) as interleaved stereo: a mono source in both
/// channels, a source with more than two channels by its first two.
export function interleaveStereo(buffer) {
  const left = buffer.getChannelData(0);
  const right = buffer.numberOfChannels > 1 ? buffer.getChannelData(1) : left;
  const out = new Float32Array(buffer.length * CHANNELS);
  for (let i = 0; i < buffer.length; i++) {
    out[i * 2] = left[i];
    out[i * 2 + 1] = right[i];
  }
  return out;
}

/// A source's whole sound, decoded once by the page at `SAMPLE_RATE`.
export class TiloPcmDecoder {
  constructor(url, scope = globalThis) {
    this.stateName = 'decoding';
    this.pcm = null;
    const Offline = scope.OfflineAudioContext;
    if (typeof Offline !== 'function') {
      this.stateName = 'none';
      return;
    }
    scope.fetch(url).then(response => response.arrayBuffer())
      // An offline context decodes straight to its own rate, so the samples
      // come out ready to mix whatever the file holds.
      .then(bytes => new Offline(CHANNELS, 1, SAMPLE_RATE).decodeAudioData(bytes))
      .then(buffer => {
        this.pcm = interleaveStereo(buffer);
        this.stateName = 'ready';
      }, () => {
        // No sound in it, or none the browser decodes: it mixes as silence.
        this.stateName = 'none';
      });
  }

  /// 'decoding', 'ready' or 'none'.
  state() { return this.stateName; }
  /// Samples per channel.
  frames() { return this.pcm ? this.pcm.length / CHANNELS : 0; }
  /// `count` interleaved stereo frames from frame `from`, short at the end.
  samples(from, count) {
    if (!this.pcm) return new Float32Array();
    const start = Math.max(0, Math.floor(from)) * CHANNELS;
    return this.pcm.slice(start, Math.min(this.pcm.length, start + Math.max(0, count) * CHANNELS));
  }
  close() { this.pcm = null; }
}

// How far behind the call the first block plays, so it is never late.
const PLAY_LEAD_S = 0.05;

/// Plays mixed blocks on the page's audio output, each at the timeline time
/// it was mixed for, measured from where `start` put the playhead.
export class TiloPcmPlayer {
  constructor() {
    this.context = null;
    this.nodes = new Set();
    this.origin = 0;
    this.anchor = 0;
  }

  /// Begin playing timeline second `at` a moment from now.
  start(at) {
    this.stop();
    if (!this.context) {
      const Context = globalThis.AudioContext ?? globalThis.webkitAudioContext;
      if (typeof Context !== 'function') return false;
      this.context = new Context({latencyHint: 'interactive'});
    }
    this.context.resume?.()?.catch?.(() => {});
    this.origin = at;
    this.anchor = this.context.currentTime + PLAY_LEAD_S;
    return true;
  }

  /// The timeline second playing now.
  position() {
    if (!this.context) return this.origin;
    return this.origin + Math.max(0, this.context.currentTime - this.anchor);
  }

  /// Play interleaved stereo `samples` mixed for timeline second `at`.
  push(samples, at) {
    const context = this.context;
    const frames = Math.floor(samples.length / CHANNELS);
    if (!context || frames === 0) return;
    const when = this.anchor + (at - this.origin);
    const late = context.currentTime - when;
    if (late >= frames / SAMPLE_RATE) return;
    const buffer = context.createBuffer(CHANNELS, frames, SAMPLE_RATE);
    const left = buffer.getChannelData(0);
    const right = buffer.getChannelData(1);
    for (let i = 0; i < frames; i++) {
      left[i] = samples[i * 2];
      right[i] = samples[i * 2 + 1];
    }
    const node = context.createBufferSource();
    node.buffer = buffer;
    node.connect(context.destination);
    node.onended = () => {
      node.disconnect();
      this.nodes.delete(node);
    };
    node.start(Math.max(when, context.currentTime), Math.max(0, late));
    this.nodes.add(node);
  }

  /// Silence what is scheduled.
  stop() {
    for (const node of this.nodes) {
      try { node.stop(); } catch { /* not started */ }
      node.disconnect();
    }
    this.nodes.clear();
  }

  close() {
    this.stop();
    this.context?.close?.();
    this.context = null;
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

/// A whole WebM/Matroska file holding a `video` track (`{codecId,
/// codecPrivate, width, height, fps}`), and a `sound` track (`{codecId,
/// codecPrivate, rate, channels, codecDelayNs, seekPreRollNs}`) when there is
/// one, of `chunks` (`{track (1 video, 2 sound), data, timestamp (µs),
/// duration (µs), key}`, each track's in decode order).
export function muxMatroska({doc, video, sound = null, chunks}) {
  // Interleaved by time, so a player reading forward finds each track's
  // blocks together; at one instant the picture comes first.
  const ordered = chunks
    .filter(chunk => chunk.track !== 2 || sound)
    .map((chunk, order) => ({...chunk, track: chunk.track ?? 1, order}))
    .sort((a, b) => a.timestamp - b.timestamp || a.track - b.track || a.order - b.order);
  let endUs = 0;
  for (const chunk of ordered) {
    endUs = Math.max(endUs, chunk.timestamp + (chunk.duration ?? (chunk.track === 1 ? 1e6 / video.fps : 0)));
  }
  const header = master(0x1A45DFA3, [
    uint(0x4286, 1), uint(0x42F7, 1), uint(0x42F2, 4), uint(0x42F3, 8),
    str(0x4282, doc), uint(0x4287, 4), uint(0x4285, 2),
  ]);
  const info = master(0x1549A966, [
    uint(0x2AD7B1, 1000000),
    str(0x4D80, 'tilo'),
    str(0x5741, 'tilo'),
    float(0x4489, endUs / 1000),
  ]);
  const picture = [
    uint(0xD7, 1), uint(0x73C5, 1), uint(0x83, 1), str(0x86, video.codecId),
    uint(0x23E383, Math.round(1e9 / video.fps)),
  ];
  if (video.codecPrivate && video.codecPrivate.length) picture.push(element(0x63A2, video.codecPrivate));
  picture.push(master(0xE0, [uint(0xB0, video.width), uint(0xBA, video.height)]));
  const entries = [master(0xAE, picture)];
  if (sound) {
    const audio = [
      uint(0xD7, 2), uint(0x73C5, 2), uint(0x83, 2), str(0x86, sound.codecId),
    ];
    if (sound.codecPrivate && sound.codecPrivate.length) audio.push(element(0x63A2, sound.codecPrivate));
    audio.push(uint(0x56AA, sound.codecDelayNs ?? 0), uint(0x56BB, sound.seekPreRollNs ?? 0));
    audio.push(master(0xE1, [float(0xB5, sound.rate), uint(0x9F, sound.channels)]));
    entries.push(master(0xAE, audio));
  }
  const tracks = master(0x1654AE6B, entries);

  // Clusters, each starting on a key frame once the last has run long
  // enough, so a player can seek to any cluster.
  const clusters = [];
  let current = null;
  for (const chunk of ordered) {
    const ms = Math.round(chunk.timestamp / 1000);
    const pictureKey = chunk.track === 1 && chunk.key;
    const startNew = !current || (pictureKey && ms - current.ms >= 1000) || ms - current.ms >= CLUSTER_SPAN_MS;
    if (startNew) {
      current = {ms, key: pictureKey, blocks: []};
      clusters.push(current);
    }
    const head = new Uint8Array(4);
    head[0] = 0x80 | chunk.track;
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

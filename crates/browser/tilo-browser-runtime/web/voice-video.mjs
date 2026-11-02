/**
 * A browser participant's pictures in a call: the camera (`getUserMedia`) or
 * a shared screen (`getDisplayMedia`) encoded here as one H.264 stream per
 * layer of its source, and the pictures the call sends for this page waiting
 * in an inbox until the app collects them.
 *
 * A layer is encoded (WebCodecs, hardware where the browser has it) only
 * while a device in the call shows it, so nothing is encoded for nobody
 * (`wanted(layer)`). A picture that stops changing is encoded again as a
 * keyframe every `REPEAT_MS`, so someone who starts showing it later sees it
 * at once, and `keyframe()` makes the next picture of each layer one, which
 * is what a device that fell behind asks for.
 *
 * What the encoder writes is one picture's coded bytes and its place in the
 * source's stream; the call seals it and puts it in a tilo-video/1 object
 * (tilo-call-lane `page`), so the page never speaks that format for what it
 * sends. What arrives is already such an object, and the app decodes it.
 */
import {SOURCES, base64Of, encodeVideoObject, fitted} from './video-profile.mjs';

const KEYFRAME_MS = 2000;
const REPEAT_MS = 2000;
const FPS = {camera: 30, screen: 15};
const INBOX_BYTES = 8 * 1024 * 1024;
/** Frames an encoder may hold before new ones are skipped. */
const ENCODE_QUEUE = 2;
/** The layers of each source, in their wire order. */
export const LAYER_CODES = Object.freeze({low: 0, mid: 1, high: 2});
/** The wire codes of the sources. */
export const SOURCE_CODES = Object.freeze({camera: 0, screen: 1});

/** The layer of a wire code, or null for one this profile has none of. */
export function layerOfCode(code) {
  return Object.keys(LAYER_CODES).find(name => LAYER_CODES[name] === code) ?? null;
}

/** The source of a wire code, or null for one this profile has none of. */
export function sourceOfCode(code) {
  return Object.keys(SOURCE_CODES).find(name => SOURCE_CODES[name] === code) ?? null;
}

/** Open this person's camera or a screen they choose: a stream with one video track. */
export async function openSource(source, environment = globalThis) {
  const devices = environment.navigator?.mediaDevices;
  if (!devices) throw new Error('this browser has no camera or screen sharing');
  if (source === 'camera') {
    return devices.getUserMedia({audio: false, video: {width: {ideal: 1280}, height: {ideal: 720}, frameRate: {ideal: FPS.camera}}});
  }
  return devices.getDisplayMedia({audio: false, video: {frameRate: {ideal: FPS.screen}}});
}

/** The H.264 profile a layer of `height` lines is encoded in: constrained baseline. */
const avcCodec = height => (height > 720 ? 'avc1.42E028' : 'avc1.42E01F');

/** The frames of `stream`'s video track, one `onFrame(frame)` each; returns its stop. */
function readFrames(stream, environment, onFrame, onEnded) {
  const [videoTrack] = stream.getVideoTracks();
  if (!videoTrack) throw new Error('the stream has no picture');
  videoTrack.addEventListener?.('ended', onEnded, {once: true});
  let stopped = false;
  if (typeof environment.MediaStreamTrackProcessor === 'function') {
    const processor = new environment.MediaStreamTrackProcessor({track: videoTrack});
    const reader = processor.readable.getReader();
    void (async () => {
      try {
        for (;;) {
          const {value, done} = await reader.read();
          if (done || stopped) { value?.close(); return; }
          onFrame(value);
        }
      } catch {
        if (!stopped) onEnded();
      }
    })();
    return () => { stopped = true; void reader.cancel().catch(() => {}); };
  }
  // Without a track processor: a hidden video element, read on each of
  // its frames.
  const element = environment.document.createElement('video');
  element.muted = true;
  element.playsInline = true;
  element.srcObject = stream;
  void element.play().catch(() => {});
  const next = () => {
    if (stopped) return;
    if (element.videoWidth && element.videoHeight) {
      try { onFrame(new environment.VideoFrame(element, {timestamp: performance.now() * 1000})); } catch {}
    }
    if (typeof element.requestVideoFrameCallback === 'function') element.requestVideoFrameCallback(next);
    else setTimeout(next, 1000 / FPS.camera);
  };
  next();
  return () => { stopped = true; element.srcObject = null; };
}

/**
 * Encode `stream`'s pictures as `source` in the layers `wanted(layer)` names
 * (`low`, `mid`, `high`). `onPicture` receives each coded picture:
 * `{source, layer, sequence, keyframe, timestampUs, width, height, payload}`,
 * with `sequence` this source's own count. Returns `{stop, keyframe}`;
 * `onEnded` runs when the person ends the capture in the browser's own
 * controls.
 */
export function createPictureEncoder({source, stream, environment = globalThis, wanted = () => false, onPicture = () => {}, onEnded = () => {}, report = () => {}}) {
  const layers = SOURCES[source];
  if (!layers) throw new RangeError(`unknown picture source ${source}`);
  const outputs = new Map();
  for (const layer of Object.keys(layers)) {
    outputs.set(layer, {layer, encoder: null, sequence: 0, size: null, canvas: null, context: null, lastKey: -Infinity, needKey: true});
  }
  let stopped = false;
  let last = null;
  let repeatTimer;

  function closeEncoder(output) {    try { output.encoder?.close(); } catch {}
    output.encoder = null;
    output.needKey = true;
  }

  function encoderFor(output, width, height) {
    const [w, h] = fitted(width, height, layers[output.layer]);
    if (output.encoder && output.size?.[0] === w && output.size?.[1] === h && output.encoder.state === 'configured') return output.encoder;
    closeEncoder(output);
    output.size = [w, h];
    const encoder = new environment.VideoEncoder({
      output: (chunk) => written(output, chunk),
      error: error => { report({type: 'video-encoder-error', error}); closeEncoder(output); },
    });
    encoder.configure({codec: avcCodec(h), width: w, height: h, bitrate: layers[output.layer].bitrate, framerate: FPS[source], latencyMode: 'realtime', avc: {format: 'annexb'}});
    output.encoder = encoder;
    return encoder;
  }

  function written(output, chunk) {
    const payload = new Uint8Array(chunk.byteLength);
    chunk.copyTo(payload);
    const [width, height] = output.size;
    let picture;
    try {
      picture = {source, layer: output.layer, sequence: output.sequence, keyframe: chunk.type === 'key', timestampUs: Math.max(0, Math.round(chunk.timestamp)), width, height, payload};
      output.sequence++;
    } catch (error) {
      report({type: 'video-object-error', error});
      return;
    }
    try { onPicture(picture); } catch (error) { report({type: 'video-picture-error', error}); }
  }

  function encode(frame, repeat) {
    const at = performance.now();
    for (const output of outputs.values()) {
      const wantedNow = wanted(output.layer);
      if (!wantedNow) { if (output.encoder) closeEncoder(output); continue; }
      let encoder;
      try { encoder = encoderFor(output, frame.displayWidth, frame.displayHeight); } catch (error) { report({type: 'video-encoder-error', error}); continue; }
      if (encoder.encodeQueueSize > ENCODE_QUEUE) continue;
      const [width, height] = output.size;
      let scaled = frame;
      if (width !== frame.displayWidth || height !== frame.displayHeight) {
        if (!output.canvas || output.canvas.width !== width || output.canvas.height !== height) {
          output.canvas = new environment.OffscreenCanvas(width, height);
          output.context = output.canvas.getContext('2d');
        }
        output.context.drawImage(frame, 0, 0, width, height);
        scaled = new environment.VideoFrame(output.canvas, {timestamp: frame.timestamp});
      }
      const keyFrame = repeat || output.needKey || at - output.lastKey >= KEYFRAME_MS;
      if (keyFrame) { output.lastKey = at; output.needKey = false; }
      try { encoder.encode(scaled, {keyFrame}); } catch (error) { report({type: 'video-encode-error', error}); }
      if (scaled !== frame) scaled.close();
    }
  }

  function repeatLater() {
    clearTimeout(repeatTimer);
    repeatTimer = setTimeout(() => {
      if (stopped || !last) return;
      // A still picture: the last one again, as a keyframe.
      const again = new environment.VideoFrame(last, {timestamp: performance.now() * 1000});
      encode(again, true);
      again.close();
      repeatLater();
    }, REPEAT_MS);
  }

  const stopFrames = readFrames(stream, environment, frame => {
    if (stopped) { frame.close(); return; }
    encode(frame, false);
    last?.close();
    last = frame;
    repeatLater();
  }, () => { if (!stopped) onEnded(); });

  return {
    source,
    /** Start every layer's next picture at a keyframe. */
    keyframe() {
      for (const output of outputs.values()) output.needKey = true;
    },
    /** Stop encoding; the capture itself stops too unless `keepStream`. */
    stop({keepStream = false} = {}) {
      if (stopped) return;
      stopped = true;
      clearTimeout(repeatTimer);
      stopFrames();
      last?.close();
      last = null;
      for (const output of outputs.values()) closeEncoder(output);
      if (!keepStream) for (const track of stream.getTracks()) track.stop();
    },
  };
}

/** The tilo-video/1 object of `picture` (see [`createPictureEncoder`]): what
 * this page's own tile draws without the call sending it back. */
export function pictureObject({source, layer, sequence, keyframe, timestampUs, width, height, payload}) {
  return encodeVideoObject({keyframe, layer, sequence, timestampUs, width, height, payload});
}

/**
 * Received pictures waiting for the app, by cursor; a stream that drops a
 * picture waits for its next keyframe (shell `video::Inbox`).
 */
export function createPictureInbox({maxBytes = INBOX_BYTES} = {}) {
  let queue = [];
  let next = 0;
  let bytes = 0;
  const behind = new Set();
  const waiters = new Set();
  const keyOf = picture => `${picture.stream}|${picture.source}`;
  function dropStream(key) {
    queue = queue.filter(entry => {
      if (keyOf(entry.picture) !== key) return true;
      bytes -= entry.size;
      return false;
    });
  }
  return {
    /** Queue `picture` (`{person, stream, source, object}` with `object` the tilo-video/1 bytes). */
    admit(picture, keyframe) {
      const key = keyOf(picture);
      if (keyframe) behind.delete(key);
      else if (behind.has(key)) return;
      const size = picture.object.byteLength;
      while (bytes + size > maxBytes && queue.length) {
        const oldest = keyOf(queue[0].picture);
        dropStream(oldest);
        behind.add(oldest);
        if (oldest === key) return;
      }
      next++;
      bytes += size;
      queue.push({cursor: next, size, picture});
      for (const wake of [...waiters]) wake();
    },
    /** Forget what the app collected (up to `after`); the next pictures,
     * up to `budget` bytes but at least one, with `object` in base64. */
    take(after, budget = 600 * 1024) {
      queue = queue.filter(entry => {
        if (entry.cursor > after) return true;
        bytes -= entry.size;
        return false;
      });
      const pictures = [];
      let cursor = after;
      let taken = 0;
      for (const entry of queue) {
        if (pictures.length && taken + entry.size > budget) break;
        taken += entry.size;
        cursor = entry.cursor;
        pictures.push({...entry.picture, object: base64Of(entry.picture.object)});
      }
      return {pictures, cursor};
    },
    /** Resolves when a picture arrives, or `ms` passed. */
    wait(ms) {
      return new Promise(resolve => {
        let timer;
        const wake = () => { clearTimeout(timer); waiters.delete(wake); resolve(); };
        timer = setTimeout(wake, ms);
        waiters.add(wake);
      });
    },
    /** Some of `stream`'s pictures were dropped before they got here. */
    behind(stream, source) {
      const key = `${stream}|${source}`;
      dropStream(key);
      behind.add(key);
    },
    clear() { queue = []; bytes = 0; behind.clear(); },
  };
}

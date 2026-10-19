/**
 * A browser participant's pictures in a call, as a Tilo device sends and
 * receives them (tilo-video/1, web/video-profile.mjs; the shell's
 * `native_media::video`):
 *
 * - **Sending.** The camera (`getUserMedia`) or a shared screen
 *   (`getDisplayMedia`) is published as one track per layer of its source.
 *   A layer is encoded (WebCodecs, H.264, hardware where the browser has
 *   it) only while someone subscribes to its track (`track.used`), so
 *   nothing is encoded for nobody. A picture that stops changing is sent
 *   again as a keyframe every `REPEAT_MS`, so someone who starts showing
 *   it later sees it at once.
 * - **Receiving.** The pictures the app shows arrive on the tracks it asks
 *   for and wait in an inbox until the app collects them, coded; the app
 *   decodes them (tilo-media's page decoder). A stream that falls behind
 *   starts over at its next keyframe.
 */
import {SOURCES, base64Of, decodeVideoObject, encodeVideoObject, fitted, videoTrack} from './video-profile.mjs';

const KEYFRAME_MS = 2000;
const REPEAT_MS = 2000;
const FPS = {camera: 30, screen: 15};
const PICTURE_AGE_MS = 2000;
const INBOX_BYTES = 8 * 1024 * 1024;
/** Frames an encoder may hold before new ones are skipped. */
const ENCODE_QUEUE = 2;

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
 * Publish `stream`'s pictures as `source` on `broadcast` (a `@moq/net`
 * broadcast producer) through `moq`. `own(layer, object)` receives this
 * person's own pictures at the layer `ownLayer()` names (their own tile).
 * Returns `{stop()}`; `onEnded` runs when the person ends the capture in
 * the browser's own controls.
 */
export function publishPictures({moq, broadcast, source, stream, environment = globalThis, ownLayer = () => null, own = () => {}, onEnded = () => {}, report = () => {}}) {
  const layers = SOURCES[source];
  if (!layers) throw new RangeError(`unknown picture source ${source}`);
  const started = performance.now();
  const outputs = new Map();
  for (const layer of Object.keys(layers)) {
    const track = broadcast.createTrack(videoTrack(source, layer), {timescale: moq.Time.Timescale.MICRO, maxAge: PICTURE_AGE_MS});
    outputs.set(layer, {layer, track, encoder: null, group: null, sequence: 0, size: null, canvas: null, context: null, lastKey: -Infinity, needKey: true});
  }
  let stopped = false;
  let last = null;
  let repeatTimer;

  function closeEncoder(output) {
    try { output.encoder?.close(); } catch {}
    output.encoder = null;
    try { output.group?.close(); } catch {}
    output.group = null;
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
    const keyframe = chunk.type === 'key';
    const timestampUs = Math.max(0, Math.round(chunk.timestamp));
    const [width, height] = output.size;
    let object;
    try {
      object = encodeVideoObject({keyframe, layer: output.layer, sequence: output.sequence++, timestampUs, width, height, payload});
    } catch (error) {
      report({type: 'video-object-error', error});
      return;
    }
    if (ownLayer() === output.layer) own(output.layer, object, keyframe);
    if (!output.track.used.peek()) return;
    if (keyframe) {
      try { output.group?.close(); } catch {}
      output.group = output.track.appendGroup();
    }
    if (!output.group) return;
    try {
      output.group.writeFrame({payload: object, timestamp: new moq.Time.Timestamp(timestampUs, moq.Time.Timescale.MICRO)});
    } catch (error) {
      report({type: 'video-publish-error', error});
      output.group = null;
      output.needKey = true;
    }
  }

  function encode(frame, repeat) {
    const at = performance.now();
    for (const output of outputs.values()) {
      const wanted = output.track.used.peek() || ownLayer() === output.layer;
      if (!wanted) { if (output.encoder) closeEncoder(output); continue; }
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
      const again = new environment.VideoFrame(last, {timestamp: (performance.now() - started) * 1000});
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
    /** Stop publishing; the capture itself stops too unless `keepStream`
     * (the call moves to a new carrier and publishes it again). */
    stop({keepStream = false} = {}) {
      if (stopped) return;
      stopped = true;
      clearTimeout(repeatTimer);
      stopFrames();
      last?.close();
      last = null;
      for (const output of outputs.values()) {
        closeEncoder(output);
        try { broadcast.removeTrack(output.track.name); } catch {}
        try { output.track.close(); } catch {}
      }
      if (!keepStream) for (const track of stream.getTracks()) track.stop();
    },
  };
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

/**
 * Read the picture tracks shown of each publisher of the call `callPath`
 * from `incoming` (a `@moq/net` origin), into `deliver(publisher, source,
 * object bytes, keyframe)`. `show([{publisher, source, layer}])` replaces
 * what is read. Returns `{show, close}`.
 */
export function receivePictures({moq, incoming, call, deliver, report = () => {}}) {
  const reading = new Map();
  let closed = false;
  function stopReading(key) {
    const entry = reading.get(key);
    if (!entry) return;
    reading.delete(key);
    entry.active = false;
    try { entry.subscriber?.close(); } catch {}
    try { entry.request?.close(); } catch {}
  }
  function startReading(key, {publisher, source, layer}) {
    const entry = {active: true, layer, request: undefined, subscriber: undefined};
    reading.set(key, entry);
    void (async () => {
      try {
        entry.request = incoming.request(moq.Path.from(call, publisher));
        let broadcast;
        while (entry.active && !(broadcast = entry.request.active.peek())) await entry.request.active.changed();
        if (!entry.active) return;
        entry.subscriber = broadcast.track(videoTrack(source, layer)).subscribe({priority: 1, maxAge: PICTURE_AGE_MS});
        let expectKey = true;
        for (;;) {
          const group = await entry.subscriber.recvGroup();
          if (!group || !entry.active) return;
          try {
            for (;;) {
              const frame = await group.readFrame();
              if (!frame || !entry.active) break;
              let object;
              try { object = decodeVideoObject(frame.payload); } catch { report({type: 'video-receive-drop', reason: 'invalid-object'}); break; }
              if (expectKey && !object.keyframe) break;
              expectKey = false;
              deliver(publisher, source, frame.payload, object.keyframe);
            }
          } catch {
            // A group cut short: its later pictures are lost; start again at a keyframe.
            expectKey = true;
          } finally {
            try { group.close(); } catch {}
          }
        }
      } catch (error) {
        if (entry.active) report({type: 'video-subscription-error', error});
      } finally {
        if (reading.get(key) === entry) reading.delete(key);
      }
    })();
  }
  return {
    show(wanted) {
      if (closed) return;
      const next = new Map(wanted.map(item => [`${item.publisher}|${item.source}`, item]));
      for (const [key, entry] of [...reading]) {
        const want = next.get(key);
        if (!want || want.layer !== entry.layer) stopReading(key);
      }
      for (const [key, item] of next) if (!reading.has(key)) startReading(key, item);
    },
    close() {
      closed = true;
      for (const key of [...reading.keys()]) stopReading(key);
    },
  };
}

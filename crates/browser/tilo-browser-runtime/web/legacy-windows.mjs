/**
 * Legacy (non-Tilo) windows on the mounted desktop. The session streams each
 * legacy window it runs (a browser, an office suite: anything that is not a
 * Tilo app) as H.264 video; the workspace host re-publishes those streams on
 * LIVE_V1 record tracks in the session device's namespace
 * (`live_wire::legacy_window`). This layer subscribes to every member's
 * legacy-window index, decodes each listed stream with WebCodecs and draws it
 * over the desktop tab showing that app, one canvas per stream on a DOM layer
 * above the WebGPU canvas. Those tabs are the ones this build has no
 * application for (`legacy` in the runtime's live snapshot).
 *
 * A member whose live grant may publish drives the window: pointer and keys
 * on its canvas go to the host as `legacy_window::InputRequest`s, which the
 * host forwards to the session. A viewer's canvas takes no input, and the
 * host refuses a viewer's input anyway.
 *
 * Frames: `0xF1 | kind u8 | payload`; kind 1 index (JSON streams), 2 start
 * (JSON stream, first frame of every group), 3 chunk (`flags u8 | seq u32 |
 * ts_us u64 | total u32 | bytes`, big endian), 4 end.
 */

export const LEGACY_APP = 'so.tilo.legacy-window';
export const INDEX_RECORD = Object.freeze({app: LEGACY_APP, record: '@tilo:legacy-windows'});
const FRAME_TAG = 0xf1;
const KIND_INDEX = 1;
const KIND_START = 2;
const KIND_CHUNK = 3;
const KIND_END = 4;
export const CHUNK_KEY = 1;
export const CHUNK_FIRST = 2;
export const CHUNK_LAST = 4;
const CHUNK_HEADER = 2 + 1 + 4 + 8 + 4;
const MAX_ENCODED_FRAME_BYTES = 4 * 1024 * 1024;
const MAX_STREAMS = 32;
/** The host re-sends a live index every second: a silent publisher's streams are gone. */
export const INDEX_EXPIRE_MS = 5000;
/** Decoder backlog past which frames are skipped until the next keyframe. */
const MAX_DECODE_QUEUE = 6;
const HEX16 = /^[0-9a-f]{16}$/;
const text = new TextDecoder();

function validStream(value) {
  if (!value || typeof value !== 'object') return null;
  const {source, surface, app, codec, width, height, record} = value;
  if (!HEX16.test(source) || !HEX16.test(surface) || typeof app !== 'string' || !app || app.length > 128
    || typeof codec !== 'string' || typeof record !== 'string' || !record || record.length > 128
    || !Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0 || width > 8192 || height > 8192) return null;
  return {source, surface, app, codec, width, height, record};
}

/** One legacy-window frame, or null for anything malformed. */
export function decodeFrame(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.byteLength < 2 || bytes[0] !== FRAME_TAG) return null;
  try {
    switch (bytes[1]) {
      case KIND_INDEX: {
        const streams = JSON.parse(text.decode(bytes.subarray(2)));
        if (!Array.isArray(streams) || streams.length > MAX_STREAMS) return null;
        const valid = streams.map(validStream);
        return valid.every(Boolean) ? {kind: 'index', streams: valid} : null;
      }
      case KIND_START: {
        const stream = validStream(JSON.parse(text.decode(bytes.subarray(2))));
        return stream ? {kind: 'start', stream} : null;
      }
      case KIND_CHUNK: {
        if (bytes.byteLength <= CHUNK_HEADER) return null;
        const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
        return {
          kind: 'chunk',
          flags: bytes[2],
          seq: view.getUint32(3),
          tsUs: Number(view.getBigUint64(7)),
          total: view.getUint32(15),
          data: bytes.subarray(CHUNK_HEADER),
        };
      }
      case KIND_END:
        return bytes.byteLength === 2 ? {kind: 'end'} : null;
      default:
        return null;
    }
  } catch {
    return null;
  }
}

/** Joins a stream's chunks into encoded frames; a gap drops the frame. */
export function createReassembler() {
  let pending = null;
  return {
    /** The complete `{key, seq, tsUs, data}` once its last chunk arrives, else null. */
    push(chunk) {
      if (chunk.flags & CHUNK_FIRST) {
        pending = chunk.total > 0 && chunk.total <= MAX_ENCODED_FRAME_BYTES
          ? {key: Boolean(chunk.flags & CHUNK_KEY), seq: chunk.seq, tsUs: chunk.tsUs, data: new Uint8Array(chunk.total), filled: 0}
          : null;
      } else if (!pending || pending.seq !== chunk.seq) {
        pending = null;
        return null;
      }
      if (!pending || pending.filled + chunk.data.byteLength > pending.data.byteLength) { pending = null; return null; }
      pending.data.set(chunk.data, pending.filled);
      pending.filled += chunk.data.byteLength;
      if (!(chunk.flags & CHUNK_LAST)) return null;
      const done = pending;
      pending = null;
      return done.filled === done.data.byteLength ? {key: done.key, seq: done.seq, tsUs: done.tsUs, data: done.data} : null;
    },
    reset() { pending = null; },
  };
}

/** `avc1.PPCCLL` from the first sequence parameter set of an Annex B access unit. */
export function h264CodecString(accessUnit) {
  const hex = byte => byte.toString(16).padStart(2, '0').toUpperCase();
  for (let i = 0; i + 6 < accessUnit.byteLength; i++) {
    if (accessUnit[i] === 0 && accessUnit[i + 1] === 0 && accessUnit[i + 2] === 1 && (accessUnit[i + 3] & 0x1f) === 7) {
      return `avc1.${hex(accessUnit[i + 4])}${hex(accessUnit[i + 5])}${hex(accessUnit[i + 6])}`;
    }
  }
  return 'avc1.42E01F';
}

/**
 * WebCodecs decode of one stream: configured from its first keyframe, frames
 * before a keyframe (or after a backlog) are skipped until the next one.
 */
export function createStreamDecoder({VideoDecoder, EncodedVideoChunk, onFrame, onError = () => {}}) {
  let decoder = null;
  let codec = null;
  let waitKey = true;
  const stats = {decoded: 0, skipped: 0, errors: 0};
  function close() {
    try { if (decoder && decoder.state !== 'closed') decoder.close(); } catch {}
    decoder = null;
    codec = null;
    waitKey = true;
  }
  function open(next) {
    close();
    decoder = new VideoDecoder({
      output: frame => { stats.decoded++; onFrame(frame); },
      error: error => { stats.errors++; onError(error); decoder = null; codec = null; waitKey = true; },
    });
    decoder.configure({codec: next, optimizeForLatency: true, hardwareAcceleration: 'no-preference'});
    codec = next;
  }
  return {
    stats,
    decode(frame) {
      if (frame.key) {
        const next = h264CodecString(frame.data);
        try {
          if (!decoder || decoder.state === 'closed' || codec !== next) open(next);
        } catch (error) { stats.errors++; onError(error); close(); return false; }
        waitKey = false;
      }
      if (waitKey || !decoder) { stats.skipped++; return false; }
      if (!frame.key && decoder.decodeQueueSize > MAX_DECODE_QUEUE) { waitKey = true; stats.skipped++; return false; }
      try {
        decoder.decode(new EncodedVideoChunk({type: frame.key ? 'key' : 'delta', timestamp: frame.tsUs, data: frame.data}));
        return true;
      } catch (error) {
        stats.errors++;
        onError(error);
        waitKey = true;
        return false;
      }
    },
    reset() { waitKey = true; },
    close,
  };
}

/** Linux input codes of the DOM's primary, middle and secondary buttons. */
const BUTTONS = [272, 274, 273];
const LETTERS = {Q: 16, W: 17, E: 18, R: 19, T: 20, Y: 21, U: 22, I: 23, O: 24, P: 25, A: 30, S: 31, D: 32, F: 33, G: 34, H: 35, J: 36, K: 37, L: 38, Z: 44, X: 45, C: 46, V: 47, B: 48, N: 49, M: 50};
const KEYS = {
  Escape: 1, Minus: 12, Equal: 13, Backspace: 14, Tab: 15, BracketLeft: 26, BracketRight: 27, Enter: 28, ControlLeft: 29,
  Semicolon: 39, Quote: 40, Backquote: 41, ShiftLeft: 42, Backslash: 43, Comma: 51, Period: 52, Slash: 53, ShiftRight: 54,
  AltLeft: 56, Space: 57, CapsLock: 58, F11: 87, F12: 88, ControlRight: 97, AltRight: 100, Home: 102, ArrowUp: 103,
  PageUp: 104, ArrowLeft: 105, ArrowRight: 106, End: 107, ArrowDown: 108, PageDown: 109, Insert: 110, Delete: 111,
  MetaLeft: 125, MetaRight: 126,
};
/** Most events one input request carries (`legacy_window::MAX_INPUT_EVENTS`). */
const MAX_INPUT_EVENTS = 64;

/** The Linux evdev keycode of a `KeyboardEvent.code`, or null. */
export function evdevKey(code) {
  if (typeof code !== 'string') return null;
  if (/^Key[A-Z]$/.test(code)) return LETTERS[code[3]];
  if (/^Digit[0-9]$/.test(code)) return code[5] === '0' ? 11 : Number(code[5]) + 1;
  if (/^F([1-9]|10)$/.test(code)) return 58 + Number(code.slice(1));
  return KEYS[code] ?? null;
}

/**
 * The window pixel under a point, for a canvas showing a `width`×`height`
 * picture letterboxed (`object-fit: contain`) into `box`; null outside it.
 */
export function windowPoint(box, width, height, clientX, clientY) {
  if (!(box.width > 0 && box.height > 0 && width > 0 && height > 0)) return null;
  const scale = Math.min(box.width / width, box.height / height);
  const x = (clientX - box.left - (box.width - width * scale) / 2) / scale;
  const y = (clientY - box.top - (box.height - height * scale) / 2) / scale;
  return x >= 0 && y >= 0 && x < width && y < height ? [Math.round(x * 100) / 100, Math.round(y * 100) / 100] : null;
}

const validRect = rect => Array.isArray(rect) && rect.length === 4 && rect.every(Number.isFinite) && rect[2] > 0 && rect[3] > 0;

/**
 * Which legacy tab each stream is drawn over: a tab of the same app id, in
 * desktop order, keeping earlier assignments while their tab still exists.
 * Returns `Map<streamKey, {tab, content}>`.
 */
export function matchStreams(snapshot, streams, previous = new Map()) {
  const tiles = [];
  if (snapshot && !snapshot.covered) {
    for (const group of snapshot.groups ?? []) {
      const legacy = group?.legacy;
      if (legacy && typeof legacy.app === 'string' && typeof legacy.tab === 'string' && validRect(legacy.content)) tiles.push(legacy);
    }
  }
  const free = new Map(tiles.map(tile => [tile.tab, tile]));
  const out = new Map();
  const same = (a, b) => a.toLowerCase() === b.toLowerCase();
  for (const [key, stream] of streams) {
    const tab = previous.get(key)?.tab;
    const tile = tab !== undefined ? free.get(tab) : undefined;
    if (tile && same(tile.app, stream.app)) { out.set(key, {tab, content: tile.content}); free.delete(tab); }
  }
  for (const [key, stream] of streams) {
    if (out.has(key)) continue;
    const tile = [...free.values()].find(candidate => same(candidate.app, stream.app));
    if (tile) { out.set(key, {tab: tile.tab, content: tile.content}); free.delete(tile.tab); }
  }
  return out;
}

function defaultRequestFrame(callback) {
  if (typeof globalThis.requestAnimationFrame === 'function' && globalThis.document?.visibilityState !== 'hidden') {
    return {raf: globalThis.requestAnimationFrame(callback)};
  }
  return {timer: globalThis.setTimeout(callback, 250)};
}
function defaultCancelFrame(handle) {
  if (handle?.raf !== undefined) globalThis.cancelAnimationFrame(handle.raf);
  else if (handle?.timer !== undefined) globalThis.clearTimeout(handle.timer);
}

/**
 * Bind the legacy-window layer to a mounted desktop surface.
 *
 * - `live()`: current `session.liveV1` or undefined.
 * - `snapshot()`: the runtime's `live_view_snapshot` JSON (or null).
 * - `presented()`: a value that changes whenever the runtime presents a frame.
 */
export function createLegacyWindows({
  surface,
  live,
  snapshot,
  presented = () => surface?.dataset?.frames,
  document: doc = globalThis.document,
  VideoDecoder = globalThis.VideoDecoder,
  EncodedVideoChunk = globalThis.EncodedVideoChunk,
  now = () => performance.now(),
  requestFrame = defaultRequestFrame,
  cancelFrame = defaultCancelFrame,
  setInterval: every = (callback, ms) => globalThis.setInterval(callback, ms),
  clearInterval: stopEvery = handle => globalThis.clearInterval(handle),
  onError = () => {},
}) {
  const report = error => { try { onError(error); } catch {} };
  const layer = doc.createElement('div');
  layer.className = 'tilo-legacy-windows';
  layer.style.cssText = 'position:absolute;inset:0;pointer-events:none;overflow:hidden;contain:strict;z-index:1';
  surface.append(layer);
  const streams = new Map(); // `${publisher}/${record}` -> entry
  const indexed = new Map(); // publisher -> last index time
  let placement = new Map();
  const stats = {indexes: 0, frames: 0, drawn: 0, placed: 0, inputSent: 0, inputRefused: 0};
  let api;
  let unwatchIndex;
  let frame;
  let closed = false;
  let lastPresented;
  let layoutDirty = true;

  function streamKey(publisher, record) { return `${publisher}/${record}`; }
  function schedule() {
    if (closed || frame !== undefined) return;
    frame = requestFrame(tick);
  }
  function removeStream(key) {
    const entry = streams.get(key);
    if (!entry) return;
    streams.delete(key);
    try { entry.unwatch(); } catch {}
    entry.decoder.close();
    entry.canvas.remove();
    layoutDirty = true;
  }
  function addStream(publisher, stream) {
    const key = streamKey(publisher, stream.record);
    const existing = streams.get(key);
    if (existing) { existing.stream = stream; return; }
    const canvas = doc.createElement('canvas');
    canvas.className = 'tilo-legacy-window';
    canvas.dataset.app = stream.app;
    canvas.width = stream.width;
    canvas.height = stream.height;
    canvas.style.cssText = 'position:absolute;left:0;top:0;display:none;object-fit:contain;background:#000;transform-origin:0 0';
    layer.append(canvas);
    const entry = {publisher, stream, canvas, context: null, reassembler: createReassembler(), visible: false, rect: ''};
    entry.decoder = createStreamDecoder({
      VideoDecoder, EncodedVideoChunk,
      onFrame: picture => draw(entry, picture),
      onError: report,
    });
    entry.unwatch = api.watchRecord({app: LEGACY_APP, record: stream.record}, (from, bytes) => {
      if (from === publisher) onStreamFrame(entry, bytes);
    });
    bindInput(entry);
    streams.set(key, entry);
    layoutDirty = true;
  }
  // ---- Input (a member whose grant may publish; the host refuses anyone else) --
  function queueInput(entry, event) {
    if (!api?.canControl) return;
    const motion = 'PointerMotion' in event;
    const last = entry.input.at(-1);
    // Latest-wins pointer motion; buttons, keys and focus keep their order.
    if (motion && last && 'PointerMotion' in last) entry.input[entry.input.length - 1] = event;
    else entry.input.push(event);
    if (!entry.flushing) void flushInput(entry);
  }
  async function flushInput(entry) {
    entry.flushing = true;
    try {
      while (entry.input.length && api?.canControl) {
        const events = entry.input.splice(0, MAX_INPUT_EVENTS);
        const sent = await api.sendLegacyInput({record: entry.stream.record, events});
        if (sent.ok) stats.inputSent += events.length;
        else stats.inputRefused += events.length;
      }
    } catch (error) { report(error); } finally {
      entry.input.length = 0;
      entry.flushing = false;
    }
  }
  function bindInput(entry) {
    const {canvas} = entry;
    entry.input = [];
    entry.focused = false;
    canvas.tabIndex = -1;
    const surface = 0;
    const point = event => windowPoint(canvas.getBoundingClientRect(), canvas.width, canvas.height, event.clientX, event.clientY);
    const motion = event => {
      const at = point(event);
      if (at) queueInput(entry, {PointerMotion: {x: at[0], y: at[1]}});
      return at;
    };
    const on = (type, handler) => canvas.addEventListener?.(type, handler);
    on('pointerenter', event => { queueInput(entry, {PointerEnter: {surface}}); motion(event); });
    on('pointermove', motion);
    on('pointerleave', () => queueInput(entry, {PointerLeave: {surface}}));
    on('pointerdown', event => {
      if (!api?.canControl) return;
      event.preventDefault();
      canvas.focus?.({preventScroll: true});
      if (!entry.focused) { entry.focused = true; queueInput(entry, {KeyboardFocus: {surface, focused: true}}); }
      if (motion(event)) queueInput(entry, {PointerButton: {button: BUTTONS[event.button] ?? 272, pressed: true}});
    });
    on('pointerup', event => {
      if (!api?.canControl) return;
      event.preventDefault();
      queueInput(entry, {PointerButton: {button: BUTTONS[event.button] ?? 272, pressed: false}});
    });
    on('contextmenu', event => { if (api?.canControl) event.preventDefault(); });
    const key = pressed => event => {
      const keysym = evdevKey(event.code);
      if (!api?.canControl || keysym === null || (pressed && event.repeat)) return;
      event.preventDefault();
      queueInput(entry, {Key: {keysym, pressed, modifiers: 0}});
    };
    on('keydown', key(true));
    on('keyup', key(false));
    on('blur', () => {
      if (!entry.focused) return;
      entry.focused = false;
      queueInput(entry, {KeyboardFocus: {surface, focused: false}});
    });
  }
  function draw(entry, picture) {
    try {
      if (!streams.has(streamKey(entry.publisher, entry.stream.record))) return;
      const {canvas} = entry;
      if (canvas.width !== picture.displayWidth || canvas.height !== picture.displayHeight) {
        canvas.width = picture.displayWidth;
        canvas.height = picture.displayHeight;
        entry.context = null;
      }
      entry.context ??= canvas.getContext('2d', {alpha: false});
      entry.context?.drawImage(picture, 0, 0, canvas.width, canvas.height);
      stats.drawn++;
    } catch (error) { report(error); } finally { picture.close(); }
  }
  function onStreamFrame(entry, bytes) {
    const value = decodeFrame(bytes);
    if (!value) return;
    stats.frames++;
    if (value.kind === 'start') {
      if (value.stream.record === entry.stream.record) entry.stream = value.stream;
      entry.reassembler.reset();
    } else if (value.kind === 'chunk') {
      const encoded = entry.reassembler.push(value);
      if (encoded) entry.decoder.decode(encoded);
    } else if (value.kind === 'end') {
      entry.decoder.reset();
      entry.reassembler.reset();
    }
  }
  function onIndex(publisher, bytes) {
    const value = decodeFrame(bytes);
    if (value?.kind !== 'index') return;
    stats.indexes++;
    indexed.set(publisher, now());
    const listed = new Set();
    for (const stream of value.streams) {
      listed.add(streamKey(publisher, stream.record));
      addStream(publisher, stream);
    }
    for (const [key, entry] of streams) if (entry.publisher === publisher && !listed.has(key)) removeStream(key);
    schedule();
  }
  function expire() {
    const t = now();
    for (const [publisher, at] of indexed) {
      if (t - at < INDEX_EXPIRE_MS) continue;
      indexed.delete(publisher);
      for (const [key, entry] of streams) if (entry.publisher === publisher) removeStream(key);
    }
  }
  function attach() {
    const next = live();
    if (next === api) return;
    try { unwatchIndex?.(); } catch {}
    unwatchIndex = undefined;
    for (const key of [...streams.keys()]) removeStream(key);
    indexed.clear();
    api = next;
    if (api && typeof api.watchRecord === 'function') unwatchIndex = api.watchRecord(INDEX_RECORD, onIndex);
  }
  function layout() {
    let current = null;
    try {
      const raw = snapshot();
      current = raw ? (typeof raw === 'string' ? JSON.parse(raw) : raw) : null;
    } catch (error) { report(error); }
    const listed = new Map([...streams].map(([key, entry]) => [key, entry.stream]));
    placement = matchStreams(current, listed, placement);
    stats.placed = placement.size;
    for (const [key, entry] of streams) {
      const place = placement.get(key);
      if (!place) {
        if (entry.visible) { entry.canvas.style.display = 'none'; entry.visible = false; }
        continue;
      }
      const [x, y, w, h] = place.content;
      const rect = `${x.toFixed(1)},${y.toFixed(1)},${w.toFixed(1)},${h.toFixed(1)}`;
      if (entry.rect !== rect) {
        entry.rect = rect;
        entry.canvas.style.width = `${w.toFixed(1)}px`;
        entry.canvas.style.height = `${h.toFixed(1)}px`;
        entry.canvas.style.transform = `translate3d(${x.toFixed(1)}px,${y.toFixed(1)}px,0)`;
      }
      entry.canvas.dataset.tab = place.tab;
      const events = api?.canControl ? 'auto' : 'none';
      if (entry.canvas.style.pointerEvents !== events) entry.canvas.style.pointerEvents = events;
      if (!entry.visible) { entry.canvas.style.display = 'block'; entry.visible = true; }
    }
  }
  function tick() {
    frame = undefined;
    if (closed) return;
    const value = presented();
    if (layoutDirty || value !== lastPresented) {
      lastPresented = value;
      layoutDirty = false;
      layout();
    }
    // Windows move with every presented frame (scrolls, drags, resizes).
    if (streams.size) schedule();
  }
  const poll = every(() => { attach(); expire(); layoutDirty = true; schedule(); }, 1000);
  attach();

  return {
    layer,
    stats: () => ({...stats, streams: streams.size, visible: [...streams.values()].filter(entry => entry.visible).length,
      decoded: [...streams.values()].reduce((sum, entry) => sum + entry.decoder.stats.decoded, 0)}),
    /** Streams by key with the tab each is drawn over. */
    streams: () => [...streams].map(([key, entry]) => ({key, publisher: entry.publisher, ...entry.stream, tab: placement.get(key)?.tab ?? null})),
    wake() { schedule(); },
    close() {
      if (closed) return;
      closed = true;
      stopEvery(poll);
      if (frame !== undefined) cancelFrame(frame);
      try { unwatchIndex?.(); } catch {}
      for (const key of [...streams.keys()]) removeStream(key);
      layer.remove();
    },
  };
}

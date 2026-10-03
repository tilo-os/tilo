/**
 * Other members' legacy (non-Tilo) windows on the mounted desktop. A session
 * streams each legacy window it shares with the room's host (a browser, an
 * office suite: anything that is not a Tilo app) as H.264 video; the host
 * re-publishes those streams on live record tracks in the session device's
 * namespace (`tilo_relay_protocol::legacy_window`). This layer reads every
 * member's legacy-window index, opens a desktop window for each listed
 * stream (the runtime's `desktop_set_peer_windows`), decodes the stream with
 * WebCodecs and draws it over that window's content, one canvas per stream
 * on a DOM layer above the WebGPU canvas.
 *
 * A member the host lets drive the window (it probes once per stream) sends
 * pointer, wheel, keys and pasted text on its canvas to the host, which
 * forwards them to the session; anyone else's canvas takes no input.
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
const MAX_FRAME_BYTES = 64 * 1024;
const MAX_STREAMS = 32;
const MAX_ROOM_STREAMS = 64;
const MAX_ROOM_PIXELS = 32 * 1024 * 1024;
const MAX_INPUT_BACKLOG = 128;
const MAX_INPUT_BACKLOG_BYTES = 128 * 1024;
/** The host re-sends a live index every second: a silent publisher's streams are gone. */
export const INDEX_EXPIRE_MS = 5000;
/** Decoder backlog past which frames are skipped until the next keyframe. */
const MAX_DECODE_QUEUE = 6;
/** Most events one input request carries (`legacy_window::MAX_INPUT_EVENTS`). */
const MAX_INPUT_EVENTS = 64;
/** Pasted text one event carries (`legacy_window::MAX_CLIPBOARD_BYTES`). */
const MAX_CLIPBOARD_BYTES = 64 * 1024;
/** Pixels one wheel line scrolls. */
const LINE_PIXELS = 40;
const HEX16 = /^[0-9a-f]{16}$/;
const HEX64 = /^[0-9a-f]{64}$/;
const text = new TextDecoder();

function validStream(value) {
  if (!value || typeof value !== 'object') return null;
  const {surface, app, codec, width, height, record} = value;
  if (!HEX16.test(surface) || typeof app !== 'string' || !app || app.length > 128
    || codec !== 'h264' || record !== `@tilo:legacy-window:${surface}`
    || !Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0 || width > 8192 || height > 8192) return null;
  return {surface, app, codec, width, height, record};
}

/** One legacy-window frame, or null for anything malformed. */
export function decodeFrame(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.byteLength < 2 || bytes.byteLength > MAX_FRAME_BYTES || bytes[0] !== FRAME_TAG) return null;
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
        if (bytes.byteLength <= CHUNK_HEADER || bytes[2] & ~7) return null;
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
      } else if (!pending || pending.seq !== chunk.seq || pending.tsUs !== chunk.tsUs
        || pending.data.byteLength !== chunk.total || pending.key !== Boolean(chunk.flags & CHUNK_KEY)) {
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
      if (decoder.decodeQueueSize > MAX_DECODE_QUEUE) { waitKey = true; stats.skipped++; return false; }
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

/**
 * The Linux evdev keycode of a `KeyboardEvent.code`, or null. With `mac`,
 * Command is Control, as the window's own shortcuts (copy, paste) expect.
 */
export function evdevKey(code, {mac = false} = {}) {
  if (typeof code !== 'string') return null;
  if (mac && code === 'MetaLeft') return KEYS.ControlLeft;
  if (mac && code === 'MetaRight') return KEYS.ControlRight;
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

/** A wheel event's distance in window pixels (`deltaMode` 0 pixels, 1 lines, 2 pages). */
export function wheelPixels(event, pageHeight) {
  const scale = event.deltaMode === 1 ? LINE_PIXELS : event.deltaMode === 2 ? pageHeight : 1;
  return [event.deltaX * scale, event.deltaY * scale];
}

/** What the desktop calls a stream's window: its app id's last part. */
export function windowTitle(app) {
  const name = app.split('.').filter(Boolean).at(-1) ?? app;
  return name.charAt(0).toUpperCase() + name.slice(1);
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
 * - `live()`: the room's live layer (`peer.mjs`), or undefined.
 * - `windows()`: `[{key, rect: [x0, y0, x1, y1]}]`, where the desktop shows
 *   each stream's window (`desktop_peer_windows`), or undefined.
 * - `setWindows(list)`: show exactly `[{key, app, title}]` as windows
 *   (`desktop_set_peer_windows`).
 * - `sendInput(publisher, surface, events)`: resolves whether the host took
 *   them (`peer.legacyInput`); with no events, whether this member may.
 * - `dismissed()`: the keys of windows the person closed since the last
 *   call (`desktop_dismissed_peer_windows`); each is asked to close at its
 *   source, when this member drives it.
 */
export function createLegacyWindows({
  surface,
  live,
  windows,
  setWindows,
  sendInput,
  dismissed = () => [],
  mac = /Mac/.test(globalThis.navigator?.platform ?? ''),
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
  const streams = new Map(); // `${publisher}:${surface}` -> entry
  const indexed = new Map(); // publisher -> last index time
  const stats = {indexes: 0, frames: 0, drawn: 0, placed: 0, inputSent: 0, inputRefused: 0};
  let api;
  let unwatch = [];
  let frame;
  let closed = false;
  let shown = '';

  const streamKey = (publisher, stream) => `${publisher}:${stream.surface}`;
  function schedule() {
    if (closed || frame !== undefined) return;
    frame = requestFrame(tick);
  }
  function publishWindows() {
    const list = [...streams].map(([key, entry]) => ({key, app: entry.stream.app, title: windowTitle(entry.stream.app)}));
    const next = JSON.stringify(list);
    if (next === shown) return;
    shown = next;
    try { setWindows(list); } catch (error) { report(error); }
  }
  function removeStream(key) {
    const entry = streams.get(key);
    if (!entry) return;
    releaseInput(entry);
    streams.delete(key);
    entry.control = false;
    entry.input.length = 0;
    entry.reassembler.reset();
    try { entry.unsubscribe(); } catch {}
    entry.decoder.close();
    entry.canvas.remove();
  }
  function addStream(publisher, stream) {
    const key = streamKey(publisher, stream);
    const existing = streams.get(key);
    if (existing) { existing.stream = stream; return; }
    const pixels = [...streams.values()].reduce((sum, entry) => sum + entry.stream.width * entry.stream.height, 0);
    if (streams.size >= MAX_ROOM_STREAMS || pixels + stream.width * stream.height > MAX_ROOM_PIXELS) return;
    const canvas = doc.createElement('canvas');
    canvas.className = 'tilo-legacy-window';
    canvas.dataset.app = stream.app;
    canvas.dataset.key = key;
    canvas.width = stream.width;
    canvas.height = stream.height;
    canvas.style.cssText = 'position:absolute;left:0;top:0;display:none;object-fit:contain;background:#000;transform-origin:0 0;pointer-events:none;outline:none';
    layer.append(canvas);
    const entry = {publisher, stream, canvas, context: null, reassembler: createReassembler(), visible: false, rect: '', control: false};
    entry.decoder = createStreamDecoder({
      VideoDecoder, EncodedVideoChunk,
      onFrame: picture => draw(entry, picture),
      onError: report,
    });
    entry.unsubscribe = api.subscribeMemberRecord(publisher, {app: LEGACY_APP, record: stream.record}, bytes => onStreamFrame(entry, bytes));
    bindInput(entry);
    streams.set(key, entry);
    probeControl(entry);
  }
  async function probeControl(entry) {
    try {
      const may = Boolean(await sendInput(entry.publisher, entry.stream.surface, []));
      entry.control = !closed && streams.get(streamKey(entry.publisher, entry.stream)) === entry && may;
    } catch (error) { entry.control = false; report(error); }
    schedule();
  }

  // ---- Input (a member the host lets drive the window) ----------------------
  function queueInput(entry, event) {
    if (!entry.control || closed) return;
    if (entry.input.length >= MAX_INPUT_BACKLOG || JSON.stringify(entry.input).length + JSON.stringify(event).length > MAX_INPUT_BACKLOG_BYTES) {
      entry.control = false;
      entry.input.length = 0;
      report(Error('legacy window input backlog exceeded'));
      schedule();
      return;
    }
    const latest = name => name in event && entry.input.at(-1) && name in entry.input.at(-1);
    // Latest-wins pointer motion; wheel distances add up; the rest keep their order.
    if (latest('PointerMotion')) entry.input[entry.input.length - 1] = event;
    else if (latest('PointerAxis')) {
      const last = entry.input.at(-1).PointerAxis;
      entry.input[entry.input.length - 1] = {PointerAxis: {dx: last.dx + event.PointerAxis.dx, dy: last.dy + event.PointerAxis.dy}};
    } else entry.input.push(event);
    if (!entry.flushing) void flushInput(entry);
  }
  async function flushInput(entry) {
    entry.flushing = true;
    try {
      while (entry.input.length && entry.control && !closed) {
        const events = entry.input.splice(0, MAX_INPUT_EVENTS);
        const sent = await sendInput(entry.publisher, entry.stream.surface, events);
        if (sent) stats.inputSent += events.length;
        else { stats.inputRefused += events.length; entry.control = false; schedule(); }
      }
    } catch (error) { entry.control = false; schedule(); report(error); } finally {
      entry.input.length = 0;
      entry.flushing = false;
      const release = entry.release;
      entry.release = undefined;
      if (release?.length) {
        try {
          for (let offset = 0; offset < release.length; offset += MAX_INPUT_EVENTS) {
            const events = release.slice(offset, offset + MAX_INPUT_EVENTS);
            if (await sendInput(entry.publisher, entry.stream.surface, events)) stats.inputSent += events.length;
          }
        } catch (error) { report(error); }
      }
    }
  }
  function releaseInput(entry) {
    if (!entry.focused) return;
    entry.focused = false;
    entry.release = [
      ...[...entry.keys].map(keysym => ({Key: {keysym, pressed: false, modifiers: 0}})),
      ...[...entry.buttons].map(button => ({PointerButton: {button, pressed: false}})),
      {KeyboardFocus: {surface: 0, focused: false}},
    ];
    entry.keys.clear();
    entry.buttons.clear();
    entry.pasteKey = false;
    if (!entry.flushing) void flushInput(entry);
  }
  function bindInput(entry) {
    const {canvas} = entry;
    entry.input = [];
    entry.focused = false;
    entry.pasteKey = false;
    entry.keys = new Set();
    entry.buttons = new Set();
    canvas.tabIndex = -1;
    const target = 0;
    const point = event => windowPoint(canvas.getBoundingClientRect(), canvas.width, canvas.height, event.clientX, event.clientY);
    const motion = event => {
      const at = point(event);
      if (at) queueInput(entry, {PointerMotion: {x: at[0], y: at[1]}});
      return at;
    };
    const on = (type, handler, options) => canvas.addEventListener?.(type, handler, options);
    on('pointerenter', event => { queueInput(entry, {PointerEnter: {surface: target}}); motion(event); });
    on('pointermove', motion);
    on('pointerleave', () => queueInput(entry, {PointerLeave: {surface: target}}));
    on('pointerdown', event => {
      if (!entry.control) return;
      event.preventDefault();
      canvas.focus?.({preventScroll: true});
      if (!entry.focused) { entry.focused = true; queueInput(entry, {KeyboardFocus: {surface: target, focused: true}}); }
      if (motion(event) && BUTTONS[event.button] !== undefined) {
        const button = BUTTONS[event.button];
        entry.buttons.add(button);
        canvas.setPointerCapture?.(event.pointerId);
        queueInput(entry, {PointerButton: {button, pressed: true}});
      }
    });
    on('pointerup', event => {
      if (!entry.control) return;
      event.preventDefault();
      const button = BUTTONS[event.button];
      if (button === undefined) return;
      entry.buttons.delete(button);
      if (canvas.hasPointerCapture?.(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
      queueInput(entry, {PointerButton: {button, pressed: false}});
    });
    on('pointercancel', () => releaseInput(entry));
    on('lostpointercapture', () => { if (entry.buttons.size) releaseInput(entry); });
    on('wheel', event => {
      if (!entry.control) return;
      event.preventDefault();
      const [dx, dy] = wheelPixels(event, canvas.height);
      if (dx || dy) queueInput(entry, {PointerAxis: {dx, dy}});
    }, {passive: false});
    on('contextmenu', event => { if (entry.control) event.preventDefault(); });
    const key = pressed => event => {
      const keysym = evdevKey(event.code, {mac});
      if (!entry.control || keysym === null || (pressed && event.repeat)) return;
      // Paste: the browser's clipboard reaches the window first (the
      // `paste` event), then the key, so the window pastes this text.
      if (event.code === 'KeyV' && (event.ctrlKey || event.metaKey)) {
        if (pressed) entry.pasteKey = true;
        return;
      }
      event.preventDefault();
      if (pressed) entry.keys.add(keysym); else entry.keys.delete(keysym);
      queueInput(entry, {Key: {keysym, pressed, modifiers: 0}});
    };
    on('keydown', key(true));
    on('keyup', key(false));
    on('paste', event => {
      if (!entry.control) return;
      event.preventDefault();
      const pasted = event.clipboardData?.getData?.('text/plain') ?? '';
      if (pasted && new TextEncoder().encode(pasted).byteLength <= MAX_CLIPBOARD_BYTES) queueInput(entry, {ClipboardText: {text: pasted}});
      if (entry.pasteKey) {
        entry.pasteKey = false;
        queueInput(entry, {Key: {keysym: LETTERS.V, pressed: true, modifiers: 0}});
        queueInput(entry, {Key: {keysym: LETTERS.V, pressed: false, modifiers: 0}});
      }
    });
    on('blur', () => releaseInput(entry));
  }

  // ---- Frames -----------------------------------------------------------------
  function draw(entry, picture) {
    try {
      if (streams.get(streamKey(entry.publisher, entry.stream)) !== entry) return;
      const {canvas} = entry;
      const pixels = [...streams.values()].reduce((sum, other) => sum + (other === entry ? 0 : other.canvas.width * other.canvas.height), 0);
      if (picture.displayWidth <= 0 || picture.displayHeight <= 0 || picture.displayWidth > 8192 || picture.displayHeight > 8192
        || pixels + picture.displayWidth * picture.displayHeight > MAX_ROOM_PIXELS) return;
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
    if (closed || streams.get(streamKey(entry.publisher, entry.stream)) !== entry) return;
    const value = decodeFrame(bytes);
    if (!value) return;
    stats.frames++;
    if (value.kind === 'start') {
      if (value.stream.surface !== entry.stream.surface || value.stream.record !== entry.stream.record) return;
      entry.stream = value.stream;
      entry.decoder.reset();
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
    if (closed || !HEX64.test(publisher)) return;
    const value = decodeFrame(bytes);
    if (value?.kind !== 'index') return;
    stats.indexes++;
    if (!indexed.has(publisher) && indexed.size >= MAX_ROOM_STREAMS) return;
    indexed.set(publisher, now());
    const listed = new Set();
    for (const stream of value.streams) {
      listed.add(streamKey(publisher, stream));
      addStream(publisher, stream);
    }
    for (const [key, entry] of streams) if (entry.publisher === publisher && !listed.has(key)) removeStream(key);
    publishWindows();
    schedule();
  }
  function dropPublisher(publisher) {
    indexed.delete(publisher);
    for (const [key, entry] of streams) if (entry.publisher === publisher) removeStream(key);
  }
  function expire() {
    const t = now();
    for (const [publisher, at] of indexed) if (t - at >= INDEX_EXPIRE_MS) dropPublisher(publisher);
    publishWindows();
  }
  function attach() {
    const next = live();
    if (next === api) return;
    for (const stop of unwatch) { try { stop(); } catch {} }
    unwatch = [];
    for (const publisher of [...indexed.keys()]) dropPublisher(publisher);
    for (const key of [...streams.keys()]) removeStream(key);
    api = next;
    if (api && typeof api.watchRecordFrames === 'function') {
      unwatch.push(api.watchRecordFrames(INDEX_RECORD, onIndex));
      unwatch.push(api.onMember(({publisher, active}) => { if (!active) { dropPublisher(publisher); publishWindows(); } }));
    }
    publishWindows();
  }
  function layout() {
    let placed = [];
    try { placed = windows() ?? []; } catch (error) { report(error); }
    const rects = new Map(placed.map(window => [window.key, window.rect]));
    stats.placed = 0;
    for (const [key, entry] of streams) {
      const rect = rects.get(key);
      if (!rect) {
        if (entry.visible) { releaseInput(entry); entry.canvas.blur?.(); entry.canvas.style.display = 'none'; entry.visible = false; }
        continue;
      }
      stats.placed++;
      const [x0, y0, x1, y1] = rect;
      const box = `${x0.toFixed(1)},${y0.toFixed(1)},${(x1 - x0).toFixed(1)},${(y1 - y0).toFixed(1)}`;
      if (entry.rect !== box) {
        entry.rect = box;
        entry.canvas.style.width = `${(x1 - x0).toFixed(1)}px`;
        entry.canvas.style.height = `${(y1 - y0).toFixed(1)}px`;
        entry.canvas.style.transform = `translate3d(${x0.toFixed(1)}px,${y0.toFixed(1)}px,0)`;
      }
      const events = entry.control ? 'auto' : 'none';
      if (entry.canvas.style.pointerEvents !== events) entry.canvas.style.pointerEvents = events;
      if (!entry.visible) { entry.canvas.style.display = 'block'; entry.visible = true; }
    }
  }
  /** Ask the source of each window the person closed to close it. */
  function closeDismissed() {
    let keys = [];
    try { keys = dismissed() ?? []; } catch (error) { report(error); }
    for (const key of keys) {
      const entry = streams.get(key);
      if (!entry?.control) continue;
      releaseInput(entry);
      sendInput(entry.publisher, entry.stream.surface, ['Close']).catch(report);
    }
  }
  function tick() {
    frame = undefined;
    if (closed) return;
    closeDismissed();
    layout();
    // Windows move with every presented frame (scrolls, drags, resizes).
    if (streams.size) schedule();
  }
  const poll = every(() => { attach(); expire(); closeDismissed(); schedule(); }, 1000);
  attach();

  return {
    layer,
    stats: () => ({...stats, streams: streams.size, visible: [...streams.values()].filter(entry => entry.visible).length,
      decoded: [...streams.values()].reduce((sum, entry) => sum + entry.decoder.stats.decoded, 0)}),
    /** Streams by key, with whether this member drives them and whether they are on screen. */
    streams: () => [...streams].map(([key, entry]) => ({key, publisher: entry.publisher, ...entry.stream, control: entry.control, visible: entry.visible})),
    wake() { schedule(); },
    close() {
      if (closed) return;
      closed = true;
      stopEvery(poll);
      if (frame !== undefined) cancelFrame(frame);
      for (const stop of unwatch) { try { stop(); } catch {} }
      for (const key of [...streams.keys()]) removeStream(key);
      try { setWindows([]); } catch {}
      indexed.clear();
      api = undefined;
      layer.remove();
    },
  };
}

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
  const stats = {indexes: 0, frames: 0, drawn: 0, placed: 0};
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
    streams.set(key, entry);
    layoutDirty = true;
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

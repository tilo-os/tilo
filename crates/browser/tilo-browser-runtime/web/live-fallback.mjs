/**
 * LIVE_V1 fallback carrier and carrier selection.
 *
 * The live path prefers moq-lite over WebTransport (live-transport.mjs).
 * Browsers or networks without a working WebTransport (no API, UDP blocked,
 * or a certificate WebTransport does not accept, e.g. a locally trusted page
 * that was clicked through) use the same host relay over plain HTTPS instead:
 *
 *   POST /peer/v3/live/fallback/open    body = grant bytes -> streamed server frames
 *   POST /peer/v3/live/fallback/send    x-tilo-live-session, body = client frames -> 204
 *   POST /peer/v3/live/fallback/resume  x-tilo-live-session -> streamed server frames
 *
 * Same grant, same PresenceFrame/OpsFrame bytes, same relay limits and
 * latest-value presence; the host bridges fallback and MoQ members both ways.
 * No WebSockets. Frame: type u8 | length u32 BE | payload
 * (see tilo-moq src/live/fallback.rs). The session object has exactly the
 * interface of a live-transport.mjs session.
 */
import {createLiveTransport, validateLiveEndpoint, LIVE_ROOT, MAX_DATAGRAM_FRAME_BYTES, MAX_STREAM_FRAME_BYTES} from './live-transport.mjs';

export const FALLBACK_OPEN_PATH = '/peer/v3/live/fallback/open';
export const FALLBACK_RESUME_PATH = '/peer/v3/live/fallback/resume';
export const FALLBACK_SEND_PATH = '/peer/v3/live/fallback/send';
export const FALLBACK_SESSION_HEADER = 'x-tilo-live-session';
export const FRAME = Object.freeze({
  HELLO: 0x01, MEMBER: 0x02, PRESENCE: 0x03, OPS: 0x04, OPS_END: 0x05, PING: 0x06, END: 0x07,
  SEND_PRESENCE: 0x11, SEND_OPS: 0x12, SEND_OPS_CLOSE: 0x13, SUBSCRIBE_OPS: 0x14, UNSUBSCRIBE_OPS: 0x15, SEND_CLOSE: 0x16,
});
export const END_ROTATE = 0;
export const END_REVOKED = 1;
const MAX_AUTHORIZATION_BYTES = 2048;
const MAX_SEND_BYTES = 256 * 1024;
const MAX_QUEUED_BYTES = 1024 * 1024;
const MAX_STREAM_FRAME = 16 + MAX_STREAM_FRAME_BYTES + 128;
/** Presence-only sends in flight at once (the relay drops stale sequences). */
const PRESENCE_IN_FLIGHT = 2;
/** The server pings every 15 s; a silent stream is dead. */
const STREAM_SILENCE_MS = 45000;
const RESUME_ATTEMPTS = 4;
const RESUME_BASE_MS = 150;
const MAX_REMOTE_PUBLISHERS = 64;
const HEX64 = /^[0-9a-f]{64}$/;
const OPS_TRACK = /^ops\/([0-9a-f]{64})$/;
/** WebTransport gets this long to become ready before the fallback is used. */
export const WEBTRANSPORT_BUDGET_MS = 3000;
/** After a WebTransport failure, prefer the fallback for this long. */
export const PREFER_FALLBACK_MS = 10 * 60 * 1000;

const failure = (message, code = 'unavailable') => Object.assign(Error(message), {code});
const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
function unhex(value) {
  const out = new Uint8Array(value.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(value.slice(i * 2, i * 2 + 2), 16);
  return out;
}
export function encodeFrame(kind, ...parts) {
  const length = parts.reduce((sum, part) => sum + part.byteLength, 0);
  const out = new Uint8Array(5 + length);
  out[0] = kind;
  new DataView(out.buffer).setUint32(1, length);
  let at = 5;
  for (const part of parts) { out.set(part, at); at += part.byteLength; }
  return out;
}
/** Incremental frame parser for a byte stream. */
export function createFrameReader(maxFrame = MAX_STREAM_FRAME) {
  let buffer = new Uint8Array(0);
  return {
    push(chunk) {
      const next = new Uint8Array(buffer.byteLength + chunk.byteLength);
      next.set(buffer); next.set(chunk, buffer.byteLength);
      buffer = next;
      const frames = [];
      let at = 0;
      while (buffer.byteLength - at >= 5) {
        const length = new DataView(buffer.buffer, buffer.byteOffset + at + 1, 4).getUint32(0);
        if (length > maxFrame) throw failure('live fallback frame exceeds bound');
        if (buffer.byteLength - at - 5 < length) break;
        frames.push({kind: buffer[at], payload: buffer.slice(at + 5, at + 5 + length)});
        at += 5 + length;
      }
      buffer = buffer.slice(at);
      return frames;
    },
  };
}

export function createFallbackTransport({fetch: fetcher = (...args) => globalThis.fetch(...args), clock = {}, onReport = () => {}} = {}) {
  const now = clock.now ?? (() => performance.now());
  const setTimer = clock.setTimeout ?? ((callback, delay) => globalThis.setTimeout(callback, delay));
  const clearTimer = clock.clearTimeout ?? (handle => globalThis.clearTimeout(handle));
  const report = value => { try { onReport(value); } catch {} };

  async function openStream(url, init, signal) {
    let response;
    try {
      response = await fetcher(url, {...init, method: 'POST', signal, redirect: 'error', credentials: 'omit', cache: 'no-store'});
    } catch (error) {
      if (signal?.aborted) throw signal.reason ?? error;
      throw failure(`live fallback unreachable: ${error?.message ?? error}`);
    }
    if (response.status === 404 || response.status === 426) throw failure(`live fallback HTTP ${response.status}`, response.status === 426 ? 'unsupported' : 'gone');
    if (!response.ok || !response.body?.getReader) throw failure(`live fallback HTTP ${response.status}`);
    return response.body.getReader();
  }

  async function connect({endpoint, authorization, namespace, directions, signal}) {
    signal?.throwIfAborted?.();
    const origin = validateLiveEndpoint(endpoint).origin;
    if (!Array.isArray(namespace) || namespace.length !== 3 || namespace[0] !== LIVE_ROOT || !HEX64.test(namespace[1]) || !HEX64.test(namespace[2])) {
      throw new TypeError('live namespace must be ("tilo-live", room hex, publisher hex)');
    }
    if (!(authorization instanceof Uint8Array) || authorization.byteLength === 0 || authorization.byteLength > MAX_AUTHORIZATION_BYTES) {
      throw new TypeError('live authorization must be 1..2048 grant bytes');
    }
    if (!directions || (!directions.publish && !directions.subscribe)) throw new TypeError('live directions required');
    const self = namespace[2];
    let streamAbort = new AbortController();
    const abortOnParent = () => streamAbort.abort(signal.reason);
    signal?.addEventListener?.('abort', abortOnParent, {once: true});
    let reader;
    try {
      reader = await openStream(`${origin}${FALLBACK_OPEN_PATH}`, {
        headers: {'content-type': 'application/octet-stream'}, body: new Uint8Array(authorization),
      }, streamAbort.signal);
    } catch (error) {
      signal?.removeEventListener?.('abort', abortOnParent);
      if (error?.code === 'gone') error.code = 'unsupported';
      throw error;
    }
    let frames = createFrameReader();
    const pending = [];
    // HELLO first: the session id and the granted directions.
    let hello;
    while (!hello) {
      if (pending.length === 0) {
        const {value, done} = await reader.read().catch(error => { throw failure(`live fallback stream failed: ${error?.message ?? error}`); });
        if (done) throw failure('live fallback stream ended before hello');
        pending.push(...frames.push(value));
        continue;
      }
      const first = pending.shift();
      if (first.kind !== FRAME.HELLO || first.payload.byteLength !== 17) { reader.cancel().catch(() => {}); throw failure('live fallback stream did not start with hello'); }
      hello = first.payload;
    }
    const id = hex(hello.subarray(0, 16));
    const allowed = {publish: (hello[16] & 1) === 1, subscribe: (hello[16] & 2) === 2};

    let closed = false;
    let resolveClosed;
    const closedPromise = new Promise(resolve => { resolveClosed = resolve; });
    const stats = {presenceSent: 0, opsSent: 0, presenceReceived: 0, opsReceived: 0, mode: 'https-stream', sends: 0, sendErrors: 0, resumes: 0, rotations: 0};
    const members = new Set();
    const memberWatchers = new Set();
    const presenceHandlers = new Map(); // publisher -> handler
    // Latest presence per member, delivered when a handler subscribes later
    // (an idle member publishes no further frames).
    const latestPresence = new Map(); // publisher -> {payload, at}
    const opsHandlers = new Map(); // `${publisher}/${record}` -> {onFrame, onEnd}
    // Send lanes: latest presence (unordered, <= 2 in flight) and ordered frames (1 in flight).
    let pendingPresence;
    let presenceInFlight = 0;
    const ordered = [];
    let orderedBytes = 0;
    let orderedInFlight = false;
    let silenceTimer;

    function close(reason) {
      if (closed) return;
      closed = true;
      signal?.removeEventListener?.('abort', abortOnParent);
      if (silenceTimer !== undefined) clearTimer(silenceTimer);
      streamAbort.abort(reason ?? failure('live fallback closed', 'aborted'));
      reader?.cancel().catch(() => {});
      for (const handler of opsHandlers.values()) handler.cancelled = true;
      opsHandlers.clear();
      presenceHandlers.clear();
      memberWatchers.clear();
      resolveClosed();
    }
    signal?.addEventListener?.('abort', () => close(signal.reason), {once: true});

    async function post(body) {
      stats.sends++;
      const response = await fetcher(`${origin}${FALLBACK_SEND_PATH}`, {
        method: 'POST', body, redirect: 'error', credentials: 'omit', cache: 'no-store',
        headers: {'content-type': 'application/octet-stream', [FALLBACK_SESSION_HEADER]: id},
      });
      if (response.status === 404 || response.status === 403) { close(failure('live fallback session ended')); return; }
      if (!response.ok) throw failure(`live fallback send HTTP ${response.status}`);
    }
    function pump() {
      if (closed) return;
      if (!orderedInFlight && ordered.length) {
        const parts = [];
        let size = 0;
        if (pendingPresence) { parts.push(pendingPresence); size += pendingPresence.byteLength; pendingPresence = undefined; }
        while (ordered.length && size + ordered[0].byteLength <= MAX_SEND_BYTES) {
          const frame = ordered.shift();
          orderedBytes -= frame.byteLength;
          parts.push(frame); size += frame.byteLength;
        }
        orderedInFlight = true;
        post(concat(parts, size)).catch(error => { stats.sendErrors++; report({type: 'live-fallback-send-error', error}); })
          .finally(() => { orderedInFlight = false; pump(); });
      }
      while (pendingPresence && presenceInFlight < PRESENCE_IN_FLIGHT) {
        const body = pendingPresence;
        pendingPresence = undefined;
        presenceInFlight++;
        post(body).catch(error => { stats.sendErrors++; report({type: 'live-fallback-send-error', error}); })
          .finally(() => { presenceInFlight--; pump(); });
      }
    }
    function enqueue(frame) {
      if (closed) return false;
      if (orderedBytes + frame.byteLength > MAX_QUEUED_BYTES) { report({type: 'live-fallback-queue-full'}); return false; }
      ordered.push(frame);
      orderedBytes += frame.byteLength;
      pump();
      return true;
    }

    function onMember(publisher, active) {
      if (active) {
        if (members.has(publisher)) {
          for (const watcher of memberWatchers) { try { watcher(publisher, false); } catch {} }
        } else if (members.size >= MAX_REMOTE_PUBLISHERS) { report({type: 'live-publisher-limit'}); return; }
        members.add(publisher);
        for (const watcher of memberWatchers) { try { watcher(publisher, true); } catch {} }
      } else if (members.delete(publisher)) {
        latestPresence.delete(publisher);
        for (const watcher of memberWatchers) { try { watcher(publisher, false); } catch {} }
      }
    }
    function dispatch({kind, payload}) {
      switch (kind) {
        case FRAME.MEMBER: {
          if (payload.byteLength !== 33) return;
          const publisher = hex(payload.subarray(0, 32));
          if (publisher !== self) onMember(publisher, payload[32] === 1);
          return;
        }
        case FRAME.PRESENCE: {
          const publisher = hex(payload.subarray(0, 32));
          if (!members.has(publisher)) return;
          const handler = presenceHandlers.get(publisher);
          if (!handler) { latestPresence.set(publisher, {payload: payload.subarray(32), at: now()}); return; }
          stats.presenceReceived++;
          try { handler(payload.subarray(32), now()); } catch (error) { report({type: 'live-frame-error', error}); }
          return;
        }
        case FRAME.OPS: {
          const key = `${hex(payload.subarray(0, 32))}/${hex(payload.subarray(32, 64))}`;
          const handler = opsHandlers.get(key);
          if (!handler) return;
          stats.opsReceived++;
          try { handler.onFrame(payload.subarray(64), now()); } catch (error) { report({type: 'live-frame-error', error}); }
          return;
        }
        case FRAME.OPS_END: {
          const key = `${hex(payload.subarray(0, 32))}/${hex(payload.subarray(32, 64))}`;
          const handler = opsHandlers.get(key);
          if (!handler) return;
          opsHandlers.delete(key);
          try { handler.onEnd(); } catch {}
          return;
        }
        case FRAME.END:
          if (payload[0] === END_ROTATE) { stats.rotations++; return 'resume'; }
          close(failure(payload[0] === END_REVOKED ? 'live fallback session revoked' : 'live fallback session ended'));
          return 'closed';
        default:
      }
    }
    function armSilence() {
      if (silenceTimer !== undefined) clearTimer(silenceTimer);
      silenceTimer = setTimer(() => { silenceTimer = undefined; streamAbort.abort(failure('live fallback stream silent')); }, STREAM_SILENCE_MS);
    }
    async function readLoop() {
      for (let attempt = 0; !closed;) {
        let outcome;
        try {
          armSilence();
          for (const frame of pending.splice(0)) { outcome = dispatch(frame) ?? outcome; if (outcome) break; }
          while (!outcome && !closed) {
            const {value, done} = await reader.read();
            if (done) break;
            armSilence();
            for (const frame of frames.push(value)) {
              if (frame.kind === FRAME.HELLO) { attempt = 0; continue; }
              outcome = dispatch(frame);
              if (outcome) break;
            }
          }
        } catch (error) {
          if (closed) return;
          report({type: 'live-fallback-stream-error', error});
        }
        if (closed || outcome === 'closed') return;
        // Rotation or a dropped stream: resume the same session.
        if (outcome !== 'resume') attempt++;
        if (attempt > RESUME_ATTEMPTS) { close(failure('live fallback stream lost')); return; }
        if (attempt > 0) await new Promise(resolve => setTimer(resolve, RESUME_BASE_MS * 2 ** (attempt - 1)));
        if (closed) return;
        reader?.cancel().catch(() => {});
        streamAbort = new AbortController();
        try {
          reader = await openStream(`${origin}${FALLBACK_RESUME_PATH}`, {headers: {[FALLBACK_SESSION_HEADER]: id}}, streamAbort.signal);
          stats.resumes++;
          frames = createFrameReader();
        } catch (error) {
          if (error?.code === 'gone' || error?.code === 'unsupported') { close(error); return; }
          report({type: 'live-fallback-resume-error', error});
          reader = {read: async () => ({done: true}), cancel: async () => {}};
        }
      }
    }
    void readLoop();

    function publishPresence(bytes) {
      if (closed || !allowed.publish || !directions.publish) return false;
      if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0 || bytes.byteLength > MAX_DATAGRAM_FRAME_BYTES) throw new RangeError('presence frame exceeds datagram bound');
      pendingPresence = encodeFrame(FRAME.SEND_PRESENCE, bytes);
      stats.presenceSent++;
      pump();
      return true;
    }
    function publishOps(name, bytes) {
      if (closed || !allowed.publish || !directions.publish) return false;
      const match = OPS_TRACK.exec(name);
      if (!match) throw new TypeError('invalid ops track name');
      if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0 || bytes.byteLength > MAX_STREAM_FRAME_BYTES) throw new RangeError('ops frame exceeds stream bound');
      if (!enqueue(encodeFrame(FRAME.SEND_OPS, unhex(match[1]), bytes))) return false;
      stats.opsSent++;
      return true;
    }
    function closeOps(name) {
      const match = OPS_TRACK.exec(name);
      if (match && allowed.publish) enqueue(encodeFrame(FRAME.SEND_OPS_CLOSE, unhex(match[1])));
    }
    function watchMembers({onMember: callback}) {
      if (closed) throw new Error('live session closed');
      if (!directions.subscribe || !allowed.subscribe) throw new Error('live session not authorized to subscribe');
      memberWatchers.add(callback);
      for (const publisher of members) { try { callback(publisher, true); } catch {} }
      return () => { memberWatchers.delete(callback); };
    }
    function subscribePresence(publisher, onFrame) {
      if (closed) throw new Error('live session closed');
      if (!HEX64.test(publisher) || publisher === self) throw new TypeError('invalid remote publisher');
      presenceHandlers.set(publisher, onFrame);
      const latest = latestPresence.get(publisher);
      latestPresence.delete(publisher);
      if (latest) {
        queueMicrotask(() => {
          if (closed || presenceHandlers.get(publisher) !== onFrame) return;
          stats.presenceReceived++;
          try { onFrame(latest.payload, latest.at); } catch (error) { report({type: 'live-frame-error', error}); }
        });
      }
      return () => { if (presenceHandlers.get(publisher) === onFrame) presenceHandlers.delete(publisher); };
    }
    function subscribeOps(publisher, name, onFrame, onEnd = () => {}) {
      if (closed) throw new Error('live session closed');
      const match = OPS_TRACK.exec(name);
      if (!match) throw new TypeError('invalid ops track name');
      if (!HEX64.test(publisher) || publisher === self) throw new TypeError('invalid remote publisher');
      const key = `${publisher}/${match[1]}`;
      const target = [unhex(publisher), unhex(match[1])];
      const handler = {onFrame, onEnd};
      opsHandlers.set(key, handler);
      enqueue(encodeFrame(FRAME.SUBSCRIBE_OPS, ...target));
      return () => {
        if (opsHandlers.get(key) !== handler) return;
        opsHandlers.delete(key);
        enqueue(encodeFrame(FRAME.UNSUBSCRIBE_OPS, ...target));
      };
    }

    return {
      namespace: [...namespace],
      directions: {publish: directions.publish && allowed.publish, subscribe: directions.subscribe && allowed.subscribe},
      publishPresence,
      publishOps,
      closeOps,
      watchMembers,
      subscribePresence,
      subscribeOps,
      stats: () => ({...stats, openRecords: 0, servedRecords: 0}),
      close() {
        if (closed) return;
        // Best effort: end the host session now instead of after the resume grace.
        void post(encodeFrame(FRAME.SEND_CLOSE)).catch(() => {});
        close();
      },
      closed: closedPromise,
    };
  }
  return {connect};
}

function concat(parts, size) {
  if (parts.length === 1) return parts[0];
  const out = new Uint8Array(size);
  let at = 0;
  for (const part of parts) { out.set(part, at); at += part.byteLength; }
  return out;
}

/**
 * Carrier selection: WebTransport when the browser has it and it becomes
 * ready within the budget; otherwise (or after a recent WebTransport
 * failure) the HTTPS fallback. A session never switches carriers mid-way;
 * the next reconnect re-selects.
 */
export function createLiveCarriers({
  fetch: fetcher,
  webtransport = createLiveTransport(),
  fallback = createFallbackTransport({fetch: fetcher}),
  hasWebTransport = () => typeof globalThis.WebTransport === 'function',
  budgetMs = WEBTRANSPORT_BUDGET_MS,
  clock = {},
  onReport = () => {},
} = {}) {
  const now = clock.now ?? (() => performance.now());
  const setTimer = clock.setTimeout ?? ((callback, delay) => globalThis.setTimeout(callback, delay));
  const clearTimer = clock.clearTimeout ?? (handle => globalThis.clearTimeout(handle));
  let preferFallbackUntil = -Infinity;
  let fallbackUnsupported = false;
  const state = {last: undefined, webtransportFailures: 0, fallbacks: 0};

  async function viaWebTransport(options) {
    const controller = new AbortController();
    const parent = options.signal;
    const forward = () => controller.abort(parent.reason);
    parent?.addEventListener?.('abort', forward, {once: true});
    const timer = setTimer(() => controller.abort(failure('WebTransport not ready within budget')), budgetMs);
    // Never wait on an attempt that ignores its abort signal.
    const aborted = new Promise((_, reject) => {
      if (controller.signal.aborted) reject(controller.signal.reason);
      controller.signal.addEventListener('abort', () => reject(controller.signal.reason), {once: true});
    });
    const attempt = Promise.resolve().then(() => webtransport.connect({...options, signal: controller.signal}));
    try {
      return await Promise.race([attempt, aborted]);
    } catch (error) {
      attempt.then(session => { try { session?.close?.(); } catch {} }, () => {});
      throw error;
    } finally {
      clearTimer(timer);
      parent?.removeEventListener?.('abort', forward);
    }
  }
  async function connect(options) {
    options.signal?.throwIfAborted?.();
    const useFallback = !hasWebTransport() || now() < preferFallbackUntil;
    if (!useFallback || fallbackUnsupported) {
      try {
        const session = await viaWebTransport(options);
        state.last = 'webtransport';
        return session;
      } catch (error) {
        if (options.signal?.aborted || fallbackUnsupported) throw error;
        state.webtransportFailures++;
        preferFallbackUntil = now() + PREFER_FALLBACK_MS;
        try { onReport({type: 'live-webtransport-unavailable', error}); } catch {}
      }
    }
    try {
      const session = await fallback.connect(options);
      state.last = 'https-stream';
      state.fallbacks++;
      return session;
    } catch (error) {
      if (error?.code === 'unsupported') {
        // An older host without the fallback routes: WebTransport only.
        fallbackUnsupported = true;
        preferFallbackUntil = -Infinity;
      }
      throw error;
    }
  }
  return {connect, state: () => ({...state, preferFallback: now() < preferFallbackUntil, fallbackUnsupported})};
}

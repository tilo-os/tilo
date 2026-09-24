/**
 * LIVE_V1 browser client: grant custody (Rust), MoQ live transport, presence
 * publishing/interpolation and ordered ops, exposed as a small API for the
 * renderer (lane E) and the latency harness (lane F).
 *
 * The live path is optional and ephemeral. When the host does not advertise
 * MEDIA_LIVE_V1, UDP/WebTransport is blocked or authorization is refused, the
 * client falls back silently: API calls become no-ops and the durable
 * delivery path keeps working unchanged.
 */
import {createLiveTransport} from './live-transport.mjs';
import {createPresencePublisher, createRemotePresence} from './live-presence.mjs';

export const LIVE_GRANT_TARGET = '/peer/v3/live/grant';
const MAX_GRANT_BYTES = 2048;
const MAX_PREVIEW_OPS = 32;
const OPS_RATE = 240;
const RECONNECT_BASE_MS = 250;
const RECONNECT_MAX_MS = 5000;
const RECONNECT_ATTEMPTS = 6;
const FALLBACK_RETRY_MS = 30000;
/** The shared HTTPS session may still be starting: re-check support quickly at first. */
const SUPPORT_RECHECK_MS = 2000;
const SUPPORT_QUICK_CHECKS = 5;
const REVOKED_RETRY_MS = 30000;
const RENEW_RETRY_MS = 1000;
const REQUEST_TIMEOUT_MS = 10000;
const SEEN_DIGESTS = 1024;
/** A dropped relay session (GOAWAY on revocation or restart) is re-granted at once, at most this often. */
const IMMEDIATE_REGRANT_MS = 2000;
const RESUBSCRIBE_BASE_MS = 250;
const RESUBSCRIBE_MAX_MS = 5000;

const failure = (message, code = 'unavailable') => Object.assign(Error(message), {code});

/** Canonical codec from the WASM runtime (Rust owns every encoding). */
export function runtimeCodec(runtime) {
  return {
    encodePresence: value => runtime.live_encode_presence(JSON.stringify(value)),
    decodePresence: bytes => JSON.parse(runtime.live_decode_presence(bytes)),
    encodePreview: value => runtime.live_encode_preview(JSON.stringify(value)),
    decodeOps: bytes => JSON.parse(runtime.live_decode_ops(bytes)),
    opsTrack: record => runtime.live_ops_track(record.app, record.record),
  };
}

function listeners() {
  const set = new Set();
  return {
    add(callback) {
      if (typeof callback !== 'function') throw new TypeError('listener must be a function');
      set.add(callback);
      return () => set.delete(callback);
    },
    emit(value) {
      for (const callback of [...set]) { try { callback(value); } catch {} }
    },
    get size() { return set.size; },
  };
}
const recordKey = record => `${record.app}\u0000${record.record}`;
function validRecord(record) {
  if (!record || typeof record.app !== 'string' || typeof record.record !== 'string'
    || !record.app || !record.record || record.app.length > 128 || record.record.length > 128) {
    throw new TypeError('live record must be {app, record} with 1..128 byte strings');
  }
  return {app: record.app, record: record.record};
}

export function createLiveClient({
  runtime,
  peerId,
  fetch: fetcher = globalThis.fetch,
  transport = createLiveTransport(),
  codec = runtimeCodec(runtime),
  clock = {},
  directions = {publish: true, subscribe: true},
  onError = () => {},
}) {
  const now = clock.now ?? (() => performance.now());
  const wallNow = clock.wallNow ?? (() => (globalThis.performance?.timeOrigin ?? 0) + performance.now());
  const setTimer = clock.setTimeout ?? ((callback, delay) => globalThis.setTimeout(callback, delay));
  const clearTimer = clock.clearTimeout ?? (handle => globalThis.clearTimeout(handle));
  const random = clock.random ?? Math.random;
  const report = error => { try { onError(error); } catch {} };

  const events = {
    state: listeners(), publish: listeners(), presence: listeners(), preview: listeners(),
    signedChange: listeners(), member: listeners(), durable: listeners(),
  };
  let state = 'idle';
  let closed = false;
  let session;
  let life;
  let grant;
  let renewTimer;
  let retryTimer;
  let attempts = 0;
  let supportChecks = 0;
  let lastImmediate = -Infinity;
  let connecting;
  const remotes = new Map();
  const watched = new Map(); // key -> {record, count}
  const trackNames = new Map(); // key -> ops track
  const previewSeqs = new Map(); // key -> seq
  const previewQueues = new Map(); // key -> {record, ops}
  const seen = new Set();
  const seenOrder = [];
  let tokens = OPS_RATE;
  let tokensAt = now();
  let flushTimer;
  const stats = {connects: 0, reconnects: 0, renewals: 0, revocations: 0, fallbacks: 0, previewsSent: 0, previewsDropped: 0, signedAdmitted: 0, signedDuplicate: 0, signedDeferred: 0, signedWithdrawn: 0};
  let reconcileTimer;
  let reconcileAtMs = Infinity;

  const setState = (next, detail) => {
    if (state === next) return;
    state = next;
    events.state.emit({state, ...detail});
  };

  const publisher = createPresencePublisher({
    encode: value => codec.encodePresence(value),
    send: bytes => session?.publishPresence(bytes) ?? false,
    now,
    wallNow: () => Date.now(),
    requestFrame: clock.requestFrame,
    cancelFrame: clock.cancelFrame,
    setTimer,
    clearTimer,
    onPublish: value => events.publish.emit(value),
  });

  function trackFor(record) {
    const key = recordKey(record);
    let name = trackNames.get(key);
    if (!name) { name = codec.opsTrack(record); trackNames.set(key, name); }
    return name;
  }

  // ---- Grant --------------------------------------------------------------
  async function send(prepared, signal) {
    const origin = new URL(prepared.origin);
    if (origin.protocol !== 'https:' || origin.origin !== prepared.origin || origin.username || origin.password || prepared.target !== LIVE_GRANT_TARGET) {
      throw failure('Invalid prepared live grant target');
    }
    if (!Array.isArray(prepared.body) || prepared.body.length === 0 || prepared.body.length > MAX_GRANT_BYTES || !Array.isArray(prepared.proof)) {
      throw failure('Invalid prepared live grant request');
    }
    const headers = {
      'Content-Type': 'application/octet-stream',
      'x-tilo-delivery-proof': prepared.proof.map(v => v.toString(16).padStart(2, '0')).join(''),
    };
    const response = await fetcher(`${prepared.origin}${prepared.target}`, {
      method: 'POST', headers, body: Uint8Array.from(prepared.body), signal,
      redirect: 'error', credentials: 'omit', cache: 'no-store',
    });
    signal.throwIfAborted();
    if (!response.ok) {
      // 401: proof missing (data session not ready/expired, transient);
      // 403: refused (not a member/permitted, wrong device or room, not a
      // caps-31 data session); 404: live relay not installed on this host.
      const code = response.status === 403 ? 'revoked'
        : response.status === 404 ? 'unsupported' : response.status === 401 ? 'session' : 'unavailable';
      throw failure(`Live grant HTTP ${response.status}`, code);
    }
    const bytes = new Uint8Array(await response.arrayBuffer());
    signal.throwIfAborted();
    if (bytes.byteLength === 0 || bytes.byteLength > MAX_GRANT_BYTES) throw failure('Live grant response exceeds bounds');
    return bytes;
  }
  async function requestGrant(fresh, parent) {
    const controller = new AbortController();
    const abort = () => controller.abort(parent.reason);
    parent?.addEventListener('abort', abort, {once: true});
    if (parent?.aborted) abort();
    const timer = setTimer(() => controller.abort(failure('Live grant timed out')), REQUEST_TIMEOUT_MS);
    let id;
    try {
      const prepared = JSON.parse(runtime.peer_live_grant(peerId, directions.publish === true, directions.subscribe === true, fresh));
      if (prepared?.kind !== 'request') throw failure('Invalid live grant step');
      id = prepared.id;
      const bytes = await send(prepared, controller.signal);
      const value = JSON.parse(runtime.peer_live_grant_complete(peerId, id, bytes));
      if (value?.kind !== 'grant' || !Array.isArray(value.authorization) || !Number.isFinite(value.expiresInMs) || !Number.isFinite(value.renewInMs)) {
        throw failure('Invalid verified live grant');
      }
      const started = now();
      return {...value, expiresAt: started + value.expiresInMs, renewAt: started + value.renewInMs};
    } finally {
      clearTimer(timer);
      parent?.removeEventListener('abort', abort);
      if (id !== undefined) { try { runtime.peer_live_forget(peerId, id); } catch {} }
    }
  }

  // ---- Connection lifecycle -------------------------------------------------
  function clearTimers() {
    if (renewTimer !== undefined) clearTimer(renewTimer);
    renewTimer = undefined;
    if (retryTimer !== undefined) clearTimer(retryTimer);
    retryTimer = undefined;
  }
  function teardown() {
    publisher.pause();
    life?.abort(failure('Live session ended', 'aborted'));
    life = undefined;
    for (const publisherId of [...remotes.keys()]) dropRemote(publisherId, false);
    const current = session;
    session = undefined;
    try { current?.close(); } catch {}
    if (renewTimer !== undefined) clearTimer(renewTimer);
    renewTimer = undefined;
  }
  function scheduleRetry(delay, nextState) {
    if (closed) return;
    if (retryTimer !== undefined) clearTimer(retryTimer);
    setState(nextState);
    retryTimer = setTimer(() => { retryTimer = undefined; void connect(); }, delay);
  }
  function failed(error) {
    if (closed) return;
    teardown();
    if (error?.code === 'revoked') {
      stats.revocations++;
      try { runtime.peer_live_reset(peerId); } catch {}
      scheduleRetry(REVOKED_RETRY_MS, 'revoked');
      return;
    }
    if (error?.code === 'unsupported') {
      stats.fallbacks++;
      scheduleRetry(FALLBACK_RETRY_MS, 'unavailable');
      return;
    }
    attempts++;
    if (attempts >= RECONNECT_ATTEMPTS) {
      stats.fallbacks++;
      attempts = 0;
      scheduleRetry(FALLBACK_RETRY_MS, 'unavailable');
      return;
    }
    const base = Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** (attempts - 1));
    stats.reconnects++;
    scheduleRetry(Math.round(base * (0.5 + random())), 'reconnecting');
  }
  /**
   * The relay closed the session: GOAWAY (revocation, host restart, relay
   * shutdown) or network loss. Stop publishing and re-grant immediately
   * without reporting a failure; a refused re-grant then means revocation.
   */
  function lost() {
    if (closed) return;
    teardown();
    const t = now();
    if (t - lastImmediate >= IMMEDIATE_REGRANT_MS) {
      lastImmediate = t;
      stats.regrants = (stats.regrants ?? 0) + 1;
      scheduleRetry(0, 'reconnecting');
      return;
    }
    failed(failure('Live relay connection closed'));
  }
  function scheduleRenewal() {
    if (renewTimer !== undefined) clearTimer(renewTimer);
    const delay = Math.max(0, grant.renewAt - now());
    renewTimer = setTimer(() => { renewTimer = undefined; void renew(); }, delay);
  }
  async function renew() {
    if (closed || !session || !grant) return;
    const signal = life?.signal;
    try {
      const next = await requestGrant(false, signal);
      if (closed || !session || signal?.aborted) return;
      grant = next;
      stats.renewals++;
      scheduleRenewal();
    } catch (error) {
      if (closed || signal?.aborted) return;
      if (error?.code === 'revoked') { report(error); failed(error); return; }
      if (/host restarted/.test(String(error?.message ?? error))) { lost(); return; }
      report(error);
      // Transient: retry while the current grant is still valid.
      if (grant && now() + RENEW_RETRY_MS < grant.expiresAt) {
        renewTimer = setTimer(() => { renewTimer = undefined; void renew(); }, RENEW_RETRY_MS);
      } else failed(failure('Live grant expired'));
    }
  }
  async function connect() {
    if (closed || connecting) return connecting;
    connecting = (async () => {
      let supported = false;
      try { supported = runtime.peer_live_supported(peerId) === true; } catch {}
      if (!supported) {
        supportChecks++;
        scheduleRetry(supportChecks <= SUPPORT_QUICK_CHECKS ? SUPPORT_RECHECK_MS : FALLBACK_RETRY_MS, 'unavailable');
        return false;
      }
      supportChecks = 0;
      teardown();
      life = new AbortController();
      const signal = life.signal;
      try {
        setState('authorizing');
        grant = await requestGrant(true, signal);
        signal.throwIfAborted();
        setState('connecting');
        const opened = await transport.connect({
          endpoint: grant.endpoint,
          authorization: Uint8Array.from(grant.authorization),
          namespace: grant.namespace,
          directions: grant.directions,
          signal,
        });
        if (closed || signal.aborted) { opened.close(); return false; }
        session = opened;
        stats.connects++;
        attempts = 0;
        setState('live', {publisher: grant.namespace[2], directions: {...grant.directions}});
        scheduleRenewal();
        if (grant.directions.publish) publisher.start();
        if (grant.directions.subscribe) session.watchMembers({onMember: (id, active) => (active ? addRemote(id) : dropRemote(id, true))});
        void Promise.resolve(opened.closed).then(() => {
          if (session === opened && !closed) lost();
        });
        flushPreviews();
        return true;
      } catch (error) {
        if (closed) return false;
        report(error);
        failed(error);
        return false;
      }
    })().finally(() => { connecting = undefined; });
    return connecting;
  }

  // ---- Receive ----------------------------------------------------------------
  function later(remote, callback, delay) {
    const timer = setTimer(() => { remote.timers.delete(timer); callback(); }, delay);
    remote.timers.add(timer);
  }
  function retryDelay(remote, key) {
    const failures = (remote.failures.get(key) ?? 0) + 1;
    remote.failures.set(key, failures);
    return Math.min(RESUBSCRIBE_MAX_MS, RESUBSCRIBE_BASE_MS * 2 ** (failures - 1));
  }
  function subscribePresence(remote) {
    if (!session || remotes.get(remote.id) !== remote) return;
    try {
      remote.cancel = session.subscribePresence(remote.id, (bytes, receivedAtMs) => { remote.failures.delete('presence'); onPresence(remote, bytes, receivedAtMs); }, () => {
        // Stream reset (publisher revoked) or egress withdrawn: resubscribe
        // while the member is still announced.
        remote.cancel = undefined;
        later(remote, () => subscribePresence(remote), retryDelay(remote, 'presence'));
      });
    } catch (error) { report(error); }
  }
  function addRemote(publisherId) {
    if (!session || remotes.has(publisherId) || publisherId === grant?.namespace[2]) return;
    const remote = {id: publisherId, presence: createRemotePresence(), ops: new Map(), previewSeq: new Map(), focus: null, cancel: undefined, failures: new Map(), timers: new Set()};
    remotes.set(publisherId, remote);
    subscribePresence(remote);
    syncOps(remote);
    events.member.emit({publisher: publisherId, active: true});
  }
  function dropRemote(publisherId, notify) {
    const remote = remotes.get(publisherId);
    if (!remote) return;
    remotes.delete(publisherId);
    for (const timer of remote.timers) clearTimer(timer);
    remote.timers.clear();
    try { remote.cancel?.(); } catch {}
    for (const cancel of remote.ops.values()) { try { cancel(); } catch {} }
    remote.ops.clear();
    if (notify) events.member.emit({publisher: publisherId, active: false});
  }
  function onPresence(remote, bytes, receivedAtMs) {
    const receivedWallMs = wallNow();
    let frame;
    try { frame = codec.decodePresence(bytes); } catch (error) { report(error); return; }
    if (!remote.presence.accept(frame, receivedAtMs)) return;
    const focus = frame.focus ? recordKey(frame.focus) : null;
    if (focus !== remote.focus) { remote.focus = focus; remote.focusRecord = frame.focus; syncOps(remote); }
    events.presence.emit({publisher: remote.id, frame, receivedAtMs, receivedWallMs, bytes: bytes.byteLength});
  }
  function desiredRecords(remote) {
    const records = new Map();
    for (const [key, entry] of watched) records.set(key, entry.record);
    // The record this client is focused on is open here too.
    const own = publisher.state().focus;
    if (own) records.set(recordKey(own), own);
    if (remote.focus && remote.focusRecord) records.set(remote.focus, remote.focusRecord);
    return records;
  }
  function syncOps(remote) {
    if (!session) return;
    const desired = desiredRecords(remote);
    for (const [key, cancel] of remote.ops) {
      if (!desired.has(key)) { try { cancel(); } catch {} remote.ops.delete(key); }
    }
    for (const [key, record] of desired) {
      if (remote.ops.has(key)) continue;
      try {
        remote.ops.set(key, session.subscribeOps(remote.id, trackFor(record), (bytes, receivedAtMs) => { remote.failures.delete(key); onOps(remote, bytes, receivedAtMs); }, () => {
          // Egress withdrawn or reset: resubscribe if the record is still wanted.
          remote.ops.delete(key);
          later(remote, () => { if (remotes.get(remote.id) === remote) syncOps(remote); }, retryDelay(remote, key));
        }));
      } catch (error) { report(error); }
    }
  }
  function remember(digest) {
    if (seen.has(digest)) return false;
    seen.add(digest);
    seenOrder.push(digest);
    while (seenOrder.length > SEEN_DIGESTS) seen.delete(seenOrder.shift());
    return true;
  }
  function onOps(remote, bytes, receivedAtMs) {
    const receivedWallMs = wallNow();
    let frame;
    try { frame = codec.decodeOps(bytes); } catch (error) { report(error); return; }
    if (frame.kind === 'preview') {
      const key = recordKey(frame.record);
      if (frame.seq <= (remote.previewSeq.get(key) ?? -1)) return;
      remote.previewSeq.set(key, frame.seq);
      events.preview.emit({publisher: remote.id, record: frame.record, seq: frame.seq, ops: frame.ops, receivedAtMs, receivedWallMs});
      return;
    }
    if (frame.kind !== 'signed-change' || !remember(frame.digest)) return;
    let result;
    try { result = JSON.parse(runtime.peer_live_admit(peerId, bytes)); } catch (error) { report(error); return; }
    const admittedAtMs = now();
    if (result.kind === 'admitted') stats.signedAdmitted++;
    else if (result.kind === 'duplicate') stats.signedDuplicate++;
    else stats.signedDeferred++;
    events.signedChange.emit({publisher: remote.id, record: frame.record, digest: frame.digest, result: result.kind, receivedAtMs, receivedWallMs, admittedAtMs});
    if (result.kind === 'admitted' || result.kind === 'duplicate') durableChanged(frame.record);
    if (result.speculative === true) scheduleReconcile(result.confirmByMs);
  }
  // Forwarded changes are speculative until durable replay confirms them; the
  // runtime withdraws unconfirmed ones at their deadline (LIVE_V1 lane C).
  function scheduleReconcile(atWallMs) {
    if (typeof runtime.peer_live_reconcile !== 'function' || !Number.isFinite(atWallMs) || atWallMs >= reconcileAtMs) return;
    if (reconcileTimer !== undefined) clearTimer(reconcileTimer);
    reconcileAtMs = atWallMs;
    reconcileTimer = setTimer(reconcile, Math.max(0, atWallMs - wallNow()) + 50);
  }
  function reconcile() {
    reconcileTimer = undefined;
    reconcileAtMs = Infinity;
    let result;
    try { result = JSON.parse(runtime.peer_live_reconcile(peerId)); } catch (error) { report(error); return; }
    stats.signedWithdrawn += result.withdrawn ?? 0;
    if (!closed && result.nextDeadlineMs != null) scheduleReconcile(result.nextDeadlineMs);
  }
  function durableChanged(record) {
    const value = validRecord(record);
    const key = recordKey(value);
    for (const remote of remotes.values()) {
      remote.presence.snap();
      remote.previewSeq.delete(key);
    }
    events.durable.emit({record: value});
  }

  // ---- Publish ops --------------------------------------------------------------
  function refill() {
    const t = now();
    tokens = Math.min(OPS_RATE, tokens + (t - tokensAt) * OPS_RATE / 1000);
    tokensAt = t;
  }
  function flushPreviews() {
    if (flushTimer !== undefined) { clearTimer(flushTimer); flushTimer = undefined; }
    if (!session || !grant?.directions.publish) return;
    refill();
    for (const [key, queue] of previewQueues) {
      while (queue.ops.length && tokens >= 1) {
        const ops = queue.ops.splice(0, MAX_PREVIEW_OPS);
        const seq = (previewSeqs.get(key) ?? 0) + 1;
        let bytes;
        try { bytes = codec.encodePreview({seq, record: queue.record, ops}); }
        catch (error) { stats.previewsDropped += ops.length; report(error); continue; }
        previewSeqs.set(key, seq);
        tokens -= 1;
        try {
          if (session.publishOps(trackFor(queue.record), bytes)) {
            stats.previewsSent++;
            events.publish.emit({kind: 'preview', record: queue.record, seq, ops: ops.length, publishedAtMs: now(), publishedWallMs: wallNow(), bytes: bytes.byteLength});
          }
        } catch (error) { report(error); }
      }
      if (!queue.ops.length) previewQueues.delete(key);
    }
    if (previewQueues.size) flushTimer = setTimer(flushPreviews, Math.ceil(1000 / OPS_RATE));
  }

  const api = Object.freeze({
    /** 'idle'|'authorizing'|'connecting'|'live'|'reconnecting'|'unavailable'|'revoked'|'closed' */
    get state() { return state; },
    get available() { return state === 'live'; },
    get publisherId() { return state === 'live' ? grant?.namespace[2] : undefined; },
    /** Merge local presence; coalesced to one frame per animation frame. */
    publishPresence(partial) {
      const before = publisher.state().focus;
      const changed = publisher.update(partial);
      if (changed && JSON.stringify(before) !== JSON.stringify(publisher.state().focus)) for (const remote of remotes.values()) syncOps(remote);
      return changed;
    },
    /** Queue ephemeral text preview ops for a record (ordered per record). */
    publishPreview(record, ops) {
      const value = validRecord(record);
      if (!Array.isArray(ops) || ops.length === 0) throw new TypeError('preview ops must be a non-empty array');
      if (!session || !grant?.directions.publish) return false;
      const key = recordKey(value);
      const queue = previewQueues.get(key) ?? {record: value, ops: []};
      if (queue.ops.length + ops.length > MAX_PREVIEW_OPS * 8) { stats.previewsDropped += ops.length; return false; }
      queue.ops.push(...ops);
      previewQueues.set(key, queue);
      if (flushTimer === undefined) queueMicrotask(flushPreviews);
      return true;
    },
    /** Close the record's ops track (editing finished). */
    endPreview(record) {
      const value = validRecord(record);
      previewQueues.delete(recordKey(value));
      try { session?.closeOps(trackFor(value)); } catch {}
    },
    /** Receive ops (previews, early signed changes) for a record from every member. */
    watchRecord(record) {
      const value = validRecord(record);
      const key = recordKey(value);
      const entry = watched.get(key) ?? {record: value, count: 0};
      entry.count++;
      watched.set(key, entry);
      for (const remote of remotes.values()) syncOps(remote);
      let active = true;
      return () => {
        if (!active) return;
        active = false;
        if (--entry.count === 0) watched.delete(key);
        for (const remote of remotes.values()) syncOps(remote);
      };
    },
    /** A durable change for the record landed: drop previews and snap drags. */
    durableChanged,
    /** Interpolated remote state for this frame. */
    remoteState(at = now()) {
      const out = [];
      for (const remote of remotes.values()) {
        const value = remote.presence.sample(at);
        if (value) out.push({publisher: remote.id, ...value, interpolationDelayMs: remote.presence.interpolationDelayMs()});
      }
      return out;
    },
    onState: callback => events.state.add(callback),
    onPublish: callback => events.publish.add(callback),
    onRemotePresence: callback => events.presence.add(callback),
    onPreview: callback => events.preview.add(callback),
    onSignedChange: callback => events.signedChange.add(callback),
    onMember: callback => events.member.add(callback),
    onDurable: callback => events.durable.add(callback),
    stats: () => ({...stats, state, presence: publisher.stats(), transport: session?.stats?.(), remotes: remotes.size}),
  });

  return {
    api,
    start() { if (!closed && state === 'idle') void connect(); },
    /** Reconnect now (e.g. after the shared HTTPS session was refreshed). */
    restart() {
      if (closed) return;
      clearTimers();
      attempts = 0;
      void connect();
    },
    close() {
      if (closed) return;
      closed = true;
      clearTimers();
      if (flushTimer !== undefined) clearTimer(flushTimer);
      if (reconcileTimer !== undefined) clearTimer(reconcileTimer);
      publisher.stop();
      teardown();
      try { runtime.peer_live_reset(peerId); } catch {}
      setState('closed');
    },
  };
}

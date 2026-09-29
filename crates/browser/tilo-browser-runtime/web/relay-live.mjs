/**
 * The live plane of a tilo-relay room (a core built with `--features
 * relay`), after #1432's `createLiveLayer`: presence (pointers, carets,
 * selections, drags) and typing previews, relayed by the room and never
 * durable. It exposes the `liveV1` surface `live-overlay.mjs` consumes, so a
 * relay mount draws remote carets and cursors like a cloud peer's mount.
 *
 * The carrier is WebTransport (`moq-lite`) at the relay's `/moq/live`, or
 * the relay's HTTPS stream fallback when WebTransport is missing or not
 * ready in time (`live-fallback.mjs`). A room served over loopback HTTP
 * (local review) reaches WebTransport on the relay's TLS port and falls back
 * to the loopback origin.
 */
import {createFallbackTransport, createLiveCarriers} from './live-fallback.mjs';
import {createPresencePublisher, createRemotePresence} from './live-presence.mjs';
import {createLiveTransport, LIVE_PATH, LIVE_ROOT} from './live-transport.mjs';

/** Ask for a new live grant this long before the current one expires. */
const GRANT_RENEW_MS = 60_000;
const LIVE_RETRY_MS = [250, 1000, 3000, 10_000];
/** A member's track may be announced a moment after the member: retry a
 * subscription that ended while the member is still present. */
const SUBSCRIBE_RETRY_MS = [100, 250, 1000, 3000];

const COLORS = ['#e5484d', '#30a46c', '#0090ff', '#f76b15', '#8e4ec6', '#12a594', '#d6409f', '#ffc53d'];
/** A member's colour from its device key: the same on every page. */
export function colorOf(key) {
  let hash = 0;
  for (const char of String(key)) hash = (hash * 31 + char.codePointAt(0)) >>> 0;
  return COLORS[hash % COLORS.length];
}

const recordKey = record => `${record.app}\u0000${record.record}`;
const unhex = text => Uint8Array.from(text.match(/../g) ?? [], byte => parseInt(byte, 16));

/**
 * WebTransport at `https://<host>:<webtransportPort>/moq/live` (pinned by
 * `certificateSha256` for a development certificate), else the HTTPS stream
 * fallback at `origin`.
 */
export function relayLiveCarriers(origin, info, {fetch: fetcher, webtransport, hasWebTransport} = {}) {
  const base = new URL(origin);
  const live = new URL(LIVE_PATH, `https://${base.hostname}:${info.webtransportPort}`).href;
  const fallbackEndpoint = new URL(LIVE_PATH, base).href;
  const fallback = createFallbackTransport({fetch: fetcher});
  return {
    endpoint: live,
    carriers: createLiveCarriers({
      fetch: fetcher,
      hasWebTransport,
      webtransport: webtransport ?? createLiveTransport({
        serverCertificateHashes: info.certificateSha256
          ? [{algorithm: 'sha-256', value: unhex(info.certificateSha256)}]
          : undefined,
      }),
      fallback: {connect: options => fallback.connect({...options, endpoint: fallbackEndpoint})},
    }),
  };
}

/**
 * The live layer of `room` for the relay peer `peer` (the wasm `WasmPeer`;
 * `runtime` is the wasm module with the live codec). `participant` is
 * `{label, color}`, published with this device's presence.
 */
export function createRelayLive(peer, runtime, room, {
  endpoint, carriers, participant, now = () => performance.now(), wallNow = () => Date.now(),
  setTimer = (callback, delay) => globalThis.setTimeout(callback, delay),
  clearTimer = handle => globalThis.clearTimeout(handle),
} = {}) {
  const remotes = new Map();
  const participants = new Map();
  const previews = new Map();
  const members = new Set();
  const watched = new Map();
  const subscriptions = new Map();
  const listeners = {presence: new Set(), preview: new Set(), member: new Set(), durable: new Set(), state: new Set(), publish: new Set()};
  const emit = (kind, value) => { for (const listener of [...listeners[kind]]) { try { listener(value); } catch {} } };
  const on = kind => listener => { listeners[kind].add(listener); return () => listeners[kind].delete(listener); };
  let session;
  let state = 'connecting';
  let publisherId;
  let stopped = false;
  let retry = 0;
  let renewTimer;
  const previewSeq = new Map();

  const setState = next => { if (state !== next) { state = next; emit('state', {state}); } };
  const publisher = createPresencePublisher({
    encode: value => runtime.encodeLivePresence(JSON.stringify(value)),
    send: bytes => session?.publishPresence(bytes) ?? false,
    now,
    wallNow,
    onPublish: value => emit('publish', value),
  });

  function onPresence(from, payload, receivedAtMs) {
    let decoded;
    try { decoded = JSON.parse(runtime.decodeLiveFrame(payload)); } catch { return; }
    if (decoded.kind !== 'presence') return;
    let remote = remotes.get(from);
    if (!remote) { remote = createRemotePresence(); remotes.set(from, remote); }
    if (!remote.accept(decoded.frame, receivedAtMs)) return;
    const {participant} = decoded.frame;
    const renamed = participant && participants.get(from)?.label !== participant.label;
    if (participant) participants.set(from, participant);
    if (decoded.frame.focus) watchRecord(decoded.frame.focus);
    emit('presence', {publisher: from, frame: decoded.frame, receivedAtMs, receivedWallMs: wallNow()});
    // The overlay re-reads `members()` on a member event: the name arrives
    // with presence, after the member itself.
    if (renamed) emit('member', {publisher: from, active: true});
  }
  function onOps(from, payload) {
    let decoded;
    try { decoded = JSON.parse(runtime.decodeLiveFrame(payload)); } catch { return; }
    const perRecord = previews.get(from) ?? new Map();
    previews.set(from, perRecord);
    const key = recordKey(decoded.record);
    const at = {receivedAtMs: now(), receivedWallMs: wallNow()};
    if (decoded.kind === 'preview') {
      perRecord.set(key, [...(perRecord.get(key) ?? []), ...decoded.ops]);
      emit('preview', {publisher: from, record: decoded.record, seq: decoded.seq, ops: decoded.ops, ...at});
    } else if (decoded.kind === 'previewEnd') {
      perRecord.delete(key);
      emit('preview', {publisher: from, record: decoded.record, seq: null, ops: [], ...at});
    }
  }
  // Keep `key` subscribed while its member is present: an ended
  // subscription (the track not announced yet, a relay hiccup) is retried
  // with backoff until the member leaves or the session changes.
  function keepSubscribed(from, key, open, attempt = 0) {
    const current = session;
    if (!current || subscriptions.has(key) || !members.has(from)) return;
    const cleanup = open(current, () => {
      if (subscriptions.get(key) !== cleanup) return;
      subscriptions.delete(key);
      if (stopped || session !== current || !members.has(from)) return;
      const delay = SUBSCRIBE_RETRY_MS[Math.min(attempt, SUBSCRIBE_RETRY_MS.length - 1)];
      setTimer(() => keepSubscribed(from, key, open, attempt + 1), delay);
    });
    subscriptions.set(key, cleanup);
  }
  function subscribeRecord(from, record) {
    const track = runtime.liveRecordTrack(JSON.stringify(record));
    keepSubscribed(from, `${from}\u0000${track}`,
      (current, ended) => current.subscribeOps(from, track, bytes => onOps(from, bytes), ended));
  }
  function onMember(from, active) {
    if (active) {
      members.add(from);
      keepSubscribed(from, `${from}\u0000presence`,
        (current, ended) => current.subscribePresence(from, (bytes, at) => onPresence(from, bytes, at), ended));
      for (const record of watched.values()) subscribeRecord(from, record);
    } else {
      members.delete(from);
      remotes.delete(from);
      previews.delete(from);
      for (const [key, cleanup] of subscriptions) {
        if (key.startsWith(`${from}\u0000`)) { try { cleanup(); } catch {} subscriptions.delete(key); }
      }
    }
    emit('member', {publisher: from, active});
  }
  function watchRecord(record) {
    const key = recordKey(record);
    if (watched.has(key)) return;
    watched.set(key, {app: record.app, record: record.record});
    for (const from of members) subscribeRecord(from, record);
  }

  async function connect() {
    if (stopped) return;
    setState(retry === 0 ? 'connecting' : 'reconnecting');
    try {
      const grant = JSON.parse(await peer.liveGrant(room));
      const opened = await carriers.connect({
        endpoint,
        authorization: unhex(grant.token),
        namespace: [LIVE_ROOT, grant.room, grant.publisher],
        directions: {publish: true, subscribe: true},
      });
      if (stopped) { opened.close(); return; }
      session = opened;
      publisherId = grant.publisher;
      retry = 0;
      setState('live');
      session.watchMembers({onMember});
      if (participant) publisher.update({participant: {kind: 'person', activity: null, ...participant}});
      publisher.start();
      const renewIn = Math.max(1000, grant.expiresAtNs / 1e6 - wallNow() - GRANT_RENEW_MS);
      renewTimer = setTimer(() => rotate(), renewIn);
      const ended = () => { if (session === opened) lost(); };
      Promise.resolve(session.closed).then(ended, ended);
    } catch {
      if (stopped) return;
      setState('unavailable');
      const delay = LIVE_RETRY_MS[Math.min(retry, LIVE_RETRY_MS.length - 1)];
      retry += 1;
      renewTimer = setTimer(() => connect(), delay);
    }
  }
  function dropSession() {
    publisher.pause();
    for (const cleanup of subscriptions.values()) { try { cleanup(); } catch {} }
    subscriptions.clear();
    for (const from of [...members]) onMember(from, false);
    const previous = session;
    session = undefined;
    try { previous?.close(); } catch {}
  }
  function lost() {
    if (stopped) return;
    if (renewTimer !== undefined) clearTimer(renewTimer);
    dropSession();
    retry = Math.max(retry, 1);
    setState('reconnecting');
    renewTimer = setTimer(() => connect(), LIVE_RETRY_MS[0]);
  }
  function rotate() {
    if (stopped) return;
    dropSession();
    connect();
  }
  function durableChanged(record) {
    for (const [from, perRecord] of previews) {
      perRecord.delete(recordKey(record));
      if (!perRecord.size) previews.delete(from);
    }
    for (const remote of remotes.values()) remote.snap();
    emit('durable', {record});
  }

  connect();

  /** `session.liveV1`'s surface, as `live-overlay.mjs` reads it. */
  const api = {
    get state() { return state; },
    get available() { return state === 'live'; },
    get publisherId() { return publisherId; },
    get memberCount() { return members.size; },
    publishPresence: partial => publisher.update(partial),
    publishPreview(record, ops) {
      if (!session) return false;
      const key = recordKey(record);
      const seq = (previewSeq.get(key) ?? 0) + 1;
      previewSeq.set(key, seq);
      return session.publishOps(runtime.liveRecordTrack(JSON.stringify(record)),
        runtime.encodeLivePreview(JSON.stringify(record), BigInt(seq), JSON.stringify(ops)));
    },
    endPreview(record) {
      if (!session) return;
      const track = runtime.liveRecordTrack(JSON.stringify(record));
      session.publishOps(track, runtime.encodeLivePreviewEnd(JSON.stringify(record)));
      session.closeOps(track);
      previewSeq.delete(recordKey(record));
    },
    watchRecord(record) {
      watchRecord(record);
      return () => {};
    },
    durableChanged,
    remoteState(nowMs = now()) {
      return [...remotes].map(([from, remote]) => ({
        publisher: from,
        ...remote.sample(nowMs),
        interpolationDelayMs: remote.interpolationDelayMs(),
      })).filter(value => value.seq !== undefined);
    },
    onState: on('state'),
    onPublish: on('publish'),
    onRemotePresence: on('presence'),
    onPreview: on('preview'),
    onMember: on('member'),
    onDurable: on('durable'),
    agents: () => [],
    stats: () => ({state, members: members.size, carrier: carriers.state?.().last ?? null}),
  };

  return {
    api,
    /** `[{device, name, color}]` of the members whose presence named them,
     * for the overlay's labels. */
    members: () => [...members].map(from => ({
      device: from, name: participants.get(from)?.label ?? '', color: participants.get(from)?.color,
    })),
    /** Apply the session's durable events (on every peer change). */
    pump() {
      for (const event of JSON.parse(peer.takeDurableEvents())) {
        if (event.kind === 'durable') durableChanged(event.record);
      }
    },
    stop() {
      stopped = true;
      if (renewTimer !== undefined) clearTimer(renewTimer);
      publisher.stop();
      dropSession();
      setState('closed');
    },
  };
}

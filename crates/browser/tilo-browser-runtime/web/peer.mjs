/**
 * `connectPeer` → `CertifiedPeer`: the page as a certified Tilo peer of the
 * user's host, reached at its HTTPS origin (PR 541's carriers, served by the
 * host itself):
 *
 * - durable documents over HTTPS: signed commits from a persisted outbox,
 *   acknowledged in host order, received by long-poll from a host cursor,
 *   replayed after every reconnect (`tilo-browser-peer` in wasm);
 * - device certification over HTTPS: the page shows the pairing code while
 *   a root-holding device of the user approves;
 * - the live plane (`peer.live`) over WebTransport `moq-lite`
 *   (`live-transport.mjs`), or its HTTPS stream fallback (`live-fallback.mjs`),
 *   admitted by a host grant for this session. Never authority;
 * - the room's calls (`peer.voice`, `voice-peer.mjs`): the session holds the
 *   participation, the page's voice carrier follows its grants.
 *
 * The page holds one device key (`device-custody.mjs`). A page that also
 * holds the user root (`root-identity.mjs`, `options.identity`) certifies
 * that key itself instead of waiting for an approver.
 * `getSnapshot()` returns `PeerSnapshot` with `SessionHealth` v1.
 */
import {createActions} from './actions.mjs';
import {createAgents} from './agents.mjs';
import {openChannelVault} from './channel-vault.mjs';
import {createPresencePublisher, createRemotePresence} from './live-presence.mjs';
import {createFallbackTransport, createLiveCarriers} from './live-fallback.mjs';
import {createSpeakers} from './live-speakers.mjs';
import {createLiveTransport, LIVE_PATH, LIVE_ROOT} from './live-transport.mjs';
import {createPeerVoice} from './voice-peer.mjs';
import {WEBKIT_WEBGPU_REFUSAL} from './webgpu-safety.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';
import {currentIdentity, IdentityError} from './root-identity.mjs';

export {
  addPasskey, certifyDevice, createIdentity, currentIdentity, IdentityError, installNodeCertificate, unlockIdentity,
} from './root-identity.mjs';

const CERTIFICATE = 'certificate';
/** A member's cursor colour, from its device key: the same on every page. */
const COLORS = ['#e5484d', '#30a46c', '#0090ff', '#f76b15', '#8e4ec6', '#12a594', '#d6409f', '#ffc53d'];
export function colorOf(publisher) {
  let hash = 0;
  for (const char of String(publisher)) hash = (hash * 31 + char.codePointAt(0)) >>> 0;
  return COLORS[hash % COLORS.length];
}
const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * `{get, put, delete}` over values kept in an open profile's durable store
 * (`openProfileStorage`), committed to IndexedDB with the profile's other
 * writes. Strings round-trip as strings.
 */
export function profileStore(runtime, profile, device) {
  return {
    async get(name) {
      const bytes = runtime.profile_value(profile, device, name);
      return bytes === undefined ? undefined : decoder.decode(bytes);
    },
    async put(name, value) {
      runtime.set_profile_value(profile, device, name, typeof value === 'string' ? encoder.encode(value) : value);
    },
    async delete(name) { runtime.set_profile_value(profile, device, name, undefined); },
  };
}
const PEER_RECORD = 'peer-record';
/** The profile an identity keeps for one host: its device key, certificate
 * and documents there. */
export const identityProfile = (identity, origin) => `tilo-identity:${identity.userPublicKey}@${new URL(origin).host}`;
/** The wasm runtime, loaded through the page host. */
const pageRuntime = async desktop => (desktop ?? await import('./host.mjs')).loadRuntime?.();
/** Whether a stored `{cert}` expires within a day (a page holding the root
 * then certifies itself anew). */
function expiresWithinADay(stored) {
  const expires = JSON.parse(stored)?.cert?.expires_at_ns;
  return typeof expires === 'number' && expires / 1e6 < Date.now() + 86_400_000;
}
/** Ask for a new live grant this long before the current one expires. */
const GRANT_RENEW_MS = 60_000;
const LIVE_RETRY_MS = [250, 1000, 3000, 10_000];
/** A member's track may be announced a moment after the member: retry a
 * subscription that ended while the member is still present. */
const SUBSCRIBE_RETRY_MS = [100, 250, 1000, 3000];

/** How long a recovery waits for its evidence: a newly presented frame, the
 * host's shared state. */
const RECOVERY_TIMEOUT_MS = 10_000;
const REFRESH_TIMEOUT_MS = 30_000;

/** The `FailureCode` of a mount's renderer error. */
export function rendererFailureCode(error) {
  const text = String(error ?? '');
  if (/device lost/i.test(text)) return 'gpu-device-lost';
  if (/WebGPU (unavailable|is required)/i.test(text)) return 'gpu-unavailable';
  if (text.includes(WEBKIT_WEBGPU_REFUSAL)) return 'gpu-unavailable';
  return 'renderer-initialization-failed';
}
/** The `FailureCode` of a failed durable commit. */
export function storageFailureCode(error) {
  const text = String(error ?? '');
  if (/QuotaExceeded/i.test(text)) return 'storage-quota';
  if (/SecurityError/i.test(text)) return 'storage-denied';
  return 'storage-transaction-failed';
}
/** The failed or recovered asset a mount status names, as
 * `{assetId, failed, message}`: images by their loader id, fonts as
 * `font:NAME`, application data as `data:NAME`. */
export function statusAsset(status) {
  if (status.asset === 'unavailable' || status.asset === 'ready') {
    return {assetId: String(status.assetId), failed: status.asset === 'unavailable', message: 'An image could not be loaded.'};
  }
  if (status.font === 'unavailable' || status.font === 'available') {
    return {assetId: `font:${status.fontAsset}`, failed: status.font === 'unavailable', message: 'A font could not be loaded.'};
  }
  if (status.data === 'unavailable' || status.data === 'available') {
    return {assetId: `data:${status.dataAsset}`, failed: status.data === 'unavailable', message: 'Application data could not be loaded.'};
  }
  return undefined;
}

const recordKey = record => `${record.app}\u0000${record.record}`;
const unhex = text => Uint8Array.from(text.match(/../g) ?? [], byte => parseInt(byte, 16));
const hexKey = key => (typeof key === 'string' ? key : Array.from(key ?? [], byte => byte.toString(16).padStart(2, '0')).join(''));

const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);
/** A host served over plain HTTP on this machine (`tilo-relay room --web-plain`). */
const isLoopbackHttp = url => url.protocol === 'http:' && LOOPBACK.has(url.hostname);

/** `post(path, session, body)` over `fetch` to the host origin: https, or
 * loopback http for local review (no certificate exception needed). */
export function httpsPost(origin, fetcher = (...args) => globalThis.fetch(...args)) {
  const base = new URL(origin);
  const secure = base.protocol === 'https:' || isLoopbackHttp(base);
  if (!secure || base.pathname !== '/' || base.search || base.hash) {
    throw new TypeError('host origin must be an https (or loopback http) origin');
  }
  return async (path, session, body) => {
    const headers = {'content-type': 'application/octet-stream'};
    if (session) headers['x-tilo-session'] = session;
    const response = await fetcher(new URL(path, base).href, {
      method: 'POST', headers, body, credentials: 'omit', cache: 'no-store', redirect: 'error',
    });
    return {status: response.status, body: new Uint8Array(await response.arrayBuffer())};
  };
}

/**
 * Where a room's live plane is reached from `origin`: its `/moq/live`, both
 * for WebTransport and the HTTPS stream fallback. A room on a loopback
 * `http:` origin (local review) serves WebTransport only on its TLS port
 * (`host.webtransportPort`, pinned by `certificateSha256`); its fallback
 * stays on the plain origin.
 */
export function liveEndpoints(origin, host = {}) {
  const base = new URL(origin);
  const fallback = new URL(LIVE_PATH, base).href;
  if (!isLoopbackHttp(base) || !host.webtransportPort) return {endpoint: fallback, fallback};
  return {endpoint: new URL(LIVE_PATH, `https://${base.hostname}:${host.webtransportPort}`).href, fallback};
}

/**
 * The live layer of one room — the interface the desktop mount consumes.
 * `peer` is the wasm `WasmPeer`, `runtime` the wasm module (live codec).
 * `participant` (`{label, color}`) is published with this device's
 * presence. With `agent`, the layer publishes as that agent of this device,
 * under its own namespace (`tilo_relay_protocol::web::agent_publisher`),
 * with an agent participant. The grant is asked for once `ready` resolves:
 * the room refuses it until it admitted the device.
 */
export function createLiveLayer(peer, runtime, room, {
  origin, endpoint = new URL(LIVE_PATH, origin).href, carriers, participant, agent, ready = Promise.resolve(),
  now = () => performance.now(), wallNow = () => Date.now(),
  setTimer = (callback, delay) => globalThis.setTimeout(callback, delay),
  clearTimer = handle => globalThis.clearTimeout(handle),
} = {}) {
  const remotes = new Map();
  const participants = new Map();
  const previews = new Map();
  const members = new Set();
  const watched = new Map(); // record key -> record
  const subscriptions = new Map(); // `${publisher}\u0000${track}` -> cleanup
  const listeners = {presence: new Set(), preview: new Set(), member: new Set(), durable: new Set(), state: new Set(), publish: new Set(), agentActivity: new Set()};
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
  // A live frame leaves only sealed with the channel keys of the record it
  // names; one this device cannot seal is dropped, never sent in the clear.
  // Sealing and opening run in the session's order, so frames keep theirs.
  const sealed = bytes => peer.sealLive(room, bytes).then(frame => frame ?? null, () => null);
  const opened = (payload, handle) => {
    peer.openLive(room, payload).then(frame => { if (frame) handle(frame); }, () => {});
  };
  const publishSealed = (bytes, publish) => {
    if (!session) return false;
    const current = session;
    void sealed(bytes).then(frame => { if (frame && session === current) publish(current, frame); });
    return true;
  };
  const publisher = createPresencePublisher({
    encode: value => runtime.encodeLivePresence(JSON.stringify(value)),
    send: bytes => publishSealed(bytes, (current, frame) => current.publishPresence(frame)),
    now,
    wallNow,
    onPublish: value => emit('publish', value),
  });

  function onPresence(from, payload, receivedAtMs) {
    opened(payload, frame => presenceOpened(from, frame, receivedAtMs));
  }
  function presenceOpened(from, payload, receivedAtMs) {
    let decoded;
    try { decoded = JSON.parse(runtime.decodeLiveFrame(payload)); } catch { return; }
    if (decoded.kind !== 'presence') return;
    let remote = remotes.get(from);
    if (!remote) { remote = createRemotePresence(); remotes.set(from, remote); }
    if (!remote.accept(decoded.frame, receivedAtMs)) return;
    const {participant: named} = decoded.frame;
    const renamed = named && participants.get(from)?.label !== named.label;
    if (named) participants.set(from, named);
    if (decoded.frame.focus) watchRecord(decoded.frame.focus);
    emit('presence', {publisher: from, frame: decoded.frame, receivedAtMs});
    // A member's name arrives with its presence, after the member itself.
    if (renamed) emit('member', {publisher: from, active: true});
    if (renamed && named.kind === 'agent') {
      emit('agentActivity', {publisher: from, agentId: named.label, name: named.label, expired: false});
    }
  }
  function onOps(publisherId, payload) {
    opened(payload, frame => opsOpened(publisherId, frame));
  }
  function opsOpened(publisherId, payload) {
    let decoded;
    try { decoded = JSON.parse(runtime.decodeLiveFrame(payload)); } catch { return; }
    const perRecord = previews.get(publisherId) ?? new Map();
    previews.set(publisherId, perRecord);
    const key = recordKey(decoded.record);
    if (decoded.kind === 'preview') {
      perRecord.set(key, [...(perRecord.get(key) ?? []), ...decoded.ops]);
      emit('preview', {publisher: publisherId, record: decoded.record, seq: decoded.seq, ops: decoded.ops});
    } else if (decoded.kind === 'previewEnd') {
      perRecord.delete(key);
      emit('preview', {publisher: publisherId, record: decoded.record, seq: null, ops: []});
    }
  }
  // Keep `key` subscribed while its member is present: an ended
  // subscription (the track not announced yet, a relay hiccup) is retried
  // with backoff until the member leaves or the session changes.
  function keepSubscribed(publisherId, key, open, attempt = 0, wanted = () => true) {
    const current = session;
    if (stopped || !current || subscriptions.has(key) || !members.has(publisherId) || !wanted()) return;
    const cleanup = open(current, () => {
      if (subscriptions.get(key) !== cleanup) return;
      subscriptions.delete(key);
      if (stopped || session !== current || !members.has(publisherId)) return;
      const delay = SUBSCRIBE_RETRY_MS[Math.min(attempt, SUBSCRIBE_RETRY_MS.length - 1)];
      setTimer(() => { if (session === current) keepSubscribed(publisherId, key, open, attempt + 1, wanted); }, delay);
    });
    subscriptions.set(key, cleanup);
  }
  function subscribeRecord(publisherId, record) {
    const track = runtime.liveRecordTrack(JSON.stringify(record));
    keepSubscribed(publisherId, `${publisherId}\u0000${track}`,
      (current, ended) => current.subscribeOps(publisherId, track, payload => onOps(publisherId, payload), ended));
  }
  function onMember(publisherId, active) {
    if (active) {
      members.add(publisherId);
      keepSubscribed(publisherId, `${publisherId}\u0000presence`,
        (current, ended) => current.subscribePresence(publisherId, (payload, at) => onPresence(publisherId, payload, at), ended));
      for (const record of watched.values()) subscribeRecord(publisherId, record);
      for (const track of rawWatched.keys()) subscribeFrames(publisherId, track);
      for (const key of memberFrames.keys()) {
        const [member, track] = key.split('\u0000');
        if (member === publisherId) subscribeFrames(publisherId, track);
      }
    } else {
      members.delete(publisherId);
      remotes.delete(publisherId);
      previews.delete(publisherId);
      for (const [key, cleanup] of subscriptions) {
        if (key.startsWith(`${publisherId}\u0000`)) { try { cleanup(); } catch {} subscriptions.delete(key); }
      }
    }
    emit('member', {publisher: publisherId, active});
  }
  function watchRecord(record) {
    const key = recordKey(record);
    if (watched.has(key)) return;
    watched.set(key, {app: record.app, record: record.record});
    for (const publisherId of members) subscribeRecord(publisherId, record);
  }
  // Raw frames of a record (not live codec frames: the host's own tracks,
  // such as legacy windows), from every member or from one.
  const rawWatched = new Map(); // track -> Set of onFrame(publisher, bytes)
  const memberFrames = new Map(); // `${publisher}\u0000${track}` -> Set of onFrame(bytes)
  function deliverFrames(publisherId, track, payload) {
    for (const onFrame of [...(rawWatched.get(track) ?? [])]) { try { onFrame(publisherId, payload); } catch {} }
    for (const onFrame of [...(memberFrames.get(`${publisherId}\u0000${track}`) ?? [])]) { try { onFrame(payload); } catch {} }
  }
  function subscribeFrames(publisherId, track) {
    keepSubscribed(publisherId, `${publisherId}\u0000raw\u0000${track}`,
      (current, ended) => current.subscribeOps(publisherId, track, payload => deliverFrames(publisherId, track, payload), ended),
      0, () => Boolean(rawWatched.get(track)?.size || memberFrames.get(`${publisherId}\u0000${track}`)?.size));
  }
  function releaseFrames(publisherId, track) {
    if (rawWatched.get(track)?.size || memberFrames.get(`${publisherId}\u0000${track}`)?.size) return;
    const key = `${publisherId}\u0000raw\u0000${track}`;
    const cleanup = subscriptions.get(key);
    subscriptions.delete(key);
    try { cleanup?.(); } catch {}
  }

  async function connect() {
    if (stopped) return;
    setState(retry === 0 ? 'connecting' : 'reconnecting');
    try {
      const grant = JSON.parse(agent === undefined ? await peer.liveGrant(room) : await peer.liveGrant(room, agent));
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
      if (participant) publisher.update({participant: {kind: agent === undefined ? 'person' : 'agent', activity: null, ...participant}});
      publisher.start();
      const renewIn = Math.max(1000, grant.expiresAtNs / 1e6 - wallNow() - GRANT_RENEW_MS);
      renewTimer = setTimer(() => rotate(), renewIn);
      Promise.resolve(session.closed).then(() => { if (session === opened) lost(); }, () => { if (session === opened) lost(); });
    } catch (error) {
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
    for (const publisherId of [...members]) onMember(publisherId, false);
    const previous = session;
    session = undefined;
    try { previous?.close(); } catch {}
  }
  function lost() {
    if (stopped) return;
    if (renewTimer !== undefined) clearTimer(renewTimer);
    dropSession();
    retry = Math.max(retry, 1);
    renewTimer = setTimer(() => connect(), LIVE_RETRY_MS[0]);
  }
  function rotate() {
    if (stopped) return;
    dropSession();
    connect();
  }

  function durableChanged(record) {
    for (const [publisherId, perRecord] of previews) {
      perRecord.delete(recordKey(record));
      if (!perRecord.size) previews.delete(publisherId);
    }
    for (const remote of remotes.values()) remote.snap();
    emit('durable', {record});
  }

  ready.then(() => connect(), () => { if (!stopped) setState('unavailable'); });

  const isAgent = from => participants.get(from)?.kind === 'agent';

  return {
    room,
    get state() { return state; },
    get available() { return state === 'live'; },
    /** This layer's publisher in the room (the device, or its agent), once live. */
    get publisherId() { return publisherId; },
    get memberCount() { return members.size; },
    /** The agent this layer publishes as, or `undefined` for the device. */
    agent,
    /** Merge presence fields (`null` clears): focus, pointer, selection, drag. */
    publishPresence: partial => publisher.update(partial),
    /** Ephemeral text preview operations for a record. */
    publishPreview(record, ops) {
      if (!session) return false;
      const key = recordKey(record);
      const seq = (previewSeq.get(key) ?? 0) + 1;
      previewSeq.set(key, seq);
      const track = runtime.liveRecordTrack(JSON.stringify(record));
      return publishSealed(runtime.encodeLivePreview(JSON.stringify(record), BigInt(seq), JSON.stringify(ops)),
        (current, frame) => current.publishOps(track, frame));
    },
    endPreview(record) {
      if (!session) return;
      const track = runtime.liveRecordTrack(JSON.stringify(record));
      const current = session;
      void sealed(runtime.encodeLivePreviewEnd(JSON.stringify(record))).then(frame => {
        if (session !== current) return;
        if (frame) current.publishOps(track, frame);
        current.closeOps(track);
      });
      previewSeq.delete(recordKey(record));
    },
    /** Receive previews for a record from every member (also once a
     * member's presence focuses it). */
    watchRecord(record) {
      watchRecord(record);
      return () => watched.delete(recordKey(record));
    },
    /** Raw frames of `record` from every member, now and joining:
     * `onFrame(publisher, bytes)`. For the host's own record tracks (legacy
     * windows), which are not live codec frames. Returns a stop function. */
    watchRecordFrames(record, onFrame) {
      const track = runtime.liveRecordTrack(JSON.stringify(record));
      const handlers = rawWatched.get(track) ?? new Set();
      rawWatched.set(track, handlers);
      handlers.add(onFrame);
      for (const publisherId of members) subscribeFrames(publisherId, track);
      return () => {
        handlers.delete(onFrame);
        if (!handlers.size) rawWatched.delete(track);
        for (const publisherId of members) releaseFrames(publisherId, track);
      };
    },
    /** Raw frames of `record` from the member `publisher` only, while it is
     * present: `onFrame(bytes)`. Returns a stop function. */
    subscribeMemberRecord(publisher, record, onFrame) {
      const track = runtime.liveRecordTrack(JSON.stringify(record));
      const key = `${publisher}\u0000${track}`;
      const handlers = memberFrames.get(key) ?? new Set();
      memberFrames.set(key, handlers);
      handlers.add(onFrame);
      if (members.has(publisher)) subscribeFrames(publisher, track);
      return () => {
        handlers.delete(onFrame);
        if (!handlers.size) memberFrames.delete(key);
        releaseFrames(publisher, track);
      };
    },
    /** A durable change for the record landed: drop previews and snap drags. */
    durableChanged,
    /** Per-frame interpolated remote state on the `performance.now()` timeline. */
    remoteState(nowMs = now()) {
      return [...remotes].map(([publisherId, remote]) => ({
        publisher: publisherId,
        ...remote.sample(nowMs),
        previews: Object.fromEntries(previews.get(publisherId) ?? []),
        interpolationDelayMs: remote.interpolationDelayMs(),
      })).filter(value => value.seq !== undefined);
    },
    members: () => [...members],
    /** `[{device, name, color}]` of the people whose presence named them;
     * agents are in `agents()`. */
    people: () => [...members].filter(from => !isAgent(from)).map(from => ({
      device: from, name: participants.get(from)?.label ?? '', color: participants.get(from)?.color,
    })),
    /** Agents present in the room (their presence names them as agents). */
    agents: () => [...members].filter(isAgent).map(from => ({
      publisher: from, agentId: participants.get(from).label, name: participants.get(from).label,
    })),
    onPresence: on('presence'),
    /** `presence` under the name `live-overlay` consumers use. */
    onRemotePresence: on('presence'),
    onPreview: on('preview'),
    onMember: on('member'),
    onDurable: on('durable'),
    onState: on('state'),
    /** Each presence frame this layer sent. */
    onPublish: on('publish'),
    /** An agent member named itself (`{publisher, agentId, name, expired}`). */
    onAgentActivity: on('agentActivity'),
    stats: () => ({state, members: members.size, agent: agent ?? null, carrier: carriers.state?.().last ?? null}),
    /** Apply durable events from the session (called on every peer change);
     * pass `events` when several layers share one peer. */
    pump(events = JSON.parse(peer.takeDurableEvents())) {
      for (const event of events) {
        if (event.kind === 'durable') durableChanged(event.record);
      }
    },
    stop() {
      stopped = true;
      if (renewTimer !== undefined) clearTimer(renewTimer);
      publisher.stop();
      dropSession();
      rawWatched.clear();
      memberFrames.clear();
      setState('closed');
    },
  };
}

/**
 * Connect this page as a certified peer.
 *
 * @param options.origin         host HTTPS origin (`https://host:port`)
 * @param options.userPublicKey  the user's root public key (hex)
 * @param options.label          device label shown to the approver
 * @param options.room           room uuid to join, optional
 * @param options.custody        from `openDeviceCustody`
 * @param options.profile        profile whose durable storage keeps the peer's
 *                               certificates and outbox (with `device`); its
 *                               desktop mounts share that storage
 * @param options.device         device namespace of `profile`
 * @param options.store          `{get, put, delete}` for public peer state;
 *                               defaults to the profile's storage
 * @param options.vaultStorage   `{get, put}` for the profile's wrapped channel
 *                               vault (`channel-vault.mjs`); defaults to the
 *                               device custody's IndexedDB
 * @param options.displayName    the person's name for history and presence
 * @param options.runtime        the wasm module (`certifyDevice`, `WasmPeer`, live codec)
 * @param options.fetch          defaults to `globalThis.fetch`
 * @param options.liveCarriers   defaults to WebTransport with HTTPS fallback
 * @param options.desktop        `{mountDesktop(target, options)}`; defaults to `host.mjs`
 * @param options.live           `false`: join no live plane
 * @param options.onPairingCode  shown while an approver decides
 * @param options.identity       an unlocked root (`root-identity.mjs`): the
 *                               page certifies its own device with it, no
 *                               approver; `userPublicKey` defaults to its key,
 *                               `profile` to one per identity, `custody` to
 *                               that profile's device key and `runtime` to
 *                               the page host's
 * @param options.deviceCertificate  `{cert, hostCert}` JSON this device was
 *                               certified with elsewhere (`joinWorkspace`)
 */
export async function connectPeer(options) {
  const {identity, custody, origin, store, profile} = options;
  if (!identity || custody) return connect(options);
  // The device key this call opens is the peer's: it is let go when the
  // peer is disposed or fails to connect, so the page can connect again.
  const own = await openDeviceCustody({profile: profile ?? identityProfile(identity, origin), storage: idbStorage()});
  return withCustody(own, () => connect({...options, custody: own, ...(store ? {} : {profile: profile ?? identityProfile(identity, origin)})}),
    options.signal);
}

/** Runs `connecting` with the device key `custody`, closing it when the
 * resulting peer is disposed, when `signal` aborts, or when connecting fails. */
export async function withCustody(custody, connecting, signal) {
  let peer;
  try {
    peer = await connecting();
  } catch (error) {
    custody.close();
    throw error;
  }
  const dispose = peer.dispose;
  peer.dispose = async () => {
    try { await dispose(); } finally { custody.close(); }
  };
  signal?.addEventListener('abort', () => custody.close(), {once: true});
  return peer;
}

async function connect({
  origin, userPublicKey, label = 'Browser', room, custody, store, runtime, identity, deviceCertificate,
  profile, device = 'tilo-browser-device', displayName = '', vaultStorage,
  fetch: fetcher, liveCarriers, desktop, signal, onStatus, onPairingCode, live: liveEnabled = true,
  voice: voiceOptions = {},
}) {
  userPublicKey ??= identity?.userPublicKey;
  if (identity && userPublicKey !== identity.userPublicKey) {
    throw new TypeError('userPublicKey must be the unlocked identity\'s');
  }
  if (identity) {
    if (!store) profile ??= identityProfile(identity, origin);
    custody ??= await openDeviceCustody({profile: profile ?? identityProfile(identity, origin), storage: idbStorage()});
    runtime ??= await pageRuntime(desktop);
  }
  if (!origin || !userPublicKey || !custody || !(store || profile) || !runtime) {
    throw new TypeError('origin, userPublicKey, custody, store or profile, and runtime are required');
  }
  signal?.throwIfAborted();
  const surfaceModule = async () => desktop ?? await import('./host.mjs');
  let storage;
  if (profile) {
    const surface = await surfaceModule();
    if (typeof surface.openProfileStorage !== 'function') throw new Error('profile storage is unavailable');
    storage = await surface.openProfileStorage(profile, device);
    store ??= profileStore(runtime, profile, device);
  }
  // Seals the channel state the runtime keeps in the profile's storage.
  const channelVault = storage
    ? await openChannelVault({profile: `${profile}/${device}`, userPublicKey, storage: vaultStorage ?? idbStorage()})
    : undefined;
  const post = httpsPost(origin, fetcher);
  const custodyForWasm = custody.kind === 'sealed-seed' ? await custody.unseal() : custody;
  const custodyKeyHex = () => [...(custodyForWasm.publicKey ?? [])].map(b => b.toString(16).padStart(2, '0')).join('');
  const carriersFor = info => {
    if (liveCarriers) return liveCarriers;
    const fallbackEndpoint = liveEndpoints(origin, info).fallback;
    const fallback = createFallbackTransport({fetch: fetcher});
    return createLiveCarriers({
      fetch: fetcher,
      webtransport: createLiveTransport({
        serverCertificateHashes: info.certificateSha256
          ? [{algorithm: 'sha-256', value: unhex(info.certificateSha256)}]
          : undefined,
      }),
      fallback: {connect: options => fallback.connect({...options, endpoint: fallbackEndpoint})},
    });
  };
  let host = JSON.parse(await runtime.hostInfo(post));
  let carriers = carriersFor(host);
  const listeners = new Set();
  let peer;
  let live;
  // Agents this page runs publish under their own namespaces: one layer each.
  const agentLayers = new Map();
  const layers = () => [...(live ? [live] : []), ...agentLayers.values()];
  let admitted;
  let ready;
  const resetReady = () => { ready = new Promise(resolve => { admitted = resolve; }); };
  resetReady();
  let agents;
  let actions;
  let voice;
  let disposed = false;
  let snapshot = {phase: 'authenticating', records: []};
  // The session's remote-action events are taken once per change and read
  // by every consumer: agents' and the room's actions share the path.
  let remoteEvents = '[]';
  const remote = {
    sendRemoteAction: (...args) => peer.sendRemoteAction(...args),
    takeRemoteActionEvents: () => remoteEvents,
  };
  // Desktop surfaces mounted through this peer, by surface id: their failures
  // are reported into SessionHealth and recovered through `recover`.
  const surfaces = new Map();
  let surfaceCount = 0;

  const publish = () => {
    if (!peer) return;
    snapshot = JSON.parse(peer.snapshot());
    if (snapshot.phase === 'ready') admitted();
    if (live) {
      const events = JSON.parse(peer.takeDurableEvents());
      for (const layer of layers()) layer.pump(events);
    }
    voice?.pump(peer.takeVoiceEvents?.() ?? '[]');
    remoteEvents = peer.takeRemoteActionEvents();
    agents?.pump();
    actions?.pump();
    remoteEvents = '[]';
    for (const listener of [...listeners]) listener(snapshot);
    onStatus?.(snapshot);
  };
  const untilSnapshot = (settled, timeoutMs, timeoutMessage) => new Promise((resolve, reject) => {
    const check = () => {
      let outcome;
      try { outcome = settled(snapshot); } catch (error) { finish(error); return; }
      if (outcome) finish();
    };
    const timer = setTimeout(() => finish(new Error(timeoutMessage)), timeoutMs);
    function finish(error) {
      clearTimeout(timer);
      listeners.delete(check);
      if (error) reject(error); else resolve();
    }
    listeners.add(check);
    check();
  });

  const reportStorage = error => {
    try { peer?.reportFailure(undefined, undefined, storageFailureCode(error), 'Changes could not be saved on this device.'); } catch {}
  };
  const unsubscribeStorage = storage?.subscribe?.(status => { if (status.persistence === 'failed') reportStorage(status.error); });
  function observeSurface(entry, status) {
    if (status.phase === 'closed') { dropSurface(entry); return; }
    if (Number.isSafeInteger(status.gpuGeneration)) entry.generation = status.gpuGeneration;
    if (status.persistence === 'failed') reportStorage(status.error);
    if (status.phase === 'presented') entry.presented = true;
    // A mount that never presented rejects its mount call instead.
    if (!entry.presented || !peer) return;
    const asset = statusAsset(status);
    if (asset?.failed) {
      try { peer.reportFailure(entry.id, asset.assetId, 'asset-unavailable', asset.message); } catch {}
    } else if (asset) {
      peer.clearFailure(entry.id, asset.assetId);
    }
    if (status.phase === 'failed' && status.error) {
      try { peer.reportFailure(entry.id, undefined, rendererFailureCode(status.error), 'The desktop could not be displayed. Your work is retained.'); } catch {}
      for (const waiter of [...entry.waiters]) if (entry.generation > waiter.after) waiter.reject(new Error('Desktop recovery failed'));
    } else if (status.phase === 'presented') {
      for (const waiter of [...entry.waiters]) {
        if (entry.generation > waiter.after) { peer.clearFailure(entry.id, undefined); waiter.resolve(); }
      }
    }
  }
  function dropSurface(entry) {
    if (surfaces.get(entry.id) !== entry) return;
    surfaces.delete(entry.id);
    for (const waiter of [...entry.waiters]) waiter.reject(new Error('Desktop surface closed'));
    try { peer?.clearSurface(entry.id); } catch {}
  }
  function surfaceOf(surfaceId) {
    const entry = surfaces.get(surfaceId);
    if (!entry?.mount) throw new Error('Unknown desktop surface');
    return entry;
  }

  /** Recreate the surface's GPU device, surface and renderer; resolves once a
   * frame of the new GPU generation is presented. Documents stay. */
  function retryDesktop(surfaceId) {
    const entry = surfaceOf(surfaceId);
    entry.recovering ??= new Promise((resolve, reject) => {
      const timer = setTimeout(() => waiter.reject(new Error('Desktop recovery timed out')), RECOVERY_TIMEOUT_MS);
      const settle = done => value => { clearTimeout(timer); entry.waiters.delete(waiter); done(value); };
      const waiter = {after: entry.generation, resolve: settle(resolve), reject: settle(reject)};
      entry.waiters.add(waiter);
      try { entry.mount.recover(); } catch (error) { waiter.reject(error); }
    }).finally(() => { entry.recovering = undefined; });
    return entry.recovering;
  }
  /** Fetch and verify the asset again; resolves once it is presented. */
  async function retryAsset(surfaceId, assetId) {
    const entry = surfaceOf(surfaceId);
    if (typeof entry.mount.retryAsset !== 'function') throw new Error('Asset recovery is unavailable');
    await entry.mount.retryAsset(assetId);
    peer.clearFailure(surfaceId, assetId);
  }
  /** Commit the store's failed writes again, then every mount's. */
  async function retrySaving() {
    await storage?.flush();
    for (const entry of [...surfaces.values()]) await entry.mount?.flush?.();
    peer.clearFailure(undefined, undefined);
  }
  /** Subscribe the rooms anew; resolves once the host admitted them again. */
  async function refreshCheckpoint() {
    if (!room) { peer.refresh(); return; }
    let resubscribed = false;
    const admitted = untilSnapshot(current => {
      const admission = current.health?.admission;
      if (!admission) return false;
      if (admission.phase !== 'ready') { resubscribed = true; return false; }
      return resubscribed && admission.pendingPrerequisites === 0;
    }, REFRESH_TIMEOUT_MS, 'The host did not send the shared state again');
    peer.refresh();
    await admitted;
  }

  async function certificate() {
    if (deviceCertificate) {
      await store.put(CERTIFICATE, deviceCertificate);
      deviceCertificate = undefined;
    }
    const existing = await store.get(CERTIFICATE);
    if (existing && !(identity && expiresWithinADay(existing))) return existing;
    const issued = identity
      ? await runtime.certifyWithRoot(post, identity, custodyForWasm, label)
      : await runtime.certifyDevice(post, userPublicKey, label, custodyForWasm, code => onPairingCode?.(code));
    await store.put(CERTIFICATE, issued);
    return issued;
  }

  async function start() {
    const certified = JSON.parse(await certificate());
    const record = storage ? undefined : await store.get(PEER_RECORD);
    peer = runtime.WasmPeer.start(
      JSON.stringify({cert: certified.cert, hostCert: certified.hostCert, displayName}),
      custodyForWasm,
      storage ? JSON.stringify({profile, device}) : undefined,
      storage ? undefined : (record ?? undefined),
      storage ? undefined : bytes => { store.put(PEER_RECORD, bytes).catch(() => {}); },
      post,
      channelVault,
    );
    peer.subscribe(publish);
    if (room) {
      peer.joinRoom(room);
      if (liveEnabled) {
        live = createLiveLayer(peer, runtime, room, {origin, endpoint: liveEndpoints(origin, host).endpoint, carriers, ready, participant: {
          kind: 'person', label: (displayName || label).slice(0, 32), color: colorOf(custodyKeyHex()), activity: null,
        }});
      }
      agents?.dispose();
      actions?.dispose();
      voice ??= voiceOptions === false ? undefined : createPeerVoice({peer: () => peer, room, ...voiceOptions});
      const hostDevice = hexKey(host.hostCert?.device_public_key);
      agents = hostDevice ? createAgents({peer: remote, runtime, room, live, userPublicKey, hostDevice}) : undefined;
      actions = hostDevice
        ? createActions({peer: remote, room, hostDevice, device: custodyKeyHex(), userPublicKey, store})
        : undefined;
    }
    publish();
  }
  /** The live layer of agent `agent` this page runs, created on first use. */
  function agentLive(agent) {
    let layer = agentLayers.get(agent);
    if (!layer && live) {
      layer = createLiveLayer(peer, runtime, room, {
        origin, endpoint: liveEndpoints(origin, host).endpoint, carriers, ready, agent,
        participant: {label: agent.slice(0, 32), color: colorOf(`agent:${agent}`)},
      });
      agentLayers.set(agent, layer);
    }
    return layer;
  }
  function stopLive() {
    for (const layer of layers()) layer.stop();
    agentLayers.clear();
  }
  // Every mount publishes through its own view of a layer, so the mount
  // acted in last speaks for the person (or the agent), never an idle one.
  const speakers = createSpeakers();
  function mountLive(target, options) {
    const speaker = speakers.mount();
    const layer = () => (typeof options.agent === 'string' && options.agent ? agentLive(options.agent) : live);
    const input = new AbortController();
    for (const type of ['pointerdown', 'keydown', 'focusin']) {
      target?.addEventListener?.(type, () => { const current = layer(); if (current) speaker.claim(current); },
        {capture: true, passive: true, signal: input.signal});
    }
    return {
      live: () => { const current = layer(); return current && speaker.view(current); },
      acted() { speaker.acted(); },
      release() { input.abort(); speaker.release(); },
    };
  }
  const hasRecord = (app, record) => Boolean(peer?.hasRecord?.(app, record));

  try {
    await start();
  } catch (error) {
    await storage?.release().catch(() => {});
    throw error;
  }

  async function dispose() {
    if (disposed) return;
    disposed = true;
    for (const entry of [...surfaces.values()]) dropSurface(entry);
    unsubscribeStorage?.();
    agents?.dispose();
    actions?.dispose();
    voice?.dispose();
    stopLive();
    peer?.dispose();
    listeners.clear();
    await storage?.release();
  }
  signal?.addEventListener('abort', dispose, {once: true});

  const certifiedPeer = {
    get host() { return host; },
    getSnapshot: () => snapshot,
    /** Diagnostic text of the page's channel membership (not a contract). */
    channelDiagnostics: () => peer?.channelDiagnostics?.() ?? '',
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    /** The live layer of the joined room, or `undefined` without a room. */
    get live() { return live; },
    /** The room's agents (`agents.mjs`), or `undefined` without a room. */
    get agents() { return agents; },
    /** Send input `events` for the legacy window `surface` (16 hex) that
     * `publisher` (64 hex) streams in the room; resolves whether the host
     * took them. With no events: whether this device may drive it. */
    legacyInput(publisher, surface, events) {
      if (!room || !peer) return Promise.resolve(false);
      return peer.legacyInput(room, publisher, surface, JSON.stringify(events));
    },
    /** The room's app actions (`actions.mjs`), or `undefined` without a room. */
    get actions() { return actions; },
    /** The room's calls (`voice-peer.mjs`), or `undefined` without a room. */
    get voice() { return voice; },
    /** `actions` in the frozen `actionsV1` contract. */
    get actionsV1() { return actions?.v1; },
    /** This peer in the wasm runtime, for mounts that replicate through it. */
    get runtimeId() { return peer?.id; },
    /** The desktop mount (`host.mjs`) in this peer's profile and room.
     * `options.surfaceId` names the surface in SessionHealth failures and
     * `retry-desktop`/`retry-asset` requests (default `surface-N`); the
     * mount carries it as `surfaceId`. */
    async mountDesktop(target, options = {}) {
      const surface = await surfaceModule();
      if (typeof surface.mountDesktop !== 'function') throw new Error('desktop mount is unavailable');
      const scope = {...(profile ? {profile, device} : {}), ...(room ? {room} : {})};
      const {surfaceId = `surface-${++surfaceCount}`, onStatus: forward, ...rest} = options;
      if (surfaces.has(surfaceId)) throw new Error(`desktop surface ${surfaceId} is already mounted`);
      const entry = {id: surfaceId, mount: undefined, generation: 0, presented: false, waiters: new Set(), recovering: undefined};
      surfaces.set(surfaceId, entry);
      const presence = mountLive(target, rest);
      let mount;
      try {
        mount = await surface.mountDesktop(target, {...scope, ...rest, peer: certifiedPeer, live: presence.live, onStatus: status => {
          observeSurface(entry, status);
          forward?.(status);
        }});
      } catch (error) {
        presence.release();
        dropSurface(entry);
        throw error;
      }
      if (surfaces.get(surfaceId) !== entry) { presence.release(); return mount; }
      entry.mount = mount;
      mount.surfaceId = surfaceId;
      const cleanup = mount.cleanup;
      mount.cleanup = async () => { presence.release(); dropSurface(entry); return cleanup?.(); };
      const invokeAction = mount.invokeAction;
      if (invokeAction) mount.invokeAction = (...args) => { presence.acted(); return invokeAction(...args); };
      return mount;
    },
    /** Resolves once the host admitted the room (`phase === 'ready'`);
     * rejects when it refused this device. */
    whenReady() {
      return new Promise((resolve, reject) => {
        const check = current => {
          if (current.phase === 'ready') { listeners.delete(check); resolve(current); }
          else if (current.phase === 'failed') { listeners.delete(check); reject(new Error(current.error ?? 'the host refused this device')); }
        };
        listeners.add(check);
        check(snapshot);
      });
    },
    /** The room's records this page holds, open in a mount or received:
     * `[{app, record}]` (a package's room document has record `''`). */
    records: () => JSON.parse(peer?.roomRecords?.() ?? '[]'),
    /** Whether this page holds the room's record `record` of `app`. */
    hasRecord,
    /** Resolves once the room's record `record` of `app` reached this page. */
    whenRecord(app, record, {timeoutMs = 30_000} = {}) {
      return new Promise((resolve, reject) => {
        if (hasRecord(app, record)) { resolve(); return; }
        const check = () => {
          if (!hasRecord(app, record)) return;
          clearTimeout(timer); listeners.delete(check); resolve();
        };
        const timer = setTimeout(() => { listeners.delete(check); reject(new Error(`the room sent no ${app} record ${record}`)); }, timeoutMs);
        listeners.add(check);
      });
    },
    /** The live layer of agent `agent` this page runs (created on first use;
     * a mount with `agent` publishes through it), or `undefined` without a
     * live plane. */
    agentLive,
    /** Resolves once every published change is durable on the host and the
     * peer's own state is durable in the profile. */
    flush: async () => { await peer.flush(); await storage?.flush(); },
    async reconnect() { peer.reconnect(); },
    /** `SessionHealth` v1 recovery: `retry-connection`, `reauthenticate`,
     * `retry-saving`, `refresh-checkpoint`, `retry-desktop` or
     * `retry-asset`. Resolves once the recovery took effect. */
    async recover(request) {
      if (request?.kind === 'retry-connection') { peer.reconnect(); return {operationId: 'retry-connection'}; }
      if (request?.kind === 'retry-saving') { await retrySaving(); return {operationId: 'retry-saving'}; }
      if (request?.kind === 'refresh-checkpoint') { await refreshCheckpoint(); return {operationId: 'refresh-checkpoint'}; }
      if (request?.kind === 'retry-desktop') {
        await retryDesktop(request.surfaceId);
        return {operationId: `retry-desktop:${request.surfaceId}`};
      }
      if (request?.kind === 'retry-asset') {
        await retryAsset(request.surfaceId, request.assetId);
        return {operationId: `retry-asset:${request.surfaceId}:${request.assetId}`};
      }
      if (request?.kind === 'reauthenticate') {
        voice?.reset();
        stopLive();
        peer.dispose();
        resetReady();
        await store.delete(CERTIFICATE);
        // The host may be another one now (a new certificate, a new user
        // root): what the page learnt of it at load is stale too.
        host = JSON.parse(await runtime.hostInfo(post));
        carriers = carriersFor(host);
        await start();
        return {operationId: 'reauthenticate'};
      }
      throw new TypeError(`unsupported recovery ${request?.kind}`);
    },
    /** Report the page store's durability of a record to SessionHealth. */
    setLocalState: (app, recordRoom, record, state) => peer.setLocalState(app, recordRoom, record, state),
    dispose,
  };
  return certifiedPeer;
}

const invitationBytes = invitation => {
  if (typeof invitation !== 'string') return Uint8Array.from(invitation);
  const text = invitation.trim().replace(/-/g, '+').replace(/_/g, '/');
  return Uint8Array.from(atob(text + '='.repeat((4 - text.length % 4) % 4)), char => char.charCodeAt(0));
};

/**
 * What an invitation (its signed bytes, or them as base64/base64url) offers
 * at the host `relayUrl`: `{workspaceName, role, room, workspaceId,
 * expiresAtNs, inviter}`. `inviter` is undefined: invitations do not name
 * who sent them.
 */
export async function inspectInvitation(relayUrl, invitation, {runtime, fetch: fetcher} = {}) {
  runtime ??= await pageRuntime();
  const reply = JSON.parse(await runtime.inspectInvitation(httpsPost(relayUrl, fetcher), invitationBytes(invitation)));
  if (reply.kind !== 'invitation') throw new Error(reply.reason ?? 'This invitation offers nothing here');
  return {
    workspaceName: reply.name, role: reply.role.toLowerCase(), room: reply.room, workspaceId: reply.workspaceId,
    expiresAtNs: reply.expiresAtNs, inviter: undefined,
  };
}

const JOIN_REQUESTS = 'tilo-join-requests';
const hexOf = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');

/**
 * Join the workspace `invitation` (as `inspectInvitation` takes) offers at
 * `origin`, as `identity` (default the current one): this page's device for
 * that host is certified by the person's own root, the request waits for
 * the room's owner (`onProgress({phase: 'waiting'})`, polled every
 * `pollMs`), and once admitted the page connects. Resolves to the
 * `CertifiedPeer` in the room; rejects when declined or refused. A page that
 * reloads while waiting asks again under the same request.
 */
export async function joinWorkspace(options) {
  const identity = options.identity ?? currentIdentity();
  if (!identity || options.custody || !options.origin) return join({...options, identity});
  // As connectPeer: the device key opened here is the peer's.
  const profile = options.profile ?? identityProfile(identity, options.origin);
  const own = await openDeviceCustody({profile, storage: idbStorage()});
  return withCustody(own, () => join({...options, identity, profile, custody: own}), options.signal);
}

async function join({
  origin, invitation, identity, displayName = '', label = 'Browser', signal, onProgress, pollMs = 2000,
  runtime, custody, profile, storage = globalThis.localStorage, fetch: fetcher, ...connectOptions
}) {
  identity ??= currentIdentity();
  if (!identity) throw new IdentityError('locked', 'Unlock the identity first');
  if (!origin || !invitation) throw new TypeError('origin and invitation are required');
  runtime ??= await pageRuntime(connectOptions.desktop);
  profile ??= identityProfile(identity, origin);
  custody ??= await openDeviceCustody({profile, storage: idbStorage()});
  const custodyForWasm = custody.kind === 'sealed-seed' ? await custody.unseal() : custody;
  const post = httpsPost(origin, fetcher);
  const bytes = invitationBytes(invitation);
  // The request id is public: it lets a reloaded page resume its request.
  const key = `${profile}:${hexOf(bytes.slice(-32))}`;
  const saved = JSON.parse(storage?.getItem?.(JOIN_REQUESTS) ?? '{}');
  const requestId = saved[key] ?? hexOf(crypto.getRandomValues(new Uint8Array(32)));
  saved[key] = requestId;
  storage?.setItem?.(JOIN_REQUESTS, JSON.stringify(saved));
  const forget = () => {
    const current = JSON.parse(storage?.getItem?.(JOIN_REQUESTS) ?? '{}');
    delete current[key];
    storage?.setItem?.(JOIN_REQUESTS, JSON.stringify(current));
  };
  signal?.throwIfAborted();
  onProgress?.({phase: 'asking'});
  const asked = JSON.parse(await runtime.askToJoin(post, bytes, identity, custodyForWasm, requestId,
    (displayName || label).slice(0, 64), label));
  let reply = asked.reply;
  const device = hexOf(asked.cert.device_public_key);
  while (reply.kind === 'waiting') {
    onProgress?.({phase: 'waiting'});
    await new Promise((resolve, reject) => {
      const timer = setTimeout(resolve, pollMs);
      signal?.addEventListener('abort', () => { clearTimeout(timer); reject(signal.reason); }, {once: true});
    });
    reply = JSON.parse(await runtime.joinStatus(post, requestId, device));
  }
  if (reply.kind !== 'admitted') {
    forget();
    throw new Error(reply.kind === 'declined' ? 'The owner declined the request' : (reply.reason ?? 'The host refused the request'));
  }
  forget();
  onProgress?.({phase: 'admitted', room: reply.room, workspaceName: reply.name, role: reply.role.toLowerCase()});
  const host = JSON.parse(await runtime.hostInfo(post));
  return connect({
    ...connectOptions, origin, identity, room: reply.room, label, displayName, custody, runtime, signal,
    profile: connectOptions.store ? undefined : profile,
    fetch: fetcher, deviceCertificate: JSON.stringify({cert: asked.cert, hostCert: host.hostCert}),
  });
}

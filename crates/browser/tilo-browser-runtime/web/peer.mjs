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
 *   admitted by a host grant for this session. Never authority.
 *
 * The page holds one device key (`device-custody.mjs`) and no user root.
 * `getSnapshot()` returns `PeerSnapshot` with `SessionHealth` v1.
 */
import {createPresencePublisher, createRemotePresence} from './live-presence.mjs';
import {createLiveCarriers} from './live-fallback.mjs';
import {createLiveTransport, LIVE_PATH, LIVE_ROOT} from './live-transport.mjs';

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

/** `post(path, session, body)` over `fetch` to the host origin. */
export function httpsPost(origin, fetcher = (...args) => globalThis.fetch(...args)) {
  const base = new URL(origin);
  if (base.protocol !== 'https:' || base.pathname !== '/' || base.search || base.hash) {
    throw new TypeError('host origin must be an https origin');
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
 * The live layer of one room — the interface the desktop mount consumes.
 * `peer` is the wasm `WasmPeer`, `runtime` the wasm module (live codec).
 */
export function createLiveLayer(peer, runtime, room, {
  origin, carriers, participant, now = () => performance.now(), wallNow = () => Date.now(),
  setTimer = (callback, delay) => globalThis.setTimeout(callback, delay),
  clearTimer = handle => globalThis.clearTimeout(handle),
} = {}) {
  const endpoint = new URL(LIVE_PATH, origin).href;
  const remotes = new Map();
  const previews = new Map();
  const members = new Set();
  const watched = new Map(); // record key -> record
  const subscriptions = new Map(); // `${publisher}\u0000${track}` -> cleanup
  const listeners = {presence: new Set(), preview: new Set(), member: new Set(), durable: new Set(), state: new Set()};
  const emit = (kind, value) => { for (const listener of listeners[kind]) { try { listener(value); } catch {} } };
  const on = kind => listener => { listeners[kind].add(listener); return () => listeners[kind].delete(listener); };
  let session;
  let state = 'connecting';
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
  });

  function onPresence(publisherId, payload, receivedAtMs) {
    let decoded;
    try { decoded = JSON.parse(runtime.decodeLiveFrame(payload)); } catch { return; }
    if (decoded.kind !== 'presence') return;
    let remote = remotes.get(publisherId);
    if (!remote) { remote = createRemotePresence(); remotes.set(publisherId, remote); }
    if (remote.accept(decoded.frame, receivedAtMs)) {
      emit('presence', {publisher: publisherId, frame: decoded.frame, receivedAtMs});
    }
  }
  function onOps(publisherId, payload) {
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
  function keepSubscribed(publisherId, key, open, attempt = 0) {
    const current = session;
    if (!current || subscriptions.has(key) || !members.has(publisherId)) return;
    const cleanup = open(current, () => {
      if (subscriptions.get(key) !== cleanup) return;
      subscriptions.delete(key);
      if (stopped || session !== current || !members.has(publisherId)) return;
      const delay = SUBSCRIBE_RETRY_MS[Math.min(attempt, SUBSCRIBE_RETRY_MS.length - 1)];
      setTimer(() => keepSubscribed(publisherId, key, open, attempt + 1), delay);
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
      retry = 0;
      setState('live');
      session.watchMembers({onMember});
      if (participant) publisher.update({participant});
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

  connect();

  return {
    room,
    get state() { return state; },
    /** Merge presence fields (`null` clears): focus, pointer, selection, drag. */
    publishPresence: partial => publisher.update(partial),
    /** Ephemeral text preview operations for a record. */
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
    /** Receive previews for a record from every member. */
    watchRecord(record) {
      watched.set(recordKey(record), record);
      for (const publisherId of members) subscribeRecord(publisherId, record);
      return () => watched.delete(recordKey(record));
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
    onPresence: on('presence'),
    onPreview: on('preview'),
    onMember: on('member'),
    onDurable: on('durable'),
    onState: on('state'),
    /** Apply durable events from the session (called on every peer change). */
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
 * @param options.displayName    the person's name for history and presence
 * @param options.runtime        the wasm module (`certifyDevice`, `WasmPeer`, live codec)
 * @param options.fetch          defaults to `globalThis.fetch`
 * @param options.liveCarriers   defaults to WebTransport with HTTPS fallback
 * @param options.desktop        `{mountDesktop(target, options)}`; defaults to `host.mjs`
 * @param options.onPairingCode  shown while an approver decides
 */
export async function connectPeer({
  origin, userPublicKey, label = 'Browser', room, custody, store, runtime,
  profile, device = 'tilo-browser-device', displayName = '',
  fetch: fetcher, liveCarriers, desktop, signal, onStatus, onPairingCode,
}) {
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
  const post = httpsPost(origin, fetcher);
  const custodyForWasm = custody.kind === 'sealed-seed' ? await custody.unseal() : custody;
  const custodyKeyHex = () => [...(custodyForWasm.publicKey ?? [])].map(b => b.toString(16).padStart(2, '0')).join('');
  const carriersFor = info => liveCarriers ?? createLiveCarriers({
    fetch: fetcher,
    webtransport: createLiveTransport({
      serverCertificateHashes: info.certificateSha256
        ? [{algorithm: 'sha-256', value: unhex(info.certificateSha256)}]
        : undefined,
    }),
  });
  let host = JSON.parse(await runtime.hostInfo(post));
  let carriers = carriersFor(host);
  const listeners = new Set();
  let peer;
  let live;
  let disposed = false;
  let snapshot = {phase: 'authenticating', records: []};
  // Desktop surfaces mounted through this peer, by surface id: their failures
  // are reported into SessionHealth and recovered through `recover`.
  const surfaces = new Map();
  let surfaceCount = 0;

  const publish = () => {
    if (!peer) return;
    snapshot = JSON.parse(peer.snapshot());
    live?.pump();
    for (const listener of listeners) listener(snapshot);
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
    const existing = await store.get(CERTIFICATE);
    if (existing) return existing;
    const issued = await runtime.certifyDevice(post, userPublicKey, label, custodyForWasm,
      code => onPairingCode?.(code));
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
    );
    peer.subscribe(publish);
    if (room) {
      peer.joinRoom(room);
      live = createLiveLayer(peer, runtime, room, {origin, carriers, participant: {
        kind: 'person', label: (displayName || label).slice(0, 32), color: colorOf(custodyKeyHex()), activity: null,
      }});
    }
    publish();
  }

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
    live?.stop();
    peer?.dispose();
    listeners.clear();
    await storage?.release();
  }
  signal?.addEventListener('abort', dispose, {once: true});

  const certifiedPeer = {
    get host() { return host; },
    getSnapshot: () => snapshot,
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    /** The live layer of the joined room, or `undefined` without a room. */
    get live() { return live; },
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
      let mount;
      try {
        mount = await surface.mountDesktop(target, {...scope, ...rest, peer: certifiedPeer, onStatus: status => {
          observeSurface(entry, status);
          forward?.(status);
        }});
      } catch (error) {
        dropSurface(entry);
        throw error;
      }
      if (surfaces.get(surfaceId) !== entry) return mount;
      entry.mount = mount;
      mount.surfaceId = surfaceId;
      const cleanup = mount.cleanup;
      if (typeof cleanup === 'function') mount.cleanup = async () => { dropSurface(entry); return cleanup(); };
      return mount;
    },
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
        live?.stop();
        peer.dispose();
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

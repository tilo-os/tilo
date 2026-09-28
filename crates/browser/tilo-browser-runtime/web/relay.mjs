/**
 * The page as a certified device of a tilo-relay room (a core built with
 * `--features relay`). After #1432's `peer.mjs`, on this runtime:
 *
 * - device certification over the host's HTTPS origin: the page holds one
 *   non-extractable device key (`device-custody.mjs`) and no user root, and
 *   shows a pairing code while a root-holding device (or the room's
 *   operator) approves;
 * - the room's documents over the relay's HTTPS durable plane, replicated
 *   into the page's own mounts: `peer.mountDesktop(target, options)` is
 *   `host.mjs`'s `mountDesktop` in the peer's room, so apps, the local
 *   desktop, `open`, `actions` and `invokeAction` behave as in any mount,
 *   with the core's and the registry's side modules.
 *
 * The certificate is kept as the JSON text the core wrote: its u64
 * nanosecond times exceed a JS number's precision, and a re-encoded
 * certificate no longer matches its signature.
 */
import {coreRuntime, mountDesktop} from './host.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';

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

/** `{get, put}` of this profile's peer values in IndexedDB. */
export function peerValues(profile, {indexedDB = globalThis.indexedDB, name = 'tilo-relay-peer'} = {}) {
  const open = () => new Promise((resolve, reject) => {
    const request = indexedDB.open(name, 1);
    request.onupgradeneeded = () => request.result.createObjectStore('values');
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  const run = async (mode, operate) => {
    const db = await open();
    try {
      return await new Promise((resolve, reject) => {
        const transaction = db.transaction('values', mode, {durability: 'strict'});
        const request = operate(transaction.objectStore('values'));
        transaction.oncomplete = () => resolve(request.result);
        transaction.onerror = transaction.onabort = () => reject(transaction.error);
      });
    } finally { db.close(); }
  };
  return {
    get: key => run('readonly', store => store.get(`${profile}\u0000${key}`)),
    put: (key, value) => run('readwrite', store => store.put(value, `${profile}\u0000${key}`)),
    delete: key => run('readwrite', store => store.delete(`${profile}\u0000${key}`)),
  };
}

const CERTIFICATE = 'certificate';
const PEER_RECORD = 'peer-record';

/**
 * Connect this page to relay room `room` at `origin` as a device of the
 * room's user root `userPublicKey` (hex). `onPairingCode(code)` is called
 * while the host waits for an approver; a page with the certificate from an
 * earlier visit skips that. Resolves once the session started (observe
 * `getSnapshot().phase` for `ready`).
 */
export async function connectRelayPeer({
  origin, userPublicKey, room, label = 'Browser', profile = `tilo-relay:${room}`, device = 'relay',
  onPairingCode, onStatus, signal, fetch: fetcher,
}) {
  if (!origin || !userPublicKey || !room) throw new TypeError('origin, userPublicKey and room are required');
  signal?.throwIfAborted();
  const runtime = await coreRuntime();
  if (typeof runtime.certifyDevice !== 'function') {
    throw new Error('this core has no relay peer: build it with --features relay');
  }
  const post = httpsPost(origin, fetcher);
  const custody = await openDeviceCustody({profile, storage: idbStorage()});
  const custodyForWasm = custody.kind === 'sealed-seed' ? await custody.unseal() : custody;
  const values = peerValues(profile);
  let certified = await values.get(CERTIFICATE);
  if (typeof certified !== 'string') {
    certified = await runtime.certifyDevice(post, userPublicKey, label, custodyForWasm, code => onPairingCode?.(code));
    await values.put(CERTIFICATE, certified);
  }
  signal?.throwIfAborted();
  const record = await values.get(PEER_RECORD);
  const peer = runtime.WasmPeer.start(certified, custodyForWasm, record ?? undefined,
    bytes => { values.put(PEER_RECORD, bytes).catch(() => {}); }, post);
  const listeners = new Set();
  let snapshot = {phase: 'authenticating'};
  const publish = () => {
    snapshot = JSON.parse(peer.snapshot());
    for (const listener of [...listeners]) listener(snapshot);
    onStatus?.(snapshot);
  };
  peer.subscribe(publish);
  peer.joinRoom(room);
  publish();
  let disposed = false;
  const dispose = () => { if (disposed) return; disposed = true; listeners.clear(); peer.dispose(); };
  signal?.addEventListener('abort', dispose, {once: true});
  const relay = {
    id: peer.id,
    room,
    getSnapshot: () => snapshot,
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    /** Resolves once the host admitted the room (`phase === 'ready'`). */
    whenReady() {
      return new Promise((resolve, reject) => {
        const check = current => {
          if (current.phase === 'ready') { stop(); resolve(current); }
          else if (current.phase === 'failed') { stop(); reject(new Error(current.error ?? 'the host refused this device')); }
        };
        const stop = relay.subscribe(check);
        check(snapshot);
      });
    },
    /** The room's records this page holds, open in a mount or replicated: `[{app, record}]`. */
    records: () => JSON.parse(peer.roomRecords()),
    /** Whether this page holds the room's record `record` of `app`. */
    hasRecord: (app, record) => peer.hasRecord(app, record),
    /** Resolves once the room's record `record` of `app` reached this page. */
    whenRecord(app, record, {timeoutMs = 30_000} = {}) {
      return new Promise((resolve, reject) => {
        if (peer.hasRecord(app, record)) { resolve(); return; }
        const timer = setTimeout(() => { stop(); reject(new Error(`the room sent no ${app} record ${record}`)); }, timeoutMs);
        const stop = relay.subscribe(() => {
          if (peer.hasRecord(app, record)) { clearTimeout(timer); stop(); resolve(); }
        });
      });
    },
    /** `host.mjs`'s `mountDesktop` in this room: the mount's records
     * replicate through the peer. */
    mountDesktop: (target, options = {}) => mountDesktop(target, {...options, profile, device, room, relay: peer.id}),
    /** Resolves once every published change is durable on the host. */
    flush: () => peer.flush(),
    reconnect: () => peer.reconnect(),
    /** The runtime's `WasmPeer`, for the live plane (`liveGrant`, codec). */
    get peer() { return peer; },
    dispose,
  };
  return relay;
}

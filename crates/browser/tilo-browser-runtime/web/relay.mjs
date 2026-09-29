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
 *   with the core's and the registry's side modules;
 * - the room's live plane (`relay-live.mjs`): the mounts publish this
 *   device's pointer, caret and selection and draw the other members'.
 *
 * A room on a loopback `http:` origin (local review, `tilo-relay room
 * --web-plain`) is accepted: the page then needs no certificate exception.
 *
 * The certificate is kept as the JSON text the core wrote: its u64
 * nanosecond times exceed a JS number's precision, and a re-encoded
 * certificate no longer matches its signature.
 */
import {coreRuntime, mountDesktop} from './host.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';
import {colorOf, createRelayLive, relayLiveCarriers} from './relay-live.mjs';

const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);

/** `post(path, session, body)` over `fetch` to the host origin. */
export function httpsPost(origin, fetcher = (...args) => globalThis.fetch(...args)) {
  const base = new URL(origin);
  const secure = base.protocol === 'https:' || (base.protocol === 'http:' && LOOPBACK.has(base.hostname));
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
  onPairingCode, onStatus, signal, fetch: fetcher, live: liveEnabled = true,
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
  let live;
  // Agents this device runs publish under their own namespaces: one layer each.
  const agentLayers = new Map();
  const layers = () => [...(live ? [live] : []), ...agentLayers.values()];
  let admitted;
  const ready = new Promise(resolve => { admitted = resolve; });
  const publish = () => {
    snapshot = JSON.parse(peer.snapshot());
    if (snapshot.phase === 'ready') admitted();
    if (live) {
      const events = JSON.parse(peer.takeDurableEvents());
      for (const layer of layers()) layer.pump(events);
    }
    for (const listener of [...listeners]) listener(snapshot);
    onStatus?.(snapshot);
  };
  peer.subscribe(publish);
  peer.joinRoom(room);
  let hostInfo;
  const liveOptions = () => relayLiveCarriers(origin, hostInfo, {fetch: fetcher});
  if (liveEnabled) {
    hostInfo = JSON.parse(await runtime.hostInfo(post));
    const key = [...(custodyForWasm.publicKey ?? [])].map(byte => byte.toString(16).padStart(2, '0')).join('');
    live = createRelayLive(peer, runtime, room, {
      ...liveOptions(), ready, participant: {label: label.slice(0, 32), color: colorOf(key || label)},
    });
  }
  const agentLive = agent => {
    let layer = agentLayers.get(agent);
    if (!layer) {
      layer = createRelayLive(peer, runtime, room, {
        ...liveOptions(), ready, agent, participant: {label: agent.slice(0, 32), color: colorOf(`agent:${agent}`)},
      });
      agentLayers.set(agent, layer);
    }
    return layer;
  };
  publish();
  let disposed = false;
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    listeners.clear();
    for (const layer of layers()) layer.stop();
    peer.dispose();
  };
  // A mount acts for an agent when it says so (`agent`), or when its author
  // is the name of an agent this page runs (an agent member of its desktop,
  // or another mount's `agent`).
  const agentNames = new Set();
  const learnAgents = options => {
    if (typeof options.agent === 'string' && options.agent) agentNames.add(options.agent);
    for (const workspace of options.local?.workspaces ?? []) {
      for (const member of workspace?.members ?? []) if (member?.agent && typeof member.name === 'string') agentNames.add(member.name);
    }
  };
  // One presence per layer: of the mounts sharing it, the one the person
  // (or the agent) acted in last publishes; idle or hidden mounts never
  // overwrite it.
  const speaker = new Map();
  const mountLive = (target, options) => {
    const token = {};
    const views = new Map();
    const current = () => {
      const agent = typeof options.agent === 'string' && options.agent ? options.agent
        : agentNames.has(options.author) ? options.author : undefined;
      return agent ? agentLive(agent) : live;
    };
    const speaks = layer => {
      if ((speaker.get(layer) ?? token) !== token) return false;
      speaker.set(layer, token);
      return true;
    };
    const view = layer => {
      let found = views.get(layer);
      if (!found) {
        found = Object.create(layer.api, {
          publishPresence: {value: partial => speaks(layer) && layer.api.publishPresence(partial)},
          publishPreview: {value: (record, ops) => speaks(layer) && layer.api.publishPreview(record, ops)},
        });
        views.set(layer, found);
      }
      return found;
    };
    const claim = () => speaker.set(current(), token);
    const release = () => { for (const layer of layers()) if (speaker.get(layer) === token) speaker.delete(layer); };
    const input = new AbortController();
    for (const type of ['pointerdown', 'keydown', 'focusin']) {
      target.addEventListener(type, claim, {capture: true, passive: true, signal: input.signal});
    }
    return {
      options: {live: () => view(current()), liveMembers: () => JSON.stringify(current().members())},
      adopt(mount) {
        const {invokeAction, cleanup} = mount;
        mount.invokeAction = (...args) => { claim(); return invokeAction(...args); };
        mount.cleanup = async (...args) => { input.abort(); release(); return cleanup(...args); };
        return mount;
      },
    };
  };
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
     * replicate through the peer, and it shows the room's live presence.
     * A mount of an agent this page runs publishes as that agent. */
    async mountDesktop(target, options = {}) {
      if (!live) return mountDesktop(target, {...options, profile, device, room, relay: peer.id});
      learnAgents(options);
      const presence = mountLive(target, options);
      return presence.adopt(await mountDesktop(target, {...options, profile, device, room, relay: peer.id, ...presence.options}));
    },
    /** Resolves once every published change is durable on the host. */
    flush: () => peer.flush(),
    reconnect: () => peer.reconnect(),
    /** The room's live plane (`liveV1`: `state`, `memberCount`,
     * `remoteState()`, `onMember`, …), or `undefined` with `live: false`. */
    get live() { return live?.api; },
    /** The live plane of agent `agent` this page runs, once one of its mounts exists. */
    agentLive: agent => agentLayers.get(agent)?.api,
    /** The runtime's `WasmPeer`. */
    get peer() { return peer; },
    dispose,
  };
  return relay;
}

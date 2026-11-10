/**
 * `connectPeer` → `CertifiedPeer`: the page as a certified Tilo peer of the
 * user's host. Before it holds a certificate the page asks the host's HTTPS
 * origin only what it cannot ask a session: the host's certificate and
 * address, joining a workspace, and a member's approval of their own
 * device. Everything after goes over one iroh session to the host, through
 * the packet relay the host names (a page holds no UDP socket), the same
 * session a native device holds (`tilo-session-host` in wasm):
 *
 * - durable documents: signed commits from a persisted outbox, acknowledged
 *   in host order, replayed after every reconnect;
 * - device certification: the page shows the pairing code while a
 *   root-holding device of the user approves;
 * - the live plane (`peer.live`): this page as one member device of the
 *   workspace beside every other, its presence, pointers, previews and shell
 *   on the same session link they use. Never authority;
 * - the workspace's calls (`peer.voice`, `voice-peer.mjs`): the session holds
 *   the participation and seals the page's frames, which reach the call as
 *   turns on its host's call lane (`voice.mjs`).
 *
 * The page holds one device key (`device-custody.mjs`): each tab is a
 * device of its own, with its own key, certificate and state. A page that also
 * holds the user root (`identity.mjs`, `options.identity`) certifies
 * that key itself instead of waiting for an approver.
 * `getSnapshot()` returns `PeerSnapshot` with `SessionHealth` v1.
 */
import {resolveResourceReference, openResourceReference} from './resource-reference.mjs';
import {backoffDelay} from './reconnect.mjs';
import {createSpeakers} from './live-speakers.mjs';
import {createPeerVoice} from './voice-peer.mjs';
import {memberText} from './json-text.mjs';
import {WEBKIT_WEBGPU_REFUSAL} from './webgpu-safety.mjs';
import {AdmissionError} from './admission.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';
import {identityProfile} from './identity-profile.mjs';
import {currentIdentity, fitDisplayName, IdentityError} from './identity.mjs';
import {FETCH_PROXY_PATH} from './fetch-proxy.mjs';

export {
  addPasskey, browserIdentity, browserPerson, certifyDevice, createIdentity, currentIdentity, fetchEnvelope, fetchProfile, fitDisplayName, IdentityError, installNode,
  installNodeCertificate, MAX_DISPLAY_NAME_BYTES, nodeInstallInfo, publishEnvelope, publishProfile, signInWithPasskey, storagePersisted, unlockIdentity,
} from './identity.mjs';
// The workspaces a page can open, for hosts of this module (Tilo Cloud) that
// import only the peer.
export {joinViaHost, listedWorkspaces, workspaceAt} from './workspaces.mjs';

const CERTIFICATE = 'certificate';
/**
 * The stored `{cert, hostCert}` JSON with the peer's start fields (the
 * person's name, whether it only watches) added, edited as text: certificate
 * timestamps are nanoseconds, beyond what a JavaScript number holds exactly,
 * and a parsed and re-serialized copy no longer matches its signature.
 */
export function withStartFields(certified, fields) {
  const added = Object.entries(fields)
    .filter(([, value]) => value !== undefined)
    .map(([key, value]) => `,${JSON.stringify(key)}:${JSON.stringify(value)}`)
    .join('');
  return added ? certified.replace(/\}\s*$/, `${added}}`) : certified;
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
 * and documents there (`identity-profile.mjs`). */
export {identityProfile};

/** The namespace of a profile's storage its peer keeps its certificate,
 * record (outbox, synced documents) and channel state in, until the host at
 * the profile's origin is set up again. Each device key of the profile (a
 * custody slot, `device-custody.mjs`: one per tab open at once) is a device
 * of its own with a namespace of its own: slot 0 keeps this one, slot `n`
 * the same name with `#n` after it. */
export const PROFILE_DEVICE = 'tilo-browser-device';
/** The name of a namespace for the custody slot `slot`. */
const forSlot = (namespace, slot) => (slot ? `${namespace}#${slot}` : namespace);
/** Which namespace each profile (and each slot past the first, as
 * `profile#slot`) keeps its peer in now (`localStorage`): absent for one
 * still in its first namespace. */
const PROFILE_DEVICES = 'tilo-profile-devices';
const readProfileDevices = namespaces => {
  try {
    const all = JSON.parse(namespaces?.getItem(PROFILE_DEVICES) ?? '{}');
    return all && typeof all === 'object' && !Array.isArray(all) ? all : {};
  } catch { return {}; }
};
/** The namespace the peer of `profile`'s custody slot `slot` keeps its
 * state in now. */
export function profileDevice(profile, namespaces = globalThis.localStorage, slot = 0) {
  const device = readProfileDevices(namespaces)[forSlot(profile, slot)];
  return typeof device === 'string' && device ? device : forSlot(PROFILE_DEVICE, slot);
}
function keepProfileDevice(profile, slot, device, namespaces) {
  const all = readProfileDevices(namespaces);
  all[forSlot(profile, slot)] = device;
  namespaces?.setItem(PROFILE_DEVICES, JSON.stringify(all));
}
/**
 * Replace the device key `custody` holds for `profile` with a new one, in a
 * namespace of its own: a new device of the same person. For a key a host
 * knows under a certificate this browser no longer holds, which that host
 * never admits again under another. What was kept for the old key stays in
 * its namespace, never opened for the new one. Resolves to the new custody.
 */
export async function renewDevice(custody, profile, {
  open = name => openDeviceCustody({profile: name, storage: idbStorage()}), namespaces = globalThis.localStorage,
  random = bytes => crypto.getRandomValues(bytes),
} = {}) {
  await custody.destroy();
  const renewed = await open(profile);
  // Another tab may have taken the freed slot meanwhile: a key that is not
  // new keeps the namespace it has.
  if (renewed.created) keepProfileDevice(profile, renewed.slot ?? 0, forSlot(`${PROFILE_DEVICE}:${hexKey(random(new Uint8Array(16)))}`, renewed.slot ?? 0), namespaces);
  return renewed;
}
/** The namespace a profile's slot keeps a host's state in once that host
 * replaced the one it kept state for: named after the host's device key, so
 * it never shares a record with another host's. */
const hostNamespace = (hostCert, slot) => {
  const key = hexKey(hostCert?.device_public_key);
  return /^[0-9a-f]{64}$/.test(key) ? forSlot(`${PROFILE_DEVICE}:${key}`, slot) : undefined;
};
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
/** Rejoining the live plane: the first wait and the longest, exponential
 * between, each jittered by `reconnect.mjs` (`backoffDelay`) so the members
 * of a workspace whose relay restarted do not all come back in the same tick. */
const LIVE_RETRY = {firstMs: 250, factor: 4, capMs: 10_000, jitter: 0.2};
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

/** `InvokeArgument` for a JavaScript value: nothing, a prompted text, or a typed object. */
function invokeArgument(argument) {
  if (argument === undefined || argument === null || argument === 'None') return 'None';
  if (typeof argument === 'string') return {Text: argument};
  if (typeof argument === 'object' && (argument.Text !== undefined || argument.Json !== undefined)) return argument;
  if (typeof argument === 'object' && !Array.isArray(argument)) return {Json: argument};
  throw new TypeError('an action argument is a string or an object');
}

/** The person's answer to an action that asks first, as the host takes it. */
function wireConfirmation(confirmation) {
  if (!confirmation) return undefined;
  if (confirmation.kind === 'phrase') return {Phrase: String(confirmation.phrase ?? '')};
  return 'Confirmed';
}

const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);
/** A host served over plain HTTP on this machine (`tilo-relay workspace --web-plain`). */
const isLoopbackHttp = url => url.protocol === 'http:' && LOOPBACK.has(url.hostname);

/** `post(path, body)` over `fetch` to the host origin: https, or loopback
 * http for local review (no certificate exception needed). */
export function httpsPost(origin, fetcher = (...args) => globalThis.fetch(...args)) {
  const base = new URL(origin);
  const secure = base.protocol === 'https:' || isLoopbackHttp(base);
  if (!secure || base.pathname !== '/' || base.search || base.hash) {
    throw new TypeError('host origin must be an https (or loopback http) origin');
  }
  return async (path, body) => {
    const response = await fetcher(new URL(path, base).href, {
      method: 'POST',
      headers: {'content-type': 'application/octet-stream'},
      body, credentials: 'omit', cache: 'no-store', redirect: 'error',
    });
    return {status: response.status, body: new Uint8Array(await response.arrayBuffer())};
  };
}

/**
 * The live layer of one workspace: this page's presence on the workspace's
 * board, and the members' ephemeral previews.
 *
 * The board is the session's, run here through the peer (`peer.sayPresence`,
 * `peer.presenceDue`, `peer.presenceMembers`): it keeps what this page says
 * of itself — its face, its pointer, its wave, who it follows — and says what
 * changed on the one schedule every device's presence keeps; it merges what
 * the members say into each of them. This layer hands it each change, arms
 * one timer for the next thing it owes, and reads back who is here. The mount
 * draws them, through the same path a native desktop does.
 */
export function createLiveLayer(peer, runtime, workspace, {
  participant, ready = Promise.resolve(),
  setTimer = (callback, delay) => globalThis.setTimeout(callback, delay),
  clearTimer = handle => globalThis.clearTimeout(handle),
} = {}) {
  const listeners = {preview: new Set(), durable: new Set(), state: new Set()};
  const emit = (kind, value) => { for (const listener of [...listeners[kind]]) { try { listener(value); } catch {} } };
  const on = kind => listener => { listeners[kind].add(listener); return () => listeners[kind].delete(listener); };
  let publisherId = peer.deviceKey?.();
  let state = 'connecting';
  let stopped = false;
  let lastPublishError = null;
  let timer;
  const previewSeq = new Map();
  /** What this page says of itself: the value the board publishes. */
  const value = {};

  const setState = next => { if (state !== next) { state = next; emit('state', {state}); } };
  const clear = () => { if (timer !== undefined) clearTimer(timer); timer = undefined; };
  /** Arm the one timer for when the board next has something to say. */
  function arm(next) {
    clear();
    if (stopped || !(next > 0)) return;
    timer = setTimer(() => {
      timer = undefined;
      if (stopped) return;
      try { arm(peer.presenceDue(workspace)); } catch (error) { lastPublishError = String(error?.message ?? error); }
    }, Math.max(0, next - Date.now()));
  }
  function say() {
    if (stopped) return;
    try { arm(peer.sayPresence(workspace, JSON.stringify(value))); lastPublishError = null; } catch (error) { lastPublishError = String(error?.message ?? error); }
  }

  function preview(publisher, record, seq, ops) {
    emit('preview', {publisher, record, seq: ops ? seq : null, ops: ops ?? []});
  }

  function durableChanged(record) {
    emit('durable', {record});
  }

  /** Take what the workspace said since the last call: the members'
   * ephemeral previews (their presence is merged into the board as it
   * arrives, never handed on). */
  function pump(events = JSON.parse(peer.takeLiveEvents())) {
    for (const event of events) {
      switch (event.kind) {
        case 'durable': durableChanged(event.record); break;
        case 'preview': preview(event.publisher, event.record, event.seq, event.ops); break;
        case 'previewEnd': preview(event.publisher, event.record, null, null); break;
        default: break;
      }
    }
  }

  function start() {
    if (stopped) return;
    publisherId = peer.deviceKey?.() ?? publisherId;
    if (participant) value.participant = {kind: 'person', profile_ms: 0, ...participant};
    say();
    setState('live');
  }

  ready.then(() => start(), () => { if (!stopped) setState('unavailable'); });

  return {
    workspace,
    get state() { return state; },
    get available() { return state === 'live'; },
    /** This layer's publisher in the workspace (the device), once live. */
    get publisherId() { return publisherId; },
    /** Merge presence fields (`null` clears): focus, pointer, selection,
     * place, layout, chat, reaction, wave, following. */
    publishPresence(partial) {
      if (stopped || !partial) return;
      for (const [key, part] of Object.entries(partial)) {
        if (part === null) delete value[key];
        else if (part !== undefined) value[key] = part;
      }
      say();
    },
    /** Ephemeral text preview operations for a record. */
    publishPreview(record, ops) {
      if (stopped) return false;
      const key = `${record.app}\u0000${record.record}`;
      const seq = (previewSeq.get(key) ?? 0) + 1;
      previewSeq.set(key, seq);
      peer.publishPreview(workspace, JSON.stringify(record), BigInt(seq), JSON.stringify(ops)).catch(() => {});
      return true;
    },
    endPreview(record) {
      if (stopped) return;
      previewSeq.delete(`${record.app}\u0000${record.record}`);
      peer.endPreview(workspace, JSON.stringify(record)).catch(() => {});
    },
    /** A durable change for the record landed: drop its previews. */
    durableChanged,
    /** Everyone here now, as the board merged their words: `[{publisher,
     * epoch, agent, label, activity, colour, device, user, focus, place,
     * following, pointer, selection, chat, reaction, layout, availability,
     * call}]`, `pointer` with `ageMs`. */
    presenceMembers() {
      try { return JSON.parse(peer.presenceMembers(workspace)) ?? []; } catch { return []; }
    },
    members() { return this.presenceMembers().map(member => member.publisher); },
    onPreview: on('preview'),
    onDurable: on('durable'),
    onState: on('state'),
    stats: () => ({state, lastPublishError}),
    /** Apply the session's live and durable events (called on every peer
     * change); pass `events` when several layers share one peer. */
    pump,
    stop() {
      stopped = true;
      clear();
      try { peer.leavePresence(workspace); } catch {}
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
 * @param options.workspace           workspace uuid to join, optional
 * @param options.custody        from `openDeviceCustody`
 * @param options.profile        profile whose durable storage keeps the peer's
 *                               certificates and outbox (with `device`); its
 *                               desktop mounts share that storage
 * @param options.device         device namespace of `profile`
 * @param options.store          `{get, put, delete}` for public peer state;
 *                               defaults to the profile's storage
 * @param options.displayName    the person's name for history and presence
 * @param options.runtime        the wasm module (`certifyDevice`, `WasmPeer`, live codec);
 *                               defaults to the page host's (`desktop`'s)
 * @param options.fetch          defaults to `globalThis.fetch`
 * @param options.desktop        `{mountDesktop(target, options)}`; defaults to `host.mjs`
 * @param options.live           `false`: join no live plane
 * @param options.onPairingCode  shown while an approver decides
 * @param options.identity       an unlocked root (`identity.mjs`): the
 *                               page certifies its own device with it, no
 *                               approver; `userPublicKey` defaults to its key,
 *                               `profile` to one per identity and `custody` to
 *                               that profile's device key
 *                               (without one, `custody` defaults to `profile`'s
 *                               device key, certified by an approver)
 * @param options.deviceCertificate  `{cert, hostCert}` JSON this device was
 *                               certified with elsewhere (`joinWorkspace`)
 */
export async function connectPeer(options) {
  const {identity, custody, origin, store, profile} = options;
  if (custody) return connect(options);
  if (!identity) {
    // A device of a person whose root is elsewhere (a paired device): its
    // key is the profile's own, let go like an identity's below.
    if (store || !profile) return connect(options);
    const own = await openDeviceCustody({profile, storage: idbStorage()});
    return withCustody(own, () => connect({...options, custody: own}), options.signal);
  }
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
    // The page may connect again once this resolves: the device key is free by then.
    try { await dispose(); } finally { await custody.close(); }
  };
  signal?.addEventListener('abort', () => custody.close(), {once: true});
  return peer;
}

async function connect({
  origin, userPublicKey, label = 'Browser', workspace, custody, store, runtime, identity, deviceCertificate,
  profile, device, displayName = '',
  fetch: fetcher, desktop, signal, onStatus, onPairingCode, live: liveEnabled = true,
  voice: voiceOptions = {}, requireExistingCertificate = false, namespaces = globalThis.localStorage,
  workspaceName,
}) {
  userPublicKey ??= identity?.userPublicKey;
  // Only a peer in its profile's own namespace moves to a fresh one when the
  // host at its origin was set up again; a caller's store or namespace stays.
  const ownNamespace = device === undefined && !store;
  if (identity && userPublicKey !== identity.userPublicKey) {
    throw new TypeError('userPublicKey must be the unlocked identity\'s');
  }
  if (identity) {
    if (!store) profile ??= identityProfile(identity, origin);
    custody ??= await openDeviceCustody({profile: profile ?? identityProfile(identity, origin), storage: idbStorage()});
  }
  // A page may start connecting while its runtime still loads.
  runtime ??= await pageRuntime(desktop);
  if (!origin || !userPublicKey || !custody || !(store || profile) || !runtime) {
    throw new TypeError('origin, userPublicKey, custody, store or profile, and runtime are required');
  }
  signal?.throwIfAborted();
  const surfaceModule = async () => desktop ?? await import('./host.mjs');
  let storage;
  // Each device key of the profile (each tab open at once) is a device of
  // its own, with its own certificate, record and channel state.
  const slot = custody.slot ?? 0;
  device ??= profile ? profileDevice(profile, namespaces, slot) : PROFILE_DEVICE;
  if (profile) {
    const surface = await surfaceModule();
    if (typeof surface.openProfileStorage !== 'function') throw new Error('profile storage is unavailable');
    storage = await surface.openProfileStorage(profile, device);
    store ??= profileStore(runtime, profile, device);
  }
  // Seals the channel state the runtime keeps in the profile's storage, from
  // the platform secret store, as the session's own MLS vault keeps its key.
  const vaultOf = () => runtime.channelVault(`page:${profile}/${device}:${userPublicKey}`);
  let channelVault = storage ? await vaultOf() : undefined;
  const post = httpsPost(origin, fetcher);
  const custodyForWasm = custody.kind === 'sealed-seed' ? await custody.unseal() : custody;
  // The colour this person's presence names, the runtime's for who they are
  // (their user root): the same one a native desktop draws them in. A
  // desktop settles everyone here over the whole workspace instead, so people
  // here together never share one.
  const ownColor = () => runtime.presenceColor(userPublicKey);
  let host = JSON.parse(await runtime.hostInfo(post));
  if (!host.address) throw new Error('the host named no iroh endpoint to reach it at');
  const listeners = new Set();
  /** Called after each published status, so a surface follows the session
   * (the transport, the admission, the store) and not only its mount. */
  const statusListeners = new Set();
  const pendingRecordWaits = new Set();
  let peer;
  let live;
  let admitted;
  let ready;
  const resetReady = () => { ready = new Promise(resolve => { admitted = resolve; }); };
  resetReady();
  let actions;
  let voice;
  let disposed = false;
  let drawName = null;
  // When the person last changed their profile, as this page knows it: their
  // presence says it, so everyone here fetches it again when it moves.
  let profileMs = 0;
  const publishParticipant = () => live?.publishPresence({participant: {kind: 'person',
    label: (displayName || label).slice(0, 32), color: ownColor(), profile_ms: profileMs}});
  // The person's name, as history and presence show it from now on.
  const rename = name => {
    displayName = fitDisplayName(name);
    peer?.setDisplayName?.(displayName);
    publishParticipant();
  };
  let snapshot = {phase: 'authenticating', records: []};
  // Desktop surfaces mounted through this peer, by surface id: their failures
  // are reported into SessionHealth and recovered through `recover`.
  const surfaces = new Map();
  let surfaceCount = 0;

  const publish = () => {
    if (!peer) return;
    snapshot = JSON.parse(peer.snapshot());
    if (snapshot.phase === 'ready') admitted();
    if (live) {
      // The session's next frame carries both: the durable changes this page
      // reads, and what the members said (presence, pointers, previews).
      live.pump(JSON.parse(peer.takeDurableEvents()));
      live.pump(JSON.parse(peer.takeLiveEvents()));
    }
    voice?.pump(peer.takeVoiceEvents?.() ?? '[]');
    for (const listener of [...listeners]) listener(snapshot);
    onStatus?.(snapshot);
    for (const listener of [...statusListeners]) {
      try { listener(snapshot); } catch {}
    }
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
  const watchStorage = () => storage?.subscribe?.(status => { if (status.persistence === 'failed') reportStorage(status.error); });
  let unsubscribeStorage = watchStorage();
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
  /** Subscribe the workspaces anew; resolves once the host admitted them again. */
  async function refreshCheckpoint() {
    if (!workspace) { peer.refresh(); return; }
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

  /** Move this peer onto a fresh namespace of its profile for the host
   * `hostCert`, the one that just admitted this device. What the profile
   * kept for the previous host (its certificate, record, outbox and
   * documents) stays in its own namespace, untouched and never opened for
   * this host. */
  async function adoptHost(hostCert) {
    const fresh = hostNamespace(hostCert, slot);
    if (!ownNamespace || !storage || !fresh || fresh === device) throw new AdmissionError('host-changed', HOST_CHANGED);
    unsubscribeStorage?.();
    const previous = storage;
    storage = undefined;
    await previous.release();
    const surface = await surfaceModule();
    storage = await surface.openProfileStorage(profile, fresh);
    device = fresh;
    store = profileStore(runtime, profile, device);
    channelVault = await vaultOf();
    unsubscribeStorage = watchStorage();
    keepProfileDevice(profile, slot, device, namespaces);
  }

  async function certificate() {
    if (requireExistingCertificate && deviceCertificate) throw Error('Resource links require a previously certified device');
    let existing = await store.get(CERTIFICATE);
    if (requireExistingCertificate) {
      if (!existing) throw Error('Sign in on this browser and obtain admission before opening a resource link');
      return existing; // Host checks expiry/revocation; never renew or pair implicitly.
    }
    const admitted = deviceCertificate;
    deviceCertificate = undefined;
    const answering = admitted ? JSON.parse(admitted).hostCert : host.hostCert;
    if (existing && !sameHost(JSON.parse(existing).hostCert, answering)) {
      // What this browser kept was certified by, and its record is pinned
      // to, another host than the one answering at this origin now (it was
      // set up again). Kept state is never handed to it, nor renewed for it.
      // Only a device that host has just admitted (an invitation redeemed,
      // its door, a pairing) goes on, in a namespace of its own.
      if (!admitted) throw new AdmissionError('host-changed', HOST_CHANGED);
      await adoptHost(answering);
      existing = undefined;
    }
    if (admitted) {
      await store.put(CERTIFICATE, admitted);
      return admitted;
    }
    if (existing && !(identity && expiresWithinADay(existing))) return existing;
    const issued = identity
      ? await runtime.certifyWithRoot(post, identity, custodyForWasm, label)
      : await runtime.certifyDevice(userPublicKey, label, null, custodyForWasm, code => onPairingCode?.(code),
        JSON.stringify(host.address));
    await store.put(CERTIFICATE, issued);
    return issued;
  }

  async function start() {
    // Kept as text: the peer starts from the exact certificate, whose
    // nanosecond timestamps a JavaScript number would round.
    const certified = await certificate();
    if (!sameHost(JSON.parse(certified).hostCert, host.hostCert)) {
      // Certified by another host than the one answering at this origin
      // now: that host would refuse the session, and the state kept for the
      // other one is never handed to it.
      throw new AdmissionError('host-changed', HOST_CHANGED);
    }
    const record = storage ? undefined : await store.get(PEER_RECORD);
    peer = runtime.WasmPeer.start(
      withStartFields(certified, {
        displayName, address: host.address,
        // What this person's root revoked: announced with this device's
        // certificate, so the host refuses those keys from the moment each
        // entry names on.
        revocations: identity?.revocations?.length ? identity.revocations : undefined,
      }),
      custodyForWasm,
      storage ? JSON.stringify({profile, device}) : undefined,
      storage ? undefined : (record ?? undefined),
      storage ? undefined : bytes => { store.put(PEER_RECORD, bytes).catch(() => {}); },
      channelVault,
    );
    peer.subscribe(publish);
    if (workspace) {
      // A workspace of the person's own its host does not govern yet is placed
      // there under the workspace's name.
      peer.joinWorkspace(workspace, workspaceName);
      if (liveEnabled) {
        live = createLiveLayer(peer, runtime, workspace, {ready, participant: {
          kind: 'person', label: (displayName || label).slice(0, 32), color: ownColor(), profile_ms: profileMs,
        }});
      }
      actions?.dispose();
      const hostDevice = hexKey(host.hostCert?.device_public_key);
      // The host bridges this page into a call, so its frames carry the
      // host's device and it is the page's own voice there.
      voice ??= voiceOptions === false ? undefined : createPeerVoice({peer: () => peer, workspace, device: hostDevice, ...voiceOptions});
      actions = hostDevice ? createActions() : undefined;
    }
    publish();
  }
  function stopLive() {
    live?.stop();
  }
  // Every mount publishes through its own view of a layer, so the mount
  // acted in last speaks for the person (or the agent), never an idle one.
  const speakers = createSpeakers();
  function mountLive(target, options) {
    const speaker = speakers.mount();
    const layer = () => live;
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

  // The page's actions: the workspace's host lists and runs them, asked through the
  // session this page holds with it (`actionMessage` in wasm) — the same door the page's
  // own Rust asks, so nothing here speaks the frames. A ticket follows one invocation: the
  // host's receipt, then what became of it.
  function createActions() {
    const TERMINAL = new Set(['succeeded', 'failed', 'cancelled']);
    const entries = new Map();
    let correlation = 0;
    class ActionsError extends Error {
      constructor(code, message) { super(message); this.name = 'ActionsError'; this.code = code; }
    }
    /** The host's refusal `error`, as it travels, read as every caller reads it
     * (`hostRefusal` in wasm): its code, its words, and its typed refusal where
     * it gave one. */
    const failure = error => {
      const refused = JSON.parse(runtime.hostRefusal(JSON.stringify(error)));
      return Object.assign(new ActionsError(refused.code, refused.message), {refusal: refused.refusal ?? null});
    };

    /** Ask the workspace's host `message` (a `RemoteActionMessage`) and answer what it said. */
    async function ask(message) {
      try {
        return JSON.parse(await peer.actionMessage(workspace, userPublicKey, JSON.stringify(message)));
      } catch (error) {
        throw new ActionsError('unavailable', String(error?.message ?? error));
      }
    }

    /** The state an `InvocationUpdate` says, as a ticket reports it. */
    function stateOf(update) {
      const [variant, body] = Object.entries(update ?? {})[0] ?? [];
      if (!variant) return null;
      switch (variant) {
        case 'Accepted': return {state: 'accepted'};
        case 'Progress': return {state: 'progress', sequence: String(body.sequence), phase: body.phase,
          completed: body.completed ?? null, total: body.total ?? null, message: body.message ?? null};
        case 'Item': return {state: 'item', sequence: String(body.sequence), value: body.value};
        case 'Succeeded': return {state: 'succeeded', output: {value: body.output?.value, resources: body.output?.resources ?? []}};
        case 'Failed': return {state: 'failed', error: body.error};
        case 'Cancelled': return {state: 'cancelled', message: body.message ?? null};
        default: return null;
      }
    }

    /** The state a receipt says. */
    function receiptState(status) {
      if (status === 'Accepted') return {state: 'accepted'};
      if (status?.Completed) return {state: 'succeeded', output: {value: status.Completed.output?.value, resources: status.Completed.output?.resources ?? []}};
      if (status?.Failed) return {state: 'failed', error: status.Failed.error};
      if (status?.Cancelled) return {state: 'cancelled', message: status.Cancelled.message ?? null};
      return null;
    }

    function keep(id, state, extra = {}) {
      const held = entries.get(id) ?? {invocation: id};
      const entry = {...held, ...extra, ...(state ?? {}), invocation: id, update: state ?? held.update ?? null};
      entry.terminal = TERMINAL.has(entry.state);
      entry.updatedAt = Date.now();
      entries.set(id, entry);
      return entry;
    }

    function send(entry) {
      const message = {Invoke: {
        invocation: entry.invocation,
        target: entry.target,
        argument: entry.argument,
        ...(entry.confirmation ? {confirmation: entry.confirmation} : {}),
      }};
      return ask(message).then(answer => {
        const receipt = answer.Receipt;
        if (!receipt) {
          const state = keep(entry.invocation, {state: 'unknown', reason: 'unanswered'});
          throw Object.assign(new ActionsError('unavailable', 'the host did not answer'), {entry: state});
        }
        if (receipt.status?.Rejected) {
          const error = failure(receipt.status.Rejected.error);
          keep(entry.invocation, {state: 'failed', error: {code: error.code, message: error.message}},
            {reason: error.code});
          throw Object.assign(error, {entry: entries.get(entry.invocation)});
        }
        return keep(entry.invocation, receiptState(receipt.status));
      }, error => {
        // The outcome is unknown: the host may have run it. Keep the id so a
        // retry observes the original instead of running it again.
        keep(entry.invocation, {state: 'unknown', reason: error.code ?? 'unknown'});
        throw error;
      });
    }

    function ticket(entry, done) {
      return {
        id: entry.invocation,
        get entry() { return entries.get(entry.invocation); },
        done,
        status: () => status(entry.invocation),
        cancel: () => cancel(entry.invocation),
      };
    }

    /** List what the host offers; `query` is a `CatalogueQuery` (`{provider, needle, …}`). */
    async function catalogue(query = {}) {
      correlation += 1;
      const answer = await ask({CatalogueRequest: {correlation, query: {max_results: 200, ...query}}});
      const result = answer.CatalogueResponse?.result;
      if (result?.Ok) return result.Ok;
      if (!result?.Err) throw new ActionsError('unavailable', 'the host did not answer');
      throw failure(result.Err);
    }

    /** Run one action; `target` is `{provider, address: {resource, action}}` from a catalogue row. */
    function invoke(target, argument, {confirmation = null, invocation, label} = {}) {
      const resource = target?.address?.resource;
      const recordsOwn = resource === '' && String(target?.provider).startsWith('record:');
      if (!target?.provider || !(resource || recordsOwn) || !target?.address?.action) {
        throw new TypeError('target is {provider, address: {resource, action}} copied from a catalogue row');
      }
      const id = invocation ?? globalThis.crypto.randomUUID();
      const existing = entries.get(id);
      if (existing) return ticket(existing, existing.terminal ? Promise.resolve(existing) : send(existing));
      const entry = keep(id, {state: 'unknown'}, {
        target: {provider: target.provider, address: {resource: target.address.resource, action: target.address.action}},
        argument: invokeArgument(argument), confirmation: wireConfirmation(confirmation), label: label ?? null,
      });
      return ticket(entry, send(entry));
    }

    async function status(id) {
      const held = entries.get(id);
      if (!held) throw new ActionsError('unknown_invocation', 'no such invocation in this page');
      const answer = await ask({Observe: {invocation: id}});
      const update = answer.Update?.state?.update;
      if (update) return keep(id, stateOf(update));
      // The host never admitted it (lost on the way, or refused and forgotten): send it again
      // under the same id.
      if (held.state === 'unknown') return send(held).catch(() => entries.get(id));
      return entries.get(id);
    }

    async function cancel(id) {
      const held = entries.get(id);
      if (!held) throw new ActionsError('unknown_invocation', 'no such invocation in this page');
      if (held.terminal) return held;
      const result = (await ask({Cancel: {invocation: id}})).CancelResponse?.result;
      if (result && !('Ok' in result)) throw failure(result.Err);
      keep(id, null, {cancellationRequested: true});
      return status(id);
    }

    return {
      catalogue,
      invoke,
      status,
      cancel,
      dispose() { entries.clear(); },
    };
  }

  try {
    await start();
  } catch (error) {
    await storage?.release().catch(() => {});
    throw error;
  }

  // Leaving the page leaves the workspace's live plane at once: closing its
  // sessions withdraws this page's announcement, so everyone else takes its
  // pointer and face down now, not when the link times out, and its host
  // session ends with it. A page kept to come back to (`persisted`) is frozen,
  // not gone.
  const page = typeof globalThis.addEventListener === 'function' ? globalThis : null;
  const leavePage = event => {
    if (event?.persisted) return;
    stopLive();
  };
  page?.addEventListener('pagehide', leavePage);
  // Back online, the session reconnects now rather than when its backoff
  // (up to half a minute after a long outage) runs out: a call waits on it
  // for its fresh grant.
  const online = () => { if (!disposed) { try { peer?.retryNow?.(); } catch {} } };
  page?.addEventListener('online', online);

  let disposal;
  function dispose() {
    if (disposal) return disposal;
    page?.removeEventListener('pagehide', leavePage);
    page?.removeEventListener('online', online);
    let resolve, reject;
    disposal = new Promise((done, failed) => { resolve = done; reject = failed; });
    disposed = true;
    // Abort and explicit close can overlap. Every caller joins the same
    // settlement, installed before teardown can call back into dispose.
    (async () => {
      for (const refuse of [...pendingRecordWaits]) refuse(new Error('Shared resource connection closed'));
      for (const entry of [...surfaces.values()]) dropSurface(entry);
      unsubscribeStorage?.();
      actions?.dispose();
      voice?.dispose();
      stopLive();
      peer?.dispose();
      listeners.clear();
      statusListeners.clear();
      await storage?.release();
    })().then(resolve, reject);
    return disposal;
  }
  signal?.addEventListener('abort', dispose, {once: true});

  /** One workspace request, in the shape the page draws (`rooms.rs`). */
  const rooms = async (op, args) => JSON.parse(await peer.rooms(op, JSON.stringify(args ?? {})));
  const membership = {
    createInvitation: ({role = 'editor', expiresIn = 7 * 86_400, waitListEntry = null} = {}) =>
      rooms('invite', {workspace, role, expiresInSeconds: Math.floor(expiresIn), inviterName: displayName,
        ...(waitListEntry ? {waitListEntry} : {})}),
    preAdmit: options => {
      if (!identity) throw new Error('pre-admitting needs the owner\'s unlocked identity');
      return peer.preAdmit(identity, JSON.stringify(options));
    },
    list: () => rooms('list', {workspace}),
    waitList: () => rooms('waitList', {workspace}),
    unlist: requestId => rooms('unlist', {workspace, requestId}),
    mailInvite: (requestId, link) => rooms('mailInvite', {workspace, requestId, link: String(link)}),
    setRole: (userPublicKey, role) => rooms('setRole', {workspace, userPublicKey, role}),
    remove: userPublicKey => rooms('remove', {workspace, userPublicKey}),
    ban: userPublicKey => rooms('ban', {workspace, userPublicKey}),
    unban: userPublicKey => rooms('unban', {workspace, userPublicKey}),
    cancelInvitation: inviteId => rooms('cancel', {workspace, inviteId}),
  };
  const governed = {
    list: () => rooms('workspaces'),
    create: name => {
      if (typeof name !== 'string' || !name.trim()) throw new TypeError('a workspace needs a name');
      return rooms('create', {name: name.trim()});
    },
    enter: room => rooms('enter', {workspace: room}),
  };
  const certifiedPeer = {
    get host() { return host; },
    /** The host origin and workspace this peer is in, as a link names them; the
     * workspace is undefined for a peer that joined none. */
    origin: new URL(origin).origin,
    workspace,
    /** The person (user root, hex) this device is certified as. */
    userPublicKey,
    /** The person's name, as history and presence show it from now on. */
    get displayName() { return displayName; },
    /** The workspace's name as its people last set it (the workspace in front on
     * its shared desktop), once the workspace delivered it; else `undefined`. */
    get workspaceName() {
      // Asked after a failure too, when the session may be gone.
      try { return peer?.workspaceName?.() ?? undefined; } catch { return undefined; }
    },
    setDisplayName: name => rename(name),
    /** Issue an invitation to the workspace (`rooms.rs`): resolves to its
     * text, when the person's role lets them invite. */
    createInvitation: options => membership.createInvitation(options),
    /** The person's own workspace on this host, with this very device in it
     * (the shared device entry, `enterOwnWorkspace` in wasm): made the first
     * time, answered again after, another for each `name`. `entered(id)`
     * says this browser is in that workspace already, so it is not redeemed
     * again. Resolves to the workspace's listing `{workspace, workspace_id,
     * name}`; rejects with the host's refusal as it typed it (`HostRefusal`:
     * `code`, and `refusal` where the host gave one). */
    enterOwnWorkspace: async ({name = null, entered = () => false} = {}) => JSON.parse(
      await peer.enterOwnWorkspace(workspace, post, name, fitDisplayName(displayName || label), entered)),
    /** Who is in the workspace, for its owner (`rooms.rs`): `list()`,
     * `setRole`, `remove`, `cancelInvitation`. */
    get membership() { return membership; },
    /** The workspaces the host governs (`rooms.rs`): `list()`,
     * `create(name)`, `enter(workspace)`; needs no workspace. */
    workspaces: governed,
    getSnapshot: () => snapshot,
    /** How many changes this page made to the workspace's records and sent:
     * above zero once the person posted or created something. */
    authoredChanges: () => peer?.authoredChanges?.() ?? 0,
    /** Diagnostic text of the page's channel membership (not a contract). */
    channelDiagnostics: () => peer?.channelDiagnostics?.() ?? '',
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    /** Read the session's own status after every change to it (`onStatus`),
     * so a page can follow it rather than only its first snapshot. */
    onStatus(listener) { statusListeners.add(listener); return () => statusListeners.delete(listener); },
    /** The live layer of the joined workspace, or `undefined` without a workspace. */
    get live() { return live; },
    /** Why this page cannot join the workspace's live plane at all, in the
     * words shown in the members' place, or `null` while it can
     * (or did): a plane that could not be reached. */
    liveNote: () => { try { return peer?.liveNote?.() ?? null; } catch { return null; } },
    /** This person as their presence names them: `{label, color}`. */
    get participant() {
      return {label: (displayName || label).slice(0, 32), color: ownColor()};
    },
    /** The person's profile was kept (or read) as of `at` (Unix ms): their
     * presence says so, and everyone here fetches it again. */
    setProfileChanged(at) {
      const stamp = Number(at) || 0;
      if (stamp <= profileMs) return;
      profileMs = stamp;
      publishParticipant();
    },
    /** The people here as their live presence names them (`[{user,
     * label}]`): someone else already going by this person's drawn name
     * makes them take another, so the workspace and what it is bridged with
     * tell them apart. */
    membersSeen(people) {
      if (!workspace || !Array.isArray(people)) return;
      const redrawn = drawName?.(displayName, people);
      if (redrawn && redrawn !== displayName) rename(redrawn);
    },
    /** Who draws this person's name again when it collides in the workspace:
     * `(current, people) => name | null`, or `null` once they chose one. */
    setNameDrawer(draw) { drawName = typeof draw === 'function' ? draw : null; },
    /** The host's fetch proxy for this page's apps (`fetch-proxy.mjs`):
     * `{endpoint, session}` while the host serves one over the current
     * session, else `null`. Read for each request: the session follows
     * reconnects. */
    fetchProxy() {
      const session = disposed ? undefined : peer?.fetchProxySession?.();
      return session ? {endpoint: new URL(FETCH_PROXY_PATH, origin).href, session} : null;
    },
    /** The workspace's app actions (`actions.mjs`), or `undefined` without a workspace. */
    get actions() { return actions; },
    /** Bind feedback before mounting the desktop. The destination only selects
     * an existing text channel: Rust obtains identity and current capabilities
     * from the authenticated owner. This creates no membership or grant. */
    async configureFeedback(destination) {
      if (!workspace || destination?.room !== workspace || disposed) throw new Error('Feedback needs this existing workspace');
      await certifiedPeer.whenRecord('so.tilo.chat', '', {signal});
      if (typeof peer?.configureFeedback !== 'function') throw new Error('Feedback delivery is unavailable in this runtime');
      peer.configureFeedback(JSON.stringify(destination));
      await untilSnapshot(() => {
        if (disposed) throw new Error('Feedback connection closed');
        const status = peer.feedbackStatus();
        if (status === 'ready') return true;
        if (status !== 'awaiting authenticated admission') throw new Error(status);
        return false;
      }, 30_000, 'Feedback owner admission timed out');
    },
    feedbackStatus: () => peer?.feedbackStatus?.() ?? 'unconfigured',
    /** Send this workspace's reports through `carrier`, the page's peer in the
     * workspace whose channel takes them (a web host's home workspace): reports
     * reach it from whichever workspace the page is in. */
    sendFeedbackThrough(carrier) {
      const id = carrier?.runtimeId;
      if (disposed || typeof id !== 'number') throw new Error('Feedback needs a connected peer to send through');
      if (typeof peer?.sendFeedbackThrough !== 'function') throw new Error('Feedback delivery is unavailable in this runtime');
      peer.sendFeedbackThrough(id);
    },

    /** Public address of this admitted session; it conveys no authority. */
    get resourceScope() {
      if (disposed || snapshot.phase !== 'ready' || !actions || !workspace) return undefined;
      return {origin: new URL(origin).origin, workspace,
        hostDevice: hexKey(host.hostCert?.device_public_key)};
    },
    /** Fresh permission-filtered metadata for a record-local shared resource.
     * The host-issued source provider is required; this never joins a workspace,
     * grants access, follows a peer or navigates as a side effect. */
    resolveResource(reference, provider) {
      const connection = actions;
      return resolveResourceReference({reference, provider, actions: connection,
        scope: certifiedPeer.resourceScope,
        current: () => !disposed && snapshot.phase === 'ready' && actions === connection});
    },
    /** Open an exact shared resource through a standalone app's current view
     * door. Calling this is explicit navigation; it never joins or grants. */
    openResource(target, reference, provider, action, {signal} = {}) {
      const connection = actions;
      return openResourceReference({reference, provider, action, actions: connection, signal,
        scope: certifiedPeer.resourceScope,
        current: () => !disposed && snapshot.phase === 'ready' && actions === connection,
        waitRecord: (app, record) => certifiedPeer.whenRecord(app, record, {signal}),
        mount: ({app, record}) => certifiedPeer.mountDesktop(target, {app, record, signal})});
    },
    /** The workspace's calls (`voice-peer.mjs`), or `undefined` without a workspace. */
    get voice() { return voice; },
    /** This peer in the wasm runtime, for mounts that replicate through it. */
    get runtimeId() { return peer?.id; },
    /** The desktop mount (`host.mjs`) in this peer's profile and workspace.
     * `options.surfaceId` names the surface in SessionHealth failures and
     * `retry-desktop`/`retry-asset` requests (default `surface-N`); the
     * mount carries it as `surfaceId`. */
    async mountDesktop(target, options = {}) {
      const surface = await surfaceModule();
      if (typeof surface.mountDesktop !== 'function') throw new Error('desktop mount is unavailable');
      const scope = {...(profile ? {profile, device} : {}), ...(workspace ? {workspace} : {})};
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
    /** Resolves once the host admitted the workspace (`phase === 'ready'`);
     * rejects when it refused this device. */
    whenReady({signal, timeoutMs = 30_000} = {}) {
      return new Promise((resolve, reject) => {
        let timer;
        const finish = (error, value) => {
          clearTimeout(timer); listeners.delete(check); pendingRecordWaits.delete(finish);
          signal?.removeEventListener('abort', abort);
          if (error) reject(error); else resolve(value);
        };
        const abort = () => finish(signal.reason ?? new DOMException('Aborted', 'AbortError'));
        const check = current => {
          if (disposed) finish(new Error('Shared resource connection closed'));
          else if (current.phase === 'ready') finish(undefined, current);
          else if (current.phase === 'failed') finish(new Error(current.error ?? 'the host refused this device'));
        };
        if (signal?.aborted) { abort(); return; }
        // Not a refusal: the host has not answered yet.
        timer = setTimeout(() => finish(new DOMException('Workspace admission timed out', 'TimeoutError')), timeoutMs);
        listeners.add(check); pendingRecordWaits.add(finish);
        signal?.addEventListener('abort', abort, {once: true});
        check(snapshot);
      });
    },
    /** The workspace's records this page holds, open in a mount or received:
     * `[{app, record}]` (a package's workspace document has record `''`). */
    records: () => JSON.parse(peer?.workspaceRecords?.() ?? '[]'),
    /** Whether this page holds the workspace's record `record` of `app`. */
    hasRecord,
    /** Resolves once the workspace's record `record` of `app` reached this page. */
    whenRecord(app, record, {timeoutMs = 30_000, signal} = {}) {
      return new Promise((resolve, reject) => {
        if (signal?.aborted) { reject(signal.reason); return; }
        if (disposed) { reject(new Error('Shared resource connection closed')); return; }
        if (hasRecord(app, record)) { resolve(); return; }
        const source = peer;
        const finish = (error, failed = error !== undefined) => {
          clearTimeout(timer); listeners.delete(check); pendingRecordWaits.delete(finish);
          signal?.removeEventListener('abort', abort);
          if (failed) reject(error); else resolve();
        };
        const abort = () => finish(signal.reason, true);
        const check = () => {
          if (peer !== source || snapshot.phase === 'failed') { finish(new Error('Shared resource connection changed')); return; }
          if (hasRecord(app, record)) finish();
        };
        // `no-record`: nothing arrived in time, which for a record nobody
        // wrote yet (an app's workspace document before its first write) is
        // the answer, not a failure of the connection.
        const timer = setTimeout(() => finish(Object.assign(new Error(`the workspace sent no ${app} record ${record}`), {code: 'no-record'})), timeoutMs);
        listeners.add(check); pendingRecordWaits.add(finish);
        signal?.addEventListener('abort', abort, {once: true});
      });
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
        if (requireExistingCertificate) throw Error('Sign in through the identity flow before reopening this resource');
        // What the page learnt of the host at load may be stale (a new
        // certificate). A host set up again is not certified for here: the
        // record is pinned to the one before, so the page loads again and
        // joins the new host as the same person (`joinViaHost`), with the
        // kept certificate left as it is.
        const answering = JSON.parse(await runtime.hostInfo(post));
        const kept = await store.get(CERTIFICATE);
        if (kept && !sameHost(JSON.parse(kept).hostCert, answering.hostCert)) {
          if (!globalThis.location) throw new AdmissionError('host-changed', HOST_CHANGED);
          globalThis.location.reload();
          return {operationId: 'reauthenticate'};
        }
        voice?.reset();
        stopLive();
        peer.dispose();
        resetReady();
        await store.delete(CERTIFICATE);
        host = answering;
        await start();
        return {operationId: 'reauthenticate'};
      }
      throw new TypeError(`unsupported recovery ${request?.kind}`);
    },
    /** Report the page store's durability of a record to SessionHealth. */
    setLocalState: (app, workspace, record, state) => peer.setLocalState(app, workspace, record, state),
    dispose,
  };
  return certifiedPeer;
}

/** What a person is told when the host at an origin is no longer the one
 * this browser's kept state belongs to. */
export const HOST_CHANGED = 'This workspace was reset since this browser last opened it. Reload the page to join it again.';
/** Whether two host certificates name the same host (its person and device),
 * as the peer's saved record pins it; unknown when either is missing. */
const sameHost = (kept, current) => !kept || !current
  || (hexKey(kept.user_public_key) === hexKey(current.user_public_key)
    && hexKey(kept.device_public_key) === hexKey(current.device_public_key));

/** What a person is told when the host will not honour an invitation. */
export const UNUSABLE_INVITATION = 'This invitation can’t be used: it was used already, expired or was withdrawn. Ask whoever sent it for a new one.';
/** What a person is told when the host refuses to admit them with one. */
export const REFUSED_JOIN = 'The workspace did not let you in. If someone sent you an invitation, ask them for a new one.';

const invitationBytes = invitation => {
  if (typeof invitation !== 'string') return Uint8Array.from(invitation);
  const text = invitation.trim().replace(/-/g, '+').replace(/_/g, '/');
  return Uint8Array.from(atob(text + '='.repeat((4 - text.length % 4) % 4)), char => char.charCodeAt(0));
};

/**
 * What an invitation (its signed bytes, or them as base64/base64url) offers
 * at the host `relayUrl`: `{workspaceName, role, workspace, workspaceId,
 * expiresAtNs, members, inviter}`. `members` is how many people the workspace
 * has, so the invitation says what someone joins, not only where. `inviter`
 * is `{name, userPublicKey}` as the host signed it into the invitation, or
 * undefined when it names nobody.
 */
export async function inspectInvitation(relayUrl, invitation, {runtime, fetch: fetcher} = {}) {
  runtime ??= await pageRuntime();
  const reply = JSON.parse(await runtime.inspectInvitation(httpsPost(relayUrl, fetcher), invitationBytes(invitation)));
  if (reply.kind !== 'invitation') {
    // The host read the invitation and will not honour it: spent, expired,
    // withdrawn or not its own. Its reason is for the console.
    console.warn('invitation refused:', reply.reason ?? reply.kind);
    throw new AdmissionError('invitation-unusable', UNUSABLE_INVITATION);
  }
  return {
    workspaceName: reply.name, role: reply.role.toLowerCase(), room: reply.room, workspaceId: reply.workspaceId,
    expiresAtNs: reply.expiresAtNs, members: reply.members ?? null, inviter: reply.inviter ?? undefined,
  };
}

/**
 * The invitation a link's `invite` secret names, as `inspectInvitation` and
 * `joinWorkspace` take one: fetched from the shelf of `site` — the site that
 * keeps it, which the link names — and opened with the key the link carries.
 * Resolves to `{invitation, host}`, the invitation's own text and, where the
 * device that left it named one, the origin of the host it admits into: a
 * link names the site that keeps the invitation, not the host it admits
 * into. A secret that is an invitation's own text is answered as it came,
 * with no host, so links of every build join the same way.
 */
export async function openInvitation(site, secret, {runtime} = {}) {
  runtime ??= await pageRuntime();
  return JSON.parse(await runtime.redeemInvitation(site, secret));
}

/**
 * Become a new device of the person `userPublicKey` (hex) at the host at
 * `origin`, with no root on this page: the host relays the request, this
 * page shows `onPairingCode(code)`, and one of the person's devices that
 * holds their root approves it. Then, with `admission` (resolving to the
 * host's `{invitation, admissionToken, origin?}` for the person), the
 * device joins that workspace as a device of theirs; without, it connects
 * to `workspace`. Takes `connectPeer`'s other options; the device key is
 * `custody`, else a free one of `profile` (default the person's profile for
 * that host). Resolves to the `CertifiedPeer`.
 */
export async function pairDevice(options) {
  const {origin, userPublicKey, label = 'Browser', admission, signal} = options;
  if (!origin || !userPublicKey) throw new TypeError('origin and userPublicKey are required');
  const profile = options.profile ?? identityProfile({userPublicKey}, origin);
  // A caller's device key (`options.custody`, of `profile`) stays the
  // caller's to let go.
  const own = options.custody ?? await openDeviceCustody({profile, storage: idbStorage()});
  const pairing = async () => {
    const runtime = options.runtime ?? await pageRuntime(options.desktop);
    const custodyForWasm = own.kind === 'sealed-seed' ? await own.unseal() : own;
    const info = JSON.parse(await runtime.hostInfo(httpsPost(origin, options.fetch)));
    if (!info.address) throw new Error('the host named no iroh endpoint to reach it at');
    const certified = await runtime.certifyDevice(userPublicKey, label,
      options.invite ?? null, custodyForWasm, code => options.onPairingCode?.(code),
      JSON.stringify(info.address));
    const common = {...options, identity: undefined, label, profile, custody: own, runtime};
    if (!admission) return connect({...common, deviceCertificate: certified});
    const offer = await admission();
    return join({...common, origin: offer.origin ?? origin, invitation: offer.invitation, admissionToken: offer.admissionToken, certified});
  };
  return options.custody ? pairing() : withCustody(own, pairing, signal);
}

const hexOf = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');

/**
 * Join the workspace `invitation` (as `inspectInvitation` takes) offers at
 * `origin`, as `identity` (default the current one): this page's device for
 * that host is certified by the person's own root, and the invitation is the
 * approval of whoever made it, so the host admits at once. Resolves to the
 * `CertifiedPeer` in the workspace; rejects when the host refuses. With
 * `admissionToken` (the one-time token of an owner's pre-admission for this
 * invitation) the host admits the page at once too. The entry itself is the
 * shared one every device takes (`acceptInvitation` in wasm).
 */
export async function joinWorkspace(options) {
  const identity = options.identity ?? currentIdentity();
  if (!identity || options.custody || !options.origin) return join({...options, identity});
  // As connectPeer: the device key opened here is the peer's.
  const profile = options.profile ?? identityProfile(identity, options.origin);
  const own = await openDeviceCustody({profile, storage: idbStorage()});
  return withCustody(own, () => join({...options, identity, profile, custody: own}), options.signal);
}

/**
 * The certificate (`{cert, hostCert}` JSON text) this profile's device kept
 * from an earlier entry, or `null`: read from `store`, else from the namespace
 * the profile keeps its peer in now. Whether it still serves to enter with
 * (this very key, this person, the same host, not ending soon) is the shared
 * entry's to decide (`acceptInvitation`).
 */
async function keptDeviceCertificate({runtime, profile, slot = 0, store, desktop, namespaces = globalThis.localStorage}) {
  let opened;
  try {
    if (!store) {
      const surface = desktop ?? await import('./host.mjs');
      if (!profile || typeof surface.openProfileStorage !== 'function') return null;
      const device = profileDevice(profile, namespaces, slot);
      opened = await surface.openProfileStorage(profile, device);
      store = profileStore(runtime, profile, device);
    }
    return (await store.get(CERTIFICATE)) ?? null;
  } catch (error) {
    console.warn('join: the device\'s certificate could not be read', error);
    return null;
  } finally {
    await opened?.release?.();
  }
}

async function join({
  origin, invitation, identity, displayName = '', label = 'Browser', signal, onProgress,
  admissionToken, runtime, custody, profile, fetch: fetcher, certified, ...connectOptions
}) {
  // A device certified elsewhere (`certified`, a paired device) asks as
  // itself; any other asks as the person whose root the page holds.
  if (!certified) identity ??= currentIdentity();
  if (!identity && !certified) throw new IdentityError('locked', 'Unlock the identity first');
  if (!origin || !invitation) throw new TypeError('origin and invitation are required');
  runtime ??= await pageRuntime(connectOptions.desktop);
  const person = identity ?? {userPublicKey: hexOf(JSON.parse(certified).cert.user_public_key)};
  profile ??= identityProfile(person, origin);
  custody ??= await openDeviceCustody({profile, storage: idbStorage()});
  const custodyForWasm = custody.kind === 'sealed-seed' ? await custody.unseal() : custody;
  signal?.throwIfAborted();
  onProgress?.({phase: 'asking'});
  const held = certified ?? await keptDeviceCertificate({runtime, profile, slot: custody.slot, store: connectOptions.store,
    desktop: connectOptions.desktop, namespaces: connectOptions.namespaces});
  // The shared entry chooses the certificate (the one this very key holds
  // from this host, else one the root makes now) and has the workspace admit
  // this device under it.
  let accepted;
  try {
    accepted = await runtime.acceptInvitation(httpsPost(origin, fetcher), invitationBytes(invitation), custodyForWasm,
      identity ?? undefined, JSON.stringify({held, label: fitDisplayName(label), name: fitDisplayName(displayName || label),
        ...(admissionToken ? {admissionToken} : {})}));
  } catch (error) {
    console.warn('join refused:', error?.code ?? error?.name, error?.message ?? error);
    throw Object.assign(new AdmissionError('refused', REFUSED_JOIN), {reason: error?.message ?? String(error), code: 'refused'});
  }
  const {admission} = JSON.parse(accepted);
  onProgress?.({phase: 'admitted', room: admission.workspace, workspaceName: admission.name, role: admission.role.toLowerCase()});
  return connect({
    ...connectOptions, origin, identity, userPublicKey: person.userPublicKey, workspace: admission.workspace, label, displayName, custody, runtime, signal,
    profile: connectOptions.store ? undefined : profile,
    // The certificate goes on as the text the entry wrote it as: its
    // nanosecond timestamps exceed what a JavaScript number holds exactly
    // (json-text.mjs).
    fetch: fetcher, deviceCertificate: memberText(accepted, 'certificate'),
  });
}

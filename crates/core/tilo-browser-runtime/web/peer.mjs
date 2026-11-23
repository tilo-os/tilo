/**
 * `connectPeer` → `CertifiedPeer`: the page as a certified Tilo peer of the
 * user's host. Before it holds a certificate the page asks the host's HTTPS
 * origin only what it cannot ask a session: the host's certificate and
 * address, joining a workspace, and a member's approval of their own
 * device. Everything after goes over one iroh session to the host, through
 * the packet relay the host names (a page holds no UDP socket), the same
 * session a native device holds (`tilo-session-client` in wasm):
 *
 * - durable documents: signed commits from a persisted outbox, acknowledged
 *   in host order, replayed after every reconnect;
 * - device certification: the page shows the pairing code while a
 *   root-holding device of the user approves;
 * - the live session (`peer.live`): this page as one member device of the
 *   workspace beside every other, its presence, pointers, previews and shell
 *   on the same session link they use. Never authority;
 * - the workspace's calls (`peer.voice`, `voice-peer.mjs`): the session holds
 *   the participation and seals the page's frames, which reach the call as
 *   turns on its host's call media queue (`voice.mjs`).
 *
 * The page holds one device key (`device-key.mjs`): each tab is a
 * device of its own, with its own key, certificate and state. A page that also
 * holds the user root (`identity.mjs`, `options.identity`) certifies
 * that key itself instead of waiting for an approver.
 * `getSnapshot()` returns `PeerSnapshot` with `SessionHealth` v1.
 */
import {createPeerVoice} from './voice-peer.mjs';
import {hex} from './hex.mjs';
import {memberText} from './json-text.mjs';
import {changed as pageChanged} from './changes.mjs';
import {AdmissionError} from './admission.mjs';
import {idbStorage, openDeviceKey, rememberBrowserDevice} from './device-key.mjs';
import {identityProfile} from './identity-profile.mjs';
import {currentIdentity, fitDisplayName, IdentityError} from './identity.mjs';
import {siteServer, STALE_SITE_SERVER, staleSiteServer, staleSiteServerReport} from './site-host.mjs';
import {deviceLabel} from './pairing.mjs';

export {
  browserIdentity, browserPerson, certifyDevice, currentIdentity, fetchProfile, fitDisplayName, IdentityError, installNode,
  installNodeCertificate, MAX_DISPLAY_NAME_BYTES, nodeInstallInfo, publishProfile, storagePersisted,
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
 * device key slot, `device-key.mjs`: one per tab open at once) is a device
 * of its own with a namespace of its own: slot 0 keeps this one, slot `n`
 * the same name with `#n` after it. */
export const PROFILE_DEVICE = 'tilo-browser-device';
/** The name of a namespace for the device key slot `slot`. */
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
/** The namespace the peer of `profile`'s device key slot `slot` keeps its
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
 * Replace the device key `device key` holds for `profile` with a new one, in a
 * namespace of its own: a new device of the same person. For a key a host
 * knows under a certificate this browser no longer holds, which that host
 * never admits again under another. What was kept for the old key stays in
 * its namespace, never opened for the new one. Resolves to the new device key.
 */
export async function renewDevice(deviceKey, profile, {
  open = name => openDeviceKey({profile: name, storage: idbStorage()}), namespaces = globalThis.localStorage,
  random = bytes => crypto.getRandomValues(bytes),
} = {}) {
  await deviceKey.destroy();
  const renewed = await open(profile);
  // Another tab may have taken the freed slot meanwhile: a key that is not
  // new keeps the namespace it has.
  if (renewed.created) keepProfileDevice(profile, renewed.slot ?? 0, forSlot(`${PROFILE_DEVICE}:${hexKey(random(new Uint8Array(16)))}`, renewed.slot ?? 0), namespaces);
  return renewed;
}
/** The namespace a profile keeps a server's state in once that server
 * replaced the one it kept state for: named after the server's device key, so
 * it never shares an entry with another server's. When that namespace is
 * already occupied by another device identity, the device key or a fresh
 * suffix keeps them apart. */
const hostNamespace = (serverCert, index, cert = undefined, current = undefined) => {
  const key = hexKey(serverCert?.device_public_key);
  if (!/^[0-9a-f]{64}$/.test(key)) return undefined;
  const pick = prefix => forSlot(prefix, index);
  const base = pick(`${PROFILE_DEVICE}:${key}`);
  if (base !== current) return base;
  const deviceKey = hexKey(cert?.device_public_key);
  if (/^[0-9a-f]{64}$/.test(deviceKey)) {
    const byDevice = pick(`${PROFILE_DEVICE}:${key}:${deviceKey}`);
    if (byDevice !== current) return byDevice;
  }
  return pick(`${PROFILE_DEVICE}:${key}:${hexKey(random(new Uint8Array(16)))}`);
};
/** The wasm runtime, loaded through the page host. */
const pageRuntime = async desktop => (desktop ?? await import('./host.mjs')).loadRuntime?.();

const REFRESH_TIMEOUT_MS = 30_000;

/** The `FailureCode` of a failed durable commit. */
export function storageFailureCode(error) {
  const text = String(error ?? '');
  if (/QuotaExceeded/i.test(text)) return 'storage-quota';
  if (/SecurityError/i.test(text)) return 'storage-denied';
  return 'storage-transaction-failed';
}
// Differs from `hex` in input type: accepts hex strings and nullish values in addition to bytes.
const hexKey = key => (typeof key === 'string' ? key : hex(key ?? []));

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
/** A host served over plain HTTP on this machine (`tilo-server workspace --web-plain`). */
const isLoopbackHttp = url => url.protocol === 'http:' && LOOPBACK.has(url.hostname);

/** `post(path, body)` over `fetch` to the host origin: https, or loopback
 * http for local review (no certificate exception needed). The request names
 * no content type: the host reads every body as the bytes it is, and a
 * cross-origin POST with only the browser's own headers goes without the
 * CORS preflight, which cost a page entering through another origin's entry a
 * round trip before each request it made of the host. */
export function httpsPost(origin, fetcher = (...args) => globalThis.fetch(...args)) {
  const base = new URL(origin);
  const secure = base.protocol === 'https:' || isLoopbackHttp(base);
  if (!secure || base.pathname !== '/' || base.search || base.hash) {
    throw new TypeError('host origin must be an https (or loopback http) origin');
  }
  return async (path, body) => {
    const response = await fetcher(new URL(path, base).href, {
      method: 'POST',
      body, credentials: 'omit', cache: 'no-store', redirect: 'error',
    });
    return {status: response.status, retryAfter: response.headers?.get?.('retry-after') ?? undefined,
      body: new Uint8Array(await response.arrayBuffer())};
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
  member, ready = Promise.resolve(),
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
    if (member) value.member = {kind: 'person', profile_ms: 0, ...member};
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
 * @param options.device key        from `openDeviceKey`
 * @param options.profile        profile whose durable storage keeps the peer's
 *                               certificates and outbox (with `device`)
 * @param options.device         device namespace of `profile`
 * @param options.store          `{get, put, delete}` for public peer state;
 *                               defaults to the profile's storage
 * @param options.displayName    the person's name for history and presence
 * @param options.runtime        the wasm module (`certifyDevice`, `WasmPeer`, live codec);
 *                               defaults to the page host's (`desktop`'s)
 * @param options.fetch          defaults to `globalThis.fetch`
 * @param options.desktop        `{loadRuntime, openProfileStorage}`; defaults to `host.mjs`
 * @param options.live           `false`: join no live session
 * @param options.onPairingCode  shown while an approver decides
 * @param options.identity       an unlocked root (`identity.mjs`): the
 *                               page certifies its own device with it, no
 *                               approver; `userPublicKey` defaults to its key,
 *                               `profile` to one per identity and `device key` to
 *                               that profile's device key
 *                               (without one, `device key` defaults to `profile`'s
 *                               device key, certified by an approver)
 * @param options.deviceCertificate  `{cert, hostCert}` JSON this device was
 *                               certified with elsewhere (`joinWorkspace`)
 */
export async function connectPeer(options) {
  const {identity, deviceKey, origin, store, profile} = options;
  if (deviceKey) return connect(options);
  if (!identity) {
    // A device of a person whose root is elsewhere (a paired device): its
    // key is the profile's own, let go like an identity's below.
    if (store || !profile) return connect(options);
    const own = await openDeviceKey({profile, storage: idbStorage()});
    return withDeviceKey(own, () => connect({...options, deviceKey: own}), options.signal);
  }
  // The device key this call opens is the peer's: it is let go when the
  // peer is disposed or fails to connect, so the page can connect again.
  const own = await openDeviceKey({profile: profile ?? identityProfile(identity, origin), storage: idbStorage()});
  return withDeviceKey(own, () => connect({...options, deviceKey: own, ...(store ? {} : {profile: profile ?? identityProfile(identity, origin)})}),
    options.signal);
}

/** Runs `connecting` with the device key `device key`, closing it when the
 * resulting peer is disposed, when `signal` aborts, or when connecting fails. */
export async function withDeviceKey(deviceKey, connecting, signal) {
  let peer;
  try {
    peer = await connecting();
  } catch (error) {
    deviceKey.close();
    throw error;
  }
  const dispose = peer.dispose;
  peer.dispose = async () => {
    // The page may connect again once this resolves: the device key is free by then.
    try { await dispose(); } finally { await deviceKey.close(); }
  };
  signal?.addEventListener('abort', () => deviceKey.close(), {once: true});
  return peer;
}

async function connect({
  origin, userPublicKey, label = deviceLabel(), workspace, deviceKey, store, runtime, identity, deviceCertificate,
  profile, device, displayName = '',
  fetch: fetcher, desktop, signal, onStatus, onPairingCode, live: liveEnabled = true,
  voice: voiceOptions = {}, requireExistingCertificate = false, namespaces = globalThis.localStorage,
  workspaceName, hostInfo: answered, entering,
}) {
  userPublicKey ??= identity?.userPublicKey;
  // A person who gave no name goes by none: never their device's label.
  displayName ??= '';
  // Only a peer in its profile's own namespace moves to a fresh one when the
  // host at its origin was set up again; a caller's store or namespace stays.
  const ownNamespace = device === undefined && !store;
  if (identity && userPublicKey !== identity.userPublicKey) {
    throw new TypeError('userPublicKey must be the unlocked identity\'s');
  }
  if (identity) {
    if (!store) profile ??= identityProfile(identity, origin);
    deviceKey ??= await openDeviceKey({profile: profile ?? identityProfile(identity, origin), storage: idbStorage()});
  }
  // A page may start connecting while its runtime still loads.
  runtime ??= await pageRuntime(desktop);
  if (!origin || !userPublicKey || !deviceKey || !(store || profile) || !runtime) {
    throw new TypeError('origin, userPublicKey, device key, store or profile, and runtime are required');
  }
  signal?.throwIfAborted();
  const surfaceModule = async () => desktop ?? await import('./host.mjs');
  let storage;
  // Each device key of the profile (each tab open at once) is a device of
  // its own, with its own certificate, record and channel state.
  const slot = deviceKey.slot ?? 0;
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
  const deviceKeyForWasm = deviceKey.kind === 'sealed-seed' ? await deviceKey.unseal() : deviceKey;
  /** How this device comes back to the host answering as `hostCert` (JSON
   * text), with what it `kept` and what an entry just `admitted` it under. */
  const reconnecting = (kept, admitted, hostCert) =>
    runtime.reconnectCertificate(kept ?? undefined, admitted ?? undefined, hostCert, Boolean(identity));
  // The colour this person's presence names, the runtime's for who they are
  // (their user root): the same one a native desktop draws them in. A
  // desktop settles everyone here over the whole workspace instead, so people
  // here together never share one.
  const ownColor = () => runtime.presenceColor(userPublicKey);
  // Who the host is and where it is dialled: as the page's own site names
  // it, handed on by the entry that took it (`enterOnSession`), else asked
  // of the host here. An answer from an entry says where the host's packet
  // relay answers and nothing of how a page reaches the host itself (the
  // WebTransport origin, and the certificate a page pins when the host's is
  // not trusted by the browser): the site's own naming of the host has
  // that, and the host is the same one, so it is kept.
  const named = siteServer(origin)?.info;
  let host = answered?.address ? answered : named ?? JSON.parse(await runtime.hostInfo(post));
  if (answered?.address && !answered.address.web && named?.address?.web) {
    host = {...answered, address: {...answered.address, web: named.address.web}};
  }
  if (!host.address) throw new Error('the host named no iroh endpoint to reach it at');
  const listeners = new Set();
  /** Called after each published status, so a surface follows the session
   * (the transport, the admission, the store) and not only its mount. */
  const statusListeners = new Set();
  const pendingRecordWaits = new Set();
  let peer;
  /** The id the page's mounts know it by, kept while a session under it is
   * replaced (`recover({kind: 'reauthenticate'})`). */
  let pageId;
  let live;
  let admitted;
  let ready;
  const resetReady = () => { ready = new Promise(resolve => { admitted = resolve; }); };
  resetReady();
  let actions;
  let voice;
  let disposed = false;
  // When the person last changed their profile, as this page knows it: their
  // presence says it, so everyone here fetches it again when it moves.
  let profileMs = 0;
  const publishMember = () => live?.publishPresence({member: {kind: 'person',
    label: displayName.slice(0, 32), color: ownColor(), profile_ms: profileMs}});
  // The person's name, as history and presence show it from now on.
  const rename = name => {
    displayName = fitDisplayName(name);
    peer?.setDisplayName?.(displayName);
    publishMember();
  };
  let snapshot = {phase: 'authenticating', records: []};
  /** The workspace's name as its host answered admission with it
   * ([`certifiedPeer.noteWorkspaceName`]), kept until the workspace's own
   * shared desktop names it again. */
  let admittedName = null;
  // A session dialling the host as the site named it, which it has not
  // reached yet, asks the host who it is once it fails or takes long: a host
  // set up again since answers as another, and the page stops there with the
  // site's deployment to fix (`site-host.mjs`) rather than wait on a host
  // nobody answers as, or go on with one the site does not name.
  let reached = host !== named;
  let checking = null;
  let stale = null;
  // A session whose first try to connect failed before it ever got through
  // is a host this page cannot reach: a refused or dropped connection says
  // so at once, and the page says so then (`hostUnreached`)
  // instead of waiting out a timeout. The session goes on trying; once it
  // gets through, it is a session like any other. A host the site named is
  // first asked who it is (`siteChecked`), which tells a host set up again
  // from a host down.
  let connectedOnce = false;
  let unreached = false;
  let siteChecked = false;
  const checked = () => {
    siteChecked = true;
    if (!disposed) publish();
  };
  const checkSiteServer = () => {
    if (reached || checking || stale || siteChecked || disposed) return;
    checking = runtime.hostInfo(post).then(text => {
      if (reached || disposed) return;
      if (!staleSiteServer(host, JSON.parse(text))) { checked(); return; }
      const report = staleSiteServerReport(origin);
      console.error(report);
      // The site's entry logs what a page did not catch (`reportErrors` in entry-page.mjs).
      globalThis.reportError?.(new Error(report));
      stale = new AdmissionError('stale-entry', STALE_SITE_SERVER);
      publish();
    }, () => {
      // Its web address does not answer either: the host is down.
      checking = null;
      checked();
    });
  };
  let siteServerCheck;
  const publish = () => {
    if (!peer) return;
    snapshot = JSON.parse(peer.snapshot());
    if (!reached) {
      const transport = snapshot.health?.transport?.phase;
      if (transport === 'connected') {
        reached = true;
        clearTimeout(siteServerCheck);
      } else if (stale) {
        snapshot = {...snapshot, phase: 'failed', error: stale.message, errorCode: stale.code};
      } else if (['reconnecting', 'offline', 'failed'].includes(transport)) {
        checkSiteServer();
      }
    }
    if (snapshot.health?.transport?.phase === 'connected') connectedOnce = true;
    if (!connectedOnce && !stale) {
      if ((reached || siteChecked) && snapshot.health?.transport?.phase === 'offline') unreached = true;
      if (unreached) snapshot = {...snapshot, ...hostUnreached(origin, snapshot)};
    }
    // Once in, a session started again (a recovery) is a member's and enters no more.
    if (snapshot.entry?.outcome === 'admitted') entering = undefined;
    if (snapshot.phase === 'ready') admitted();
    if (live) {
      // The session's next frame carries both: the durable changes this page
      // reads, and what the members said (presence, pointers, previews).
      live.pump(JSON.parse(peer.takeDurableEvents()));
      live.pump(JSON.parse(peer.takeLiveEvents()));
    }
    voice?.pump(peer.takeVoiceEvents?.() ?? '[]');
    announce();
  };
  /** Tell everyone who follows the page's state what it is now. */
  const announce = () => {
    for (const listener of [...listeners]) listener(snapshot);
    onStatus?.(snapshot);
    for (const listener of [...statusListeners]) {
      try { listener(snapshot); } catch {}
    }
    // Whoever waits on the page's state reads it again now (`changes.mjs`).
    pageChanged();
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
  /** Commit the store's failed writes again. */
  async function retrySaving() {
    await storage?.flush();
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

  /** Move this peer onto a fresh namespace of its profile for the server
   * `serverCert`, the one that just admitted this device. What the profile
   * kept for the previous server or device (its certificate, peer entry,
   * outbox and documents) stays in its own namespace, untouched and never
   * opened for this session. */
  async function adoptHost(serverCert, cert = undefined) {
    const fresh = hostNamespace(serverCert, slot, cert, device);
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

  /** A certificate for this device made now, for the server the page reaches:
   * by the person's root where the page holds it, whoever that server belongs to (the
   * person's own, or one that keeps a workspace they were invited into), else
   * by one of their devices that holds it (pairing). */
  async function certifyNow() {
    return identity
      ? await runtime.certifyWithRoot(identity, deviceKeyForWasm, label, JSON.stringify(host.hostCert))
      : await runtime.certifyDevice(userPublicKey, label, null, deviceKeyForWasm, code => onPairingCode?.(code),
        JSON.stringify(host.address));
  }

  async function certificate() {
    if (requireExistingCertificate && deviceCertificate) throw Error('Resource links require a previously certified device');
    let existing = await store.get(CERTIFICATE);
    if (requireExistingCertificate) {
      if (!existing) throw Error('Sign in on this browser and obtain admission before opening a resource link');
      return existing; // Server checks expiry/revocation; never renew or pair implicitly.
    }
    const admitted = deviceCertificate;
    deviceCertificate = undefined;
    const saved = storage && typeof runtime.savedPeerCertificate === 'function'
      ? runtime.savedPeerCertificate(profile, device, userPublicKey)
      : undefined;
    const serverCert = host.hostCert;
    const certServer = raw => JSON.parse(raw).hostCert ?? serverCert;
    const adopt = raw => {
      const parsed = JSON.parse(raw);
      return adoptHost(parsed.hostCert ?? serverCert, parsed.cert);
    };
    const answering = JSON.stringify(admitted ? certServer(admitted) : serverCert);
    // The one rule every device keeps as it comes back (`reconnectCertificate`
    // in wasm): what this browser kept is never handed to another server than
    // the one it was certified by, and only a device that server has just
    // admitted (an invitation redeemed, its entry, a pairing) goes on there,
    // in a namespace of its own. The saved peer entry is checked beside the
    // kept certificate so a stale entry never blocks a newly admitted session.
    const decision = reconnecting(existing, admitted, answering);
    const savedDecision = saved ? reconnecting(saved, admitted, answering) : null;
    if (!admitted && (decision === 'host-changed' || savedDecision === 'host-changed')) {
      throw new AdmissionError('host-changed', HOST_CHANGED);
    }
    if (admitted) {
      if (decision === 'adopt-host' || savedDecision === 'adopt-host') await adopt(admitted);
      await store.put(CERTIFICATE, admitted);
      return admitted;
    }
    if (decision === 'kept') {
      if (saved && reconnecting(saved, existing, answering) === 'adopt-host') {
        await adopt(existing);
        await store.put(CERTIFICATE, existing);
      }
      return existing;
    }
    const issued = await certifyNow();
    if (saved && reconnecting(saved, issued, JSON.stringify(certServer(issued))) === 'adopt-host') {
      await adopt(issued);
    }
    await store.put(CERTIFICATE, issued);
    return issued;
  }

  /** Start the page's session. `keep` is the page this one replaces, when the
   * page certifies its device again: its mounts name it by one id for as long
   * as it is open, and every one of them follows the session under it. */
  async function start(keep = undefined) {
    // Kept as text: the peer starts from the exact certificate, whose
    // nanosecond timestamps a JavaScript number would round.
    const certified = await certificate();
    if (reconnecting(certified, undefined, JSON.stringify(host.hostCert)) === 'host-changed') {
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
        // The workspace this device enters through its entry as its session opens, until
        // the host admitted it.
        entering,
      }),
      deviceKeyForWasm,
      storage ? JSON.stringify({profile, device}) : undefined,
      storage ? undefined : (record ?? undefined),
      storage ? undefined : bytes => { store.put(PEER_RECORD, bytes).catch(() => {}); },
      channelVault,
      keep,
    );
    pageId = peer.id;
    // Settings lists this browser's tabs' device keys as one entry.
    if (profile) rememberBrowserDevice(certified, namespaces);
    peer.subscribe(publish);
    if (!reached) siteServerCheck ??= setTimeout(checkSiteServer, SITE_HOST_CHECK_MS);
    if (workspace) {
      // A workspace of the person's own its server does not hold yet is
      // placed there under the workspace's name.
      peer.joinWorkspace(workspace, workspaceName);
      if (liveEnabled) {
        live = createLiveLayer(peer, runtime, workspace, {ready, member: {
          kind: 'person', label: displayName.slice(0, 32), color: ownColor(), profile_ms: profileMs,
        }});
      }
      actions?.dispose();
      const hostDevice = hexKey(host.hostCert?.device_public_key);
      // The host bridges this page into a call, so its frames carry the
      // host's device and it is the page's own voice there.
      voice ??= voiceOptions === false ? undefined : createPeerVoice({peer: () => peer, workspace, device: hostDevice, person: userPublicKey, ...voiceOptions});
      actions = hostDevice ? createActions() : undefined;
    }
    publish();
  }
  function stopLive() {
    live?.stop();
  }

  // The page's actions: the workspace's host lists and runs them, asked through the
  // session this page holds with it (`actionMessage` in wasm) — the same entry the page's
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

    /** `target` as it travels: `{provider, address: {resource, action}}` copied from a catalogue row. */
    function wireTarget(target) {
      const resource = target?.address?.resource;
      const recordsOwn = resource === '' && String(target?.provider).startsWith('record:');
      if (!target?.provider || !(resource || recordsOwn) || !target?.address?.action) {
        throw new TypeError('target is {provider, address: {resource, action}} copied from a catalogue row');
      }
      return {provider: target.provider, address: {resource: target.address.resource, action: target.address.action}};
    }

    /** Run one action; `target` is `{provider, address: {resource, action}}` from a catalogue row. */
    function invoke(target, argument, {confirmation = null, invocation, label} = {}) {
      const wired = wireTarget(target);
      const id = invocation ?? globalThis.crypto.randomUUID();
      const existing = entries.get(id);
      if (existing) return ticket(existing, existing.terminal ? Promise.resolve(existing) : send(existing));
      const entry = keep(id, {state: 'unknown'}, {
        target: wired,
        argument: invokeArgument(argument), confirmation: wireConfirmation(confirmation), label: label ?? null,
      });
      return ticket(entry, send(entry));
    }

    /**
     * Run one action and answer what the host said of it, `{invocation, state, output?, error?}`,
     * keeping nothing of the call in this page: not its argument, which may be what a person
     * typed into a protected field. A refusal throws as `invoke`'s does. One the host accepted
     * and still runs is followed with `observe`; a caller that must not run it twice names the
     * `invocation` itself and asks again under the same id.
     */
    async function once(target, argument, {confirmation = null, invocation} = {}) {
      const id = invocation ?? globalThis.crypto.randomUUID();
      const confirmed = wireConfirmation(confirmation);
      const answer = await ask({Invoke: {invocation: id, target: wireTarget(target), argument: invokeArgument(argument),
        ...(confirmed ? {confirmation: confirmed} : {})}});
      const receipt = answer.Receipt;
      if (!receipt) throw new ActionsError('unavailable', 'the host did not answer');
      if (receipt.status?.Rejected) throw failure(receipt.status.Rejected.error);
      return {invocation: id, ...(receiptState(receipt.status) ?? {state: 'unknown'})};
    }

    /** What became of an invocation the host accepted, asked of the host and kept nowhere here. */
    async function observe(id) {
      const update = (await ask({Observe: {invocation: id}})).Update?.state?.update;
      return {invocation: id, ...(update ? stateOf(update) : {state: 'unknown'})};
    }

    /**
     * Which of the workspace's documents changed after `after`, a cursor an earlier answer gave:
     * `{changed: [{app, document}], cursor}`, each document once, from the server's own feed of its
     * documents' changes (`ChangesRequest`), which answers this person with what they may read.
     * Only the names leave this function: what a change says (its sentence, who made it, what
     * it was given) is read by nobody here. With no cursor nothing that happened yet is news:
     * the answer names nothing and gives the cursor to read on from.
     */
    async function changes(after = null) {
      correlation += 1;
      const answer = await ask({ChangesRequest: {correlation, ...(typeof after === 'string' && after ? {after} : {})}});
      const result = answer.ChangesResponse?.result;
      if (!result?.Ok) {
        if (!result?.Err) throw new ActionsError('unavailable', 'the server did not answer');
        throw failure(result.Err);
      }
      const named = new Map();
      for (const change of result.Ok.changes ?? []) {
        const {app, record: document} = change ?? {};
        if (typeof app !== 'string' || typeof document !== 'string') continue;
        named.set(`${app}\n${document}`, {app, document});
      }
      return {changed: [...named.values()], cursor: String(result.Ok.cursor ?? '')};
    }

    /**
     * Wait for the server's word that a write landed in the workspace's documents, `withinMs` at
     * most: `{woke, release}`. `woke` resolves `true` at that word and `false` once the time
     * is over, never sooner: a server that cannot tell, or never answers, costs the whole wait
     * and nothing else. `release()` ends the wait now. The word says that something was
     * written, never what or by whom.
     */
    function landed(withinMs) {
      let over;
      const until = new Promise(resolve => { over = resolve; });
      const timer = setTimeout(over, Math.max(0, Number(withinMs) || 0));
      // The session's own wait ends with `until`; one that never settles is not waited for.
      const heard = Promise.resolve().then(() => peer.writeLanded(workspace, until)).then(said => said === true, () => false);
      const woke = Promise.race([heard, until.then(() => false)])
        .then(async said => { if (!said) await until; return said; });
      return {woke, release() { clearTimeout(timer); over(); }};
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
      once,
      observe,
      changes,
      landed,
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

  // Leaving the page leaves the workspace's live session at once: closing its
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
    clearTimeout(siteServerCheck);
    // Abort and explicit close can overlap. Every caller joins the same
    // settlement, installed before teardown can call back into dispose.
    (async () => {
      for (const refuse of [...pendingRecordWaits]) refuse(new Error('Shared resource connection closed'));
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

  /** One workspace request, in the shape the page draws (`workspaces.rs`). */
  const ask = async (op, args) => JSON.parse(await peer.workspaces(op, JSON.stringify(args ?? {})));
  const membership = {
    createInvitation: ({role = 'editor', expiresIn = 7 * 86_400, waitListEntry = null} = {}) =>
      ask('invite', {workspace, role, expiresInSeconds: Math.floor(expiresIn), inviterName: displayName,
        ...(waitListEntry ? {waitListEntry} : {})}),
    preAdmit: options => {
      if (!identity) throw new Error('pre-admitting needs the owner\'s unlocked identity');
      return peer.preAdmit(identity, JSON.stringify(options));
    },
    list: () => ask('list', {workspace}),
    mayInvite: () => ask('mayInvite', {workspace}),
    waitList: () => ask('waitList', {workspace}),
    unlist: requestId => ask('unlist', {workspace, requestId}),
    mailInvite: (requestId, link) => ask('mailInvite', {workspace, requestId, link: String(link)}),
    setRole: (userPublicKey, role) => ask('setRole', {workspace, userPublicKey, role}),
    remove: userPublicKey => ask('remove', {workspace, userPublicKey}),
    ban: userPublicKey => ask('ban', {workspace, userPublicKey}),
    unban: userPublicKey => ask('unban', {workspace, userPublicKey}),
    cancelInvitation: inviteId => ask('cancel', {workspace, inviteId}),
  };
  const governed = {
    list: () => ask('workspaces'),
    create: name => {
      if (typeof name !== 'string' || !name.trim()) throw new TypeError('a workspace needs a name');
      return ask('create', {name: name.trim()});
    },
    /** A workspace made under `workspace` (a UUID the page chose) and named
     * `name`, this device admitted to it as its owner: asked again with the
     * same id it answers the one already made, never a second. */
    place: ({workspace, name}) => {
      if (typeof workspace !== 'string' || !workspace) throw new TypeError('a placed workspace needs an id');
      if (typeof name !== 'string' || !name.trim()) throw new TypeError('a workspace needs a name');
      return ask('place', {workspace, name: name.trim()});
    },
    enter: workspace => ask('enter', {workspace}),
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
    /** The workspace's name as its host answered this page's admission with:
     * the name the tab is titled after and the shell's first space is given,
     * for a workspace the page read in no directory — one a link's invitation
     * admits into, or the person's own. */
    noteWorkspaceName(name) {
      admittedName = typeof name === 'string' && name.trim() ? name.trim() : null;
    },
    /** The name the workspace's host answered admission with, else `undefined`. */
    get workspaceName() { return admittedName ?? undefined; },
    /** The name the page shows this person by from now on: the one they gave
     * themselves, or one drawn from their key while they give none. It stays
     * the page's: only `nameSelf` tells the workspace a name. */
    setDisplayName: name => rename(name),
    /** The person named themselves `name`: their server keeps it as theirs and
     * names them by it in every workspace it keeps. Never a name the page drew. */
    nameSelf: name => {
      const given = fitDisplayName(name);
      if (given) peer?.nameSelf?.(given);
    },
    /** Issue an invitation to the workspace (`workspaces.rs`): resolves to its
     * text, when the person's role lets them invite. */
    createInvitation: options => membership.createInvitation(options),
    /** This person's new devices waiting at the host, paired with the root
     * the page holds over this session (`PairingHost`: `pending`, `answer`). */
    pairing: () => peer.pairing(),
    /** The person's own workspace on this host, with this very device in it
     * (the shared device entry, `enterOwnWorkspace` in wasm): made the first
     * time under an id the host chooses, answered again after, another for
     * each `name`. `entered(id)` says this browser is in that workspace already,
     * so it is not redeemed again. `invocation` (a UUID) is the caller's own id
     * for this make, kept when the same make is asked again after no answer: the
     * server answers the workspace the first ask made rather than making another.
     * Resolves to the workspace's listing
     * `{workspace, workspace_id, name}`; rejects with the host's refusal as it
     * typed it (`DeviceRefusal`: `code`, and `refusal` where the host gave one). */
    enterOwnWorkspace: async ({name = null, entered = () => false, invocation = null} = {}) => JSON.parse(
      await peer.enterOwnWorkspace(name, fitDisplayName(displayName), entered, invocation)),
    /** Who is in the workspace, for its owner (`workspaces.rs`): `list()`,
     * `setRole`, `remove`, `cancelInvitation`. */
    get membership() { return membership; },
    /** The workspaces the host governs (`workspaces.rs`): `list()`,
     * `create(name)`, `enter(workspace)`; needs no workspace. */
    workspaces: governed,
    /** The person's workspaces as their host lists them (`workspaces.rs`,
     * `personal`): `[{workspace, name, role?, host?}]`, each naming its host
     * where this one does not keep it. What the workspace switcher offers
     * besides the workspaces this browser remembered, each saying when it was
     * made so two of one name are told apart. */
    personalWorkspaces: async () => (await ask('personal')).workspaces ?? [],
    getSnapshot: () => snapshot,
    /** Diagnostic text of the page's channel membership (not a contract). */
    channelDiagnostics: () => peer?.channelDiagnostics?.() ?? '',
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    /** Read the session's own status after every change to it (`onStatus`),
     * so a page can follow it rather than only its first snapshot. */
    onStatus(listener) { statusListeners.add(listener); return () => statusListeners.delete(listener); },
    /** The live layer of the joined workspace, or `undefined` without a workspace. */
    get live() { return live; },
    /** Why this page cannot join the workspace's live session at all, in the
     * words shown in the members' place, or `null` while it can
     * (or did): a live session that could not be reached. */
    liveNote: () => { try { return peer?.liveNote?.() ?? null; } catch { return null; } },
    /** This person as their presence names them: `{label, color}`. */
    get member() {
      return {label: displayName.slice(0, 32), color: ownColor()};
    },
    /** The person's profile was kept (or read) as of `at` (Unix ms): their
     * presence says so, and everyone here fetches it again. */
    setProfileChanged(at) {
      const stamp = Number(at) || 0;
      if (stamp <= profileMs) return;
      profileMs = stamp;
      publishMember();
    },
    /** The workspace's app actions (`actions.mjs`), or `undefined` without a workspace. */
    get actions() { return actions; },
    /** Public address of this admitted session; it conveys no authority. */
    get resourceScope() {
      if (disposed || snapshot.phase !== 'ready' || !actions || !workspace) return undefined;
      return {origin: new URL(origin).origin, workspace,
        hostDevice: hexKey(host.hostCert?.device_public_key)};
    },
    /** The workspace's calls (`voice-peer.mjs`), or `undefined` without a workspace. */
    get voice() { return voice; },
    /** This peer in the wasm runtime, for mounts that replicate through it. */
    get runtimeId() { return peer?.id ?? pageId; },
    /** Resolves once the host admitted the workspace (`phase === 'ready'`);
     * rejects when it refused this device.
     *
     * A host that says nothing for `timeoutMs` has not answered. One that
     * is still getting the page there (the session moving on to admitting)
     * is waited for, up to `capMs` in all: a busy host is no unreachable
     * host. One that refuses the page, or a session that never reached the
     * host (`hostUnreached`), is said as soon as the session says so. */
    whenReady({signal, timeoutMs = 30_000, capMs = 120_000} = {}) {
      return new Promise((resolve, reject) => {
        let timer;
        let done = false;
        let progress = null;
        const began = Date.now();
        const finish = (error, value) => {
          done = true;
          clearTimeout(timer); listeners.delete(check); pendingRecordWaits.delete(finish);
          signal?.removeEventListener('abort', abort);
          if (error) reject(error); else resolve(value);
        };
        const abort = () => finish(signal.reason ?? new DOMException('Aborted', 'AbortError'));
        // Not a refusal: the host has not let the page in yet. A session
        // that never got through to the host by then is a host that did not
        // answer (`unanswered`, as `hostUnreached` says it sooner), which the
        // page asks again; one the host answered and never let in is
        // `timed-out`. What the session last said goes with it.
        const timedOut = () => {
          const connected = snapshot.health?.transport?.phase === 'connected';
          finish(Object.assign(new Error('Workspace admission timed out'), {
            name: 'TimeoutError', code: connected ? 'timed-out' : 'unanswered', phase: snapshot.phase ?? null,
            sessionError: snapshot.error ?? null, sessionErrorCode: snapshot.errorCode ?? null, connected}));
        };
        const check = current => {
          if (disposed) finish(new Error('Shared resource connection closed'));
          else if (current.phase === 'ready') finish(undefined, current);
          // The session stopped: its code says why (a host never reached,
          // `hostUnreached`; the host's refusal, `not-admitted`,
          // `device-revoked`, `workspace-deleted` and the rest), for the page
          // to say that, not that it stopped.
          else if (current.phase === 'failed') {
            finish(Object.assign(new Error(current.error ?? 'the host refused this device'),
              current.errorCode ? {code: current.errorCode, sessionError: current.error ?? null, sessionErrorCode: current.errorCode} : {}));
          }
          // The host answered the page's subscription with a no: the session
          // asks no more, so nothing comes of waiting on.
          else if (current.phase === 'admitting' && current.errorCode === 'permission-required') {
            finish(Object.assign(new Error(`the host did not let this tab in: ${current.error ?? 'refused'}`), {
              code: 'not-let-in', phase: current.phase, sessionError: current.error ?? null,
              sessionErrorCode: current.errorCode, connected: true}));
          }
          if (done) return;
          // Each step the host takes gives it `timeoutMs` more, within `capMs`.
          if (current.phase === progress) return;
          progress = current.phase;
          clearTimeout(timer);
          timer = setTimeout(timedOut, Math.max(0, Math.min(timeoutMs, began + capMs - Date.now())));
        };
        if (signal?.aborted) { abort(); return; }
        listeners.add(check); pendingRecordWaits.add(finish);
        signal?.addEventListener('abort', abort, {once: true});
        check(snapshot);
      });
    },
    /** Resolves once every published change is durable on the host and the
     * peer's own state is durable in the profile. */
    flush: async () => { await peer.flush(); await storage?.flush(); },
    async reconnect() { peer.reconnect(); },
    /** `SessionHealth` v1 recovery: `retry-connection`, `reauthenticate`,
     * `retry-saving` or `refresh-checkpoint`. Resolves once the recovery
     * took effect. */
    async recover(request) {
      if (request?.kind === 'retry-connection') { peer.reconnect(); return {operationId: 'retry-connection'}; }
      if (request?.kind === 'retry-saving') { await retrySaving(); return {operationId: 'retry-saving'}; }
      if (request?.kind === 'refresh-checkpoint') { await refreshCheckpoint(); return {operationId: 'refresh-checkpoint'}; }
      if (request?.kind === 'reauthenticate') {
        if (requireExistingCertificate) throw Error('Sign in through the identity flow before reopening this resource');
        // What the page learnt of the host at load may be stale (a new
        // certificate). A host set up again is not certified for here: the
        // record is pinned to the one before, so the page loads again and
        // joins the new host as the same person (`joinViaHost`), with the
        // kept certificate left as it is.
        const answering = siteServer(origin)?.info ?? JSON.parse(await runtime.hostInfo(post));
        const kept = await store.get(CERTIFICATE);
        if (reconnecting(kept, undefined, JSON.stringify(answering.hostCert)) === 'host-changed') {
          if (!globalThis.location) throw new AdmissionError('host-changed', HOST_CHANGED);
          globalThis.location.reload();
          return {operationId: 'reauthenticate'};
        }
        host = answering;
        // The new certificate first, while the session it replaces still
        // stands: a certification that fails leaves the page as it was, its
        // session, and what that session says of itself, untouched.
        const issued = await certifyNow();
        voice?.reset();
        stopLive();
        // The page stays the same page while the session under it is replaced:
        // its mounts' windows, documents and server all name it by this id.
        const page = peer?.id ?? pageId;
        peer?.dispose();
        // Until the new session says otherwise, the page holds none: it is
        // never "ready" on a session it let go.
        peer = undefined;
        snapshot = {phase: 'authenticating'};
        announce();
        resetReady();
        try {
          await store.put(CERTIFICATE, issued);
          await start(page);
        } catch (error) {
          snapshot = notConnected(error);
          announce();
          throw error;
        }
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
  return invitationOffer(reply);
}

/**
 * The server `invitation` admits into, as the invitation itself names it (`invitedServer` in
 * wasm, checked under its signature): `{info, endpoint, offer}`, `info` as the server's `/host` answers,
 * the server's endpoint id, and what the invitation offers as `inspectInvitation` answers it. A
 * server reached only over iroh has no web origin to ask any of it of.
 */
export async function invitedServer(invitation, {runtime} = {}) {
  runtime ??= await pageRuntime();
  if (typeof runtime.invitedServer !== 'function') return null;
  const named = JSON.parse(await runtime.invitedServer(invitationBytes(invitation)));
  return {info: named.info, endpoint: named.endpoint, offer: invitationOffer(named.offer)};
}

/** What an invitation offers, as `inspectInvitation` answers it, from its `reply` (the server's
 * answer, or the invitation's own word for a server reached only over iroh: `invitedServer`). */
export function invitationOffer(reply) {
  return {
    // An administrator's invitation gives the Editor role and makes them an
    // administrator, an owner's an administrator the workspace is kept for:
    // the card names what they become.
    workspaceName: reply.name ?? '',
    role: reply.owner ? 'owner' : reply.administrator ? 'administrator' : String(reply.role ?? '').toLowerCase(),
    workspace: reply.workspace, workspaceId: reply.workspaceId,
    expiresAtNs: reply.expiresAtNs, members: reply.members ?? null, inviter: reply.inviter ?? undefined,
    // Sent to someone who left their address at the workspace's entry: a
    // person new here, who opens a workspace of their own first.
    waitList: reply.waitList === true,
  };
}

/**
 * The invitation a link's `invite` secret names, as `inspectInvitation` and
 * `joinWorkspace` take one: the invitation `workspace` keeps at its host
 * `origin`, which answers with it for the secret a short link carries.
 * Resolves to the invitation's own text. A secret that is an invitation's own
 * text is answered as it came, so every link joins the same way.
 */
export async function openInvitation(origin, workspace, secret, {runtime} = {}) {
  runtime ??= await pageRuntime();
  return runtime.openInvitation(origin, workspace ?? '', secret);
}

/**
 * Become a new device of the person `userPublicKey` (hex) at the host at
 * `origin`, with no root on this page: the host relays the request, this
 * page shows `onPairingCode(code)`, and one of the person's devices that
 * holds their root approves it. Then, with `entry` (a workspace's entry, its
 * base URL), the device goes through it into that workspace as a device of
 * theirs; without, it connects to `workspace`. Takes `connectPeer`'s other options; the device key is
 * `device key`, else a free one of `profile` (default the person's profile for
 * that host). Resolves to the `CertifiedPeer`.
 */
export async function pairDevice(options) {
  const {origin, userPublicKey, label = deviceLabel(), entry, signal} = options;
  if (!origin || !userPublicKey) throw new TypeError('origin and userPublicKey are required');
  const profile = options.profile ?? identityProfile({userPublicKey}, origin);
  // A caller's device key (`options.device key`, of `profile`) stays the
  // caller's to let go.
  const own = options.deviceKey ?? await openDeviceKey({profile, storage: idbStorage()});
  const pairing = async () => {
    const runtime = options.runtime ?? await pageRuntime(options.desktop);
    const deviceKeyForWasm = own.kind === 'sealed-seed' ? await own.unseal() : own;
    const info = siteServer(origin)?.info ?? JSON.parse(await runtime.hostInfo(httpsPost(origin, options.fetch)));
    if (!info.address) throw new Error('the host named no iroh endpoint to reach it at');
    // The host names this device: the name asked for, or the next numbered one
    // when another of the person's devices reads by it already.
    const certified = await runtime.certifyDevice(userPublicKey, label, options.invite ?? null, deviceKeyForWasm,
      code => options.onPairingCode?.(code), JSON.stringify(info.address));
    const enrolled = JSON.parse(certified).label ?? label;
    const common = {...options, identity: undefined, label: enrolled, profile, deviceKey: own, runtime};
    if (!entry) return connect({...common, deviceCertificate: certified});
    return join({...common, origin, entry, certified});
  };
  return options.deviceKey ? pairing() : withDeviceKey(own, pairing, signal);
}

/**
 * Join the workspace `invitation` (as `inspectInvitation` takes) offers at
 * `origin`, as `identity` (default the current one): this page's device for
 * that host is certified by the person's own root, and the invitation is the
 * approval of whoever made it, so the host admits at once. Resolves to the
 * `CertifiedPeer` in the workspace; rejects when the host refuses. With
 * `admissionToken` (the one-time token of an owner's pre-admission for this
 * invitation) the host admits the page at once too. The entry itself is the
 * shared one every device takes, on the session itself (`invitationEntry` in wasm).
 */
export async function joinWorkspace(options) {
  const identity = options.identity ?? currentIdentity();
  if (!identity || options.deviceKey || !options.origin) return join({...options, identity});
  // As connectPeer: the device key opened here is the peer's.
  const profile = options.profile ?? identityProfile(identity, options.origin);
  const own = await openDeviceKey({profile, storage: idbStorage()});
  return withDeviceKey(own, () => join({...options, identity, profile, deviceKey: own}), options.signal);
}

/**
 * The certificate (`{cert, hostCert}` JSON text) this profile's device kept
 * from an earlier entry, or `null`: read from `store`, else from the namespace
 * the profile keeps its peer in now. Whether it still serves to enter with
 * (this very key, this person, the same host, not ending soon) is the shared
 * entry's to decide (`invitationEntry`, `entryRequest`).
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

/** What a session that never got through to the host at `origin` says in
 * place of its own phase: `failed`, `unanswered` (the code an entry not
 * reached ends an entry with), and words that name the host and read as a
 * connection that failed (`failure-screen.mjs`, `unanswered`). */
function hostUnreached(origin, snapshot) {
  const why = snapshot?.error ? `: ${snapshot.error}` : '';
  return {phase: 'failed', errorCode: 'unanswered', error: `connection refused or lost before ${new URL(origin).origin} answered${why}`};
}

/** What a page says of itself once it let its session go and could not start
 * the one that was to replace it: `failed`, `not-connected`, in words a person
 * reads, with the cause beside them for the console. */
export function notConnected(error) {
  return {
    phase: 'failed', errorCode: 'not-connected',
    error: 'This browser isn’t connected to the workspace. Reload the page to connect again.',
    cause: String(error?.message ?? error ?? ''),
  };
}

/** The codes an entry refuses with (`EntryRefusalCode`), and its entry when not reached. */
const ENTRY_CODES = new Set(['banned', 'removed', 'invitation-only', 'full', 'refused', 'unanswered']);

/** How long a page waits for its host's answer to the entry its session asked for: the host
 * holds an entry for at most 30 seconds (`ENTERING_DEADLINE`), and a connection takes some. */
const ENTRY_TIMEOUT_MS = 45_000;
/** How long a session dialling the host its site named may go without reaching it before it
 * asks the host who it is (`site-host.mjs`): an entry reaches it in about a second. */
const SITE_HOST_CHECK_MS = 8_000;

/** A refusal as the shared entry reads it (`DeviceRefusal`): its `code`, the wait the entry
 * asked for in seconds, and, when a workspace's entry turned the person away, the entry's own
 * answer (`entry`: whether it keeps a wait list, and where its contact is). */
function hostRefusal(code, message, retryAfter = null, entry = null) {
  const error = new Error(message);
  error.name = 'DeviceRefusal';
  error.code = code;
  if (Number.isFinite(retryAfter) && retryAfter > 0) error.retryAfter = retryAfter;
  if (entry) error.entry = entry;
  return error;
}

/** The workspace `entry` (its base URL, `…/directory/<workspace>/`) lets people into. */
function entryWorkspace(entry) {
  const segments = new URL(entry).pathname.split('/').filter(Boolean);
  const at = segments.lastIndexOf('directory');
  return at >= 0 ? segments[at + 1] ?? null : null;
}

/** The host's answer to the entry `peer`'s session asked for, from its status (`entry`):
 * resolves once the workspace admitted it, rejects with the refusal. */
function entryAnswered(peer, timeoutMs = ENTRY_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    let stop = () => {};
    const timer = setTimeout(() => finish(hostRefusal('unanswered', 'The workspace’s host did not answer in time.')), timeoutMs);
    function finish(error, value) {
      clearTimeout(timer);
      stop();
      if (error) reject(error); else resolve(value);
    }
    const check = snapshot => {
      const entry = snapshot?.entry;
      if (entry?.outcome === 'admitted') finish(undefined, entry);
      else if (entry?.outcome === 'refused') finish(hostRefusal(entry.code, entry.message, entry.retryAfterSecs, entry.entry ?? null));
      else if (snapshot?.phase === 'failed') finish(hostRefusal(snapshot.errorCode ?? 'refused', snapshot.error ?? 'The host refused this device.'));
    };
    stop = peer.onStatus(check);
    check(peer.getSnapshot());
  });
}

/** Enter `workspace` on the session itself, through its entry or with an invitation (`entry`,
 * given the host's certificate as text, makes the entry: `entryRequest` or `invitationEntry` in
 * wasm). Who the host is the page's own site says (`site-host.mjs`), else the host itself; the
 * shared entry chooses the certificate (the one this very key holds from this host, else one the
 * root makes now), and the session asks to enter as it opens: the entry and the admission ride the
 * session's own first message, so a page its site told of the host asks it nothing over HTTPS,
 * and its first requests follow right behind. */
async function enterOnSession({
  origin, workspace, entry, identity, person, label, displayName, signal, onProgress, runtime, deviceKey,
  profile, fetcher, connectOptions,
}) {
  const named = siteServer(origin);
  let hostText = named?.text;
  let entered;
  try {
    hostText ??= await runtime.hostInfo(httpsPost(origin, fetcher));
    entered = await entry(memberText(hostText, 'hostCert'));
  } catch (error) {
    if (ENTRY_CODES.has(error?.code)) throw error;
    console.warn('join refused:', error?.code ?? error?.name, error?.message ?? error);
    throw Object.assign(new AdmissionError('refused', REFUSED_JOIN), {reason: error?.message ?? String(error), code: 'refused'});
  }
  const entering = JSON.parse(memberText(entered, 'entering'));
  const peer = await connect({
    ...connectOptions, origin, identity, userPublicKey: person.userPublicKey, workspace: workspace ?? entering.workspace,
    label, displayName, deviceKey, runtime, signal,
    profile: connectOptions.store ? undefined : profile,
    // As text: the certificate's nanosecond timestamps exceed what a JavaScript number holds
    // exactly (json-text.mjs).
    fetch: fetcher, deviceCertificate: memberText(entered, 'certificate'),
    hostInfo: named?.info ?? JSON.parse(hostText),
    entering,
  });
  try {
    const answered = await entryAnswered(peer);
    // The name the workspace gave itself, before its shared desktop reaches
    // the page: what the tab is titled after and the shell's first space is
    // given.
    peer.noteWorkspaceName?.(answered.name);
    onProgress?.({phase: 'admitted', workspace: answered.workspace, workspaceName: answered.name, role: answered.role});
    return peer;
  } catch (error) {
    await peer.dispose?.().catch(() => {});
    // The site names the host as it was before it was set up again: the site's to fix.
    if (error?.code === 'stale-entry') throw new AdmissionError('stale-entry', STALE_SITE_SERVER);
    // An entry's own refusal (its code, and the wait it asked for) is the caller's to show:
    // `workspaces.mjs` reads it. The workspace refusing the device the entry let in is a refusal
    // of what this browser brings, which the caller answers by coming again as a new device.
    if (ENTRY_CODES.has(error?.code)) throw error;
    console.warn('join refused:', error?.code ?? error?.name, error?.message ?? error);
    throw Object.assign(new AdmissionError('refused', REFUSED_JOIN), {reason: error?.message ?? String(error), code: 'refused'});
  }
}

async function join({
  origin, invitation, entry, identity, displayName = '', label = deviceLabel(), signal, onProgress,
  admissionToken, runtime, deviceKey, profile, fetch: fetcher, certified, ...connectOptions
}) {
  // A device certified elsewhere (`certified`, a paired device) asks as
  // itself; any other asks as the person whose root the page holds.
  if (!certified) identity ??= currentIdentity();
  if (!identity && !certified) throw new IdentityError('locked', 'Unlock the identity first');
  if (!origin || !(invitation || entry)) throw new TypeError('origin and an invitation or an entry are required');
  runtime ??= await pageRuntime(connectOptions.desktop);
  const person = identity ?? {userPublicKey: hex(JSON.parse(certified).cert.user_public_key)};
  profile ??= identityProfile(person, origin);
  deviceKey ??= await openDeviceKey({profile, storage: idbStorage()});
  const deviceKeyForWasm = deviceKey.kind === 'sealed-seed' ? await deviceKey.unseal() : deviceKey;
  signal?.throwIfAborted();
  onProgress?.({phase: 'asking'});
  const held = certified ?? await keptDeviceCertificate({runtime, profile, slot: deviceKey.slot, store: connectOptions.store,
    desktop: connectOptions.desktop, namespaces: connectOptions.namespaces});
  // The shared entry chooses the certificate (the one this very key holds
  // from this host, else one the root makes now) and has the workspace admit
  // this device under it.
  const accepting = JSON.stringify({held, label: fitDisplayName(label), name: fitDisplayName(displayName ?? ''),
    ...(admissionToken ? {admissionToken} : {})});
  const common = {
    origin, identity, person, label, displayName, signal, onProgress, runtime, deviceKey, profile, fetcher, connectOptions,
  };
  if (entry) {
    const workspace = entryWorkspace(entry);
    if (!workspace) throw new TypeError('the entry names no workspace');
    return enterOnSession({
      ...common, workspace,
      entry: hostCert => runtime.entryRequest(hostCert, workspace, deviceKeyForWasm, identity ?? undefined, accepting),
    });
  }
  return enterOnSession({
    ...common, workspace: null,
    entry: hostCert => runtime.invitationEntry(hostCert, invitationBytes(invitation), deviceKeyForWasm,
      identity ?? undefined, accepting),
  });
}

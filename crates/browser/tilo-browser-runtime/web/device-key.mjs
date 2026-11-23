/**
 * Device key device key: the page holds exactly one key, a device key, and
 * never a user root. The user's root-holding device certifies it through the
 * host (`peer.mjs`); this module only keeps the key and signs what the peer
 * runtime shows it (handshake challenges, certification requests, signed
 * document envelopes).
 *
 * - Where WebCrypto has Ed25519, the key is generated non-extractable and
 *   the CryptoKey object itself is stored (IndexedDB structured clone keeps
 *   it non-extractable). The private key never exists as bytes in the page.
 * - Otherwise a random 32-byte seed is sealed under a non-extractable
 *   AES-GCM key and only the ciphertext is stored; `unseal()` hands the seed
 *   to the wasm runtime, which keeps it in wasm memory only.
 *
 * A tab is a device of its own: the protocol takes one writer per device key
 * (each author numbers its changes, each device advances its own encryption
 * ratchet, and its store has one writer), so two tabs cannot share one. A
 * profile keeps a pool of at most `MAX_SLOTS` device keys, one per slot, each
 * held under an exclusive Web Lock for the device key's whole lifetime:
 * - A tab remembers its slot (`sessionStorage`, which a reload of the same
 *   tab keeps), and takes it back after a reload or a navigation, waiting
 *   for the page before it to let go of it: a reload is the same device.
 *   The slot it remembers is the first its page took: a second device key the
 *   same page opens of the profile is another device, not the tab's.
 * - A tab whose remembered slot another live tab holds (a duplicated tab
 *   copies `sessionStorage`) asks first (`BroadcastChannel`): a holder that
 *   answers is another tab, and this one takes a slot of its own at once.
 * - A new tab takes a free slot that has a key, so keys are reused across
 *   tab sessions, and makes a new one (a new device of the same person) only
 *   when every slot with a key is held by another tab.
 *
 * This one store also holds the person the browser is, when a tab holds one:
 * the root in exactly the identity store's record format (`personRecord`,
 * `rootOfPerson`), so the root a tab founds and the root a computer keeps are
 * the same record, and a computer imports it. That is `tilo_identity::store`'s
 * `IdentityRecord` (`crates/runtime/tilo-identity/src/store/`): a single-device
 * identity whose `secret_bytes` is the root's 32-byte seed. Beside it live the
 * two things that travel, not the root: the envelope that opens it elsewhere
 * and the revocations its root signed.
 *
 * Storage is `{get(key), put(key, value), delete(key)}`; `idbStorage()`
 * provides it over IndexedDB with strict durability.
 */

import {hex} from './hex.mjs';

export const LOCK_PREFIX = 'tilo-device-key:';
/** The prefix a page took a key's lock under before the one above. A page takes the lock
 * under both, the older first, so a page loaded before a site's update and one loaded after
 * it keep each other from the same key. */
const LOCK_PREFIX_BEFORE = 'tilo-device-custody:';
/** How many device keys (tabs open at once) a profile keeps at most. */
export const MAX_SLOTS = 16;
/** A slot's name within its profile: slot 0 is the profile itself. */
const slotName = (profile, slot) => (slot ? `${profile}#${slot}` : profile);
const keyRecord = (profile, slot = 0) => `device-key:${slotName(profile, slot)}`;

export class DeviceKeyError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'DeviceKeyError';
    this.code = code;
  }
}

async function supportsEd25519(subtle) {
  try {
    const probe = await subtle.generateKey({name: 'Ed25519'}, false, ['sign', 'verify']);
    return Boolean(probe?.privateKey);
  } catch (error) {
    if (error?.name === 'NotSupportedError' || error instanceof TypeError || error?.name === 'SyntaxError') return false;
    throw error;
  }
}

async function createWebCryptoKey(subtle) {
  const pair = await subtle.generateKey({name: 'Ed25519'}, false, ['sign', 'verify']);
  const publicKey = new Uint8Array(await subtle.exportKey('raw', pair.publicKey));
  return {kind: 'webcrypto-ed25519', privateKey: pair.privateKey, publicKey};
}

async function createSealedSeed(subtle, random) {
  const wrapKey = await subtle.generateKey({name: 'AES-GCM', length: 256}, false, ['encrypt', 'decrypt']);
  const seed = random(new Uint8Array(32));
  const iv = random(new Uint8Array(12));
  const sealed = new Uint8Array(await subtle.encrypt({name: 'AES-GCM', iv}, wrapKey, seed));
  seed.fill(0);
  return {kind: 'sealed-seed', wrapKey, iv, sealed};
}

/** How long a page asks whether a live tab holds a slot it remembers. */
export const LIVE_ASK_MS = 300;
/** How long a page waits for its remembered slot once no live tab says it
 * holds it: the page before a reload lets go of it within seconds, and a tab
 * frozen by its browser while holding it is let be after this. */
export const OWN_SLOT_WAIT_MS = 20_000;
const SLOT_MEMORY = 'tilo-device-slot:';
const CHANNEL = 'tilo-device-key';
/** Where pages ask whether a live tab holds a key, each with the prefix a key's lock is asked
 * by there: today's, and where pages asked before. A page asks and answers on both. */
const ASKED_ON = [[CHANNEL, LOCK_PREFIX], ['tilo-device-custody', LOCK_PREFIX_BEFORE]];

/** Takes the lock `name`: `{release, held}`, or `null` when another holds it (or
 * still does when `signal` aborts, with `signal`). `held` settles once the user
 * agent let go of the lock, after `release()`. */
async function takeLock(locks, name, signal) {
  let release;
  const released = new Promise(resolve => { release = resolve; });
  let held;
  const acquired = await new Promise((resolve, reject) => {
    held = locks.request(name, signal ? {mode: 'exclusive', signal} : {mode: 'exclusive', ifAvailable: true}, lock => {
      if (!lock) { resolve(false); return undefined; }
      resolve(true);
      return released;
    });
    held.catch(error => (signal?.aborted ? resolve(false) : reject(error)));
  });
  return acquired ? {release, held} : null;
}

/** Takes the lock of the key `name` under both prefixes: `{release, held}`, or `null` when
 * another page holds it under either. */
async function takeBoth(locks, name, signal) {
  const before = await takeLock(locks, LOCK_PREFIX_BEFORE + name, signal);
  if (!before) return null;
  const now = await takeLock(locks, LOCK_PREFIX + name, signal);
  if (!now) { before.release(); return null; }
  const held = Promise.all([now.held, before.held]);
  held.catch(() => {});
  return {release() { now.release(); before.release(); }, held};
}

/** The slots this page's deviceKeys hold, which it answers for while it lives. */
const holding = new Set();
let answering = null;
/** Answer other tabs asking whether a live tab holds a slot this page holds,
 * until the page goes (`pagehide`): a page on its way out answers nothing, so
 * the reload after it waits for its slot instead of taking another. */
function answerForSlots(broadcast, events) {
  if (answering !== null || !broadcast) return;
  answering = [];
  try {
    for (const [asked, prefix] of ASKED_ON) answering.push([broadcast(asked), prefix]);
  } catch {
    for (const [opened] of answering) opened.close?.();
    answering = false;
    return;
  }
  for (const [opened, prefix] of answering) {
    // Answering keeps nothing alive by itself (Node's channels would).
    opened.unref?.();
    opened.onmessage = ({data}) => {
      if (typeof data?.ask === 'string' && data.ask.startsWith(prefix) && holding.has(data.ask.slice(prefix.length))) opened.postMessage({live: data.ask});
    };
  }
  events?.addEventListener?.('pagehide', () => { holding.clear(); }, {once: true});
}

/** Asks the tabs live now whether one holds the device key `name`, and keeps
 * listening after the ask: an answer comes on the holder's own event loop, and
 * a busy machine can take longer over it than a page waits before deciding no
 * one is there. `live` settles at the first answer; `close()` stops asking. */
function askLiveTabs(broadcast, name) {
  const listening = [];
  let answered;
  const live = new Promise(resolve => { answered = resolve; });
  try {
    for (const [asked, prefix] of ASKED_ON) {
      const opened = broadcast?.(asked);
      if (opened) listening.push([opened, prefix]);
    }
  } catch { /* asked on the one that opened */ }
  for (const [opened, prefix] of listening) {
    opened.onmessage = ({data}) => { if (data?.live === prefix + name) answered(true); };
    opened.postMessage({ask: prefix + name});
  }
  return {live, asking: listening.length > 0, close() { for (const [opened] of listening) opened.close(); }};
}

/** `promise`'s answer, or `false` when it has not settled within `ms`. */
async function settledWithin(promise, ms) {
  let timer;
  const late = new Promise(resolve => { timer = setTimeout(() => resolve(false), ms); });
  try {
    return await Promise.race([promise, late]);
  } finally {
    clearTimeout(timer);
  }
}

/** The lock of the device key `name` this tab remembers, unless a live tab
 * holds it: the lock itself where it is free, or where the page before a
 * reload lets go of it before that wait ends. A tab that answers at any
 * point before then is live, so this page takes a key of its own instead of
 * waiting out a tab that never lets go: the answer decides, not the moment it
 * arrives. */
async function takeRememberedKey(locks, broadcast, name) {
  const free = await takeBoth(locks, name);
  if (free) return free;
  const asked = askLiveTabs(broadcast, name);
  try {
    if (asked.asking && await settledWithin(asked.live, LIVE_ASK_MS)) return null;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), OWN_SLOT_WAIT_MS);
    try {
      const waiting = takeBoth(locks, name, controller.signal);
      const outcome = await Promise.race([waiting.then(lock => ({lock})), asked.live.then(() => ({live: true}))]);
      if (outcome.live) {
        controller.abort();
        (await waiting.catch(() => null))?.release();
        return null;
      }
      return outcome.lock;
    } finally {
      clearTimeout(timer);
    }
  } finally {
    asked.close();
  }
}

const readSlot = (session, profile) => {
  try {
    const slot = Number(session?.getItem(SLOT_MEMORY + profile));
    return session?.getItem(SLOT_MEMORY + profile) != null && Number.isInteger(slot) && slot >= 0 && slot < MAX_SLOTS ? slot : null;
  } catch { return null; }
};
const keepSlot = (session, profile, slot) => {
  try { session?.setItem(SLOT_MEMORY + profile, String(slot)); } catch { /* not kept: the next load picks a free slot */ }
};
/** The profiles whose slot this page already keeps for its tab, by the
 * tab's session: the first device key a page opens of a profile is the tab's
 * device. Another one the same page opens of it (a second peer on the same
 * host, such as the one carrying reports to the home workspace) is a device
 * of its own, and a reload must not come back as that one: the host let only
 * the first into the workspace in front. */
const kept = new WeakMap();
const keepFirstSlot = (session, profile, slot) => {
  if (!session || typeof session !== 'object') return;
  const profiles = kept.get(session) ?? new Set();
  if (profiles.has(profile)) return;
  profiles.add(profile);
  kept.set(session, profiles);
  keepSlot(session, profile, slot);
};

/**
 * The slot of `profile` this tab takes: the one it remembers (`session`)
 * where no other live tab holds it, waiting for the page before it to let go
 * of it; else a free one with a key; else the first free one. With
 * `requireExisting`, only a slot that already has a key counts.
 * Resolves to `{slot, release, held}`.
 */
export async function takeSlot({profile, storage, locks, requireExisting = false, session, broadcast}) {
  const remembered = readSlot(session, profile);
  if (remembered !== null && (!requireExisting || await storage.get(keyRecord(profile, remembered)))) {
    const name = slotName(profile, remembered);
    const lock = await takeRememberedKey(locks, broadcast, name);
    if (lock) return {slot: remembered, ...lock};
  }
  const keyed = [];
  for (let slot = 0; slot < MAX_SLOTS; slot++) keyed.push(Boolean(await storage.get(keyRecord(profile, slot))));
  let busy = false;
  // Slots with a key first, so a key is reused before another is made.
  const order = [...keyed.keys()].sort((a, b) => Number(keyed[b]) - Number(keyed[a]) || a - b);
  for (const slot of order) {
    if (requireExisting && !keyed[slot]) continue;
    const lock = await takeBoth(locks, slotName(profile, slot));
    if (lock) return {slot, ...lock};
    busy = true;
  }
  if (requireExisting && !busy) throw new DeviceKeyError('device-key-missing', 'This account has no existing browser device');
  throw new DeviceKeyError('device-key-busy', requireExisting
    ? 'Every device key this browser has for this account is held by another tab'
    : `Every one of this browser's ${MAX_SLOTS} device keys for this profile is held by another tab`);
}

/**
 * Open the device key of the first free slot of `profile` (creating it on
 * first use: a new device). Resolves to `{kind, slot, created, publicKey?,
 * sign?, unseal?, close, destroy}`:
 * - `slot`: which of the profile's keys this is (0 for the first); each slot
 *   is a device of its own, with its own certificate and state.
 * - `created`: the key was made now, so no host has certified it yet.
 * - `webcrypto-ed25519`: `publicKey` (32 bytes) and `sign(message)`.
 * - `sealed-seed`: `unseal()` resolving to `{seed}` for the wasm runtime.
 * With `requireExisting`, rejects with `DeviceKeyError('device-key-missing')` when
 * the profile has no key, and `device-key-busy` when other tabs hold every one.
 */
export async function openDeviceKey({
  profile,
  storage,
  crypto = globalThis.crypto,
  locks = globalThis.navigator?.locks,
  preferWebCrypto = true,
  requireExisting = false,
  session = globalThis.sessionStorage,
  broadcast = globalThis.BroadcastChannel && (name => new BroadcastChannel(name)),
  events = globalThis,
}) {
  if (!profile || typeof profile !== 'string') throw new TypeError('profile is required');
  if (!storage) throw new TypeError('storage is required');
  if (!locks?.request) throw new DeviceKeyError('locks-unavailable', 'Web Locks are required for device key');
  const subtle = crypto.subtle;
  const random = bytes => crypto.getRandomValues(bytes);

  const {slot, release: letGo, held} = await takeSlot({profile, storage, locks, requireExisting, session, broadcast});
  const taken = slotName(profile, slot);
  holding.add(taken);
  answerForSlots(broadcast, events);
  keepFirstSlot(session, profile, slot);
  const release = () => { holding.delete(taken); letGo(); };
  try {
    let record = await storage.get(keyRecord(profile, slot));
    const created = !record;
    if (!record) {
      if (requireExisting) throw new DeviceKeyError('device-key-missing', 'This account has no existing browser device');
      record = preferWebCrypto && await supportsEd25519(subtle)
        ? await createWebCryptoKey(subtle)
        : await createSealedSeed(subtle, random);
      await storage.put(keyRecord(profile, slot), record);
    }
    let closed = false;
    const live = () => { if (closed) throw new DeviceKeyError('device-key-closed', 'Device key is closed'); };
    const common = {
      kind: record.kind,
      slot,
      created,
      /** Lets go of the device key; resolves once another holder may take it. */
      close() { if (!closed) { closed = true; release(); } return held.then(() => undefined, () => undefined); },
      /** Forget the key (after revocation): the next open creates a new device. */
      async destroy() { live(); await storage.delete(keyRecord(profile, slot)); this.close(); },
    };
    if (record.kind === 'webcrypto-ed25519') {
      return {
        ...common,
        publicKey: new Uint8Array(record.publicKey),
        async sign(message) {
          live();
          return new Uint8Array(await subtle.sign({name: 'Ed25519'}, record.privateKey, message));
        },
      };
    }
    if (record.kind === 'sealed-seed') {
      return {
        ...common,
        async unseal() {
          live();
          const seed = new Uint8Array(await subtle.decrypt({name: 'AES-GCM', iv: record.iv}, record.wrapKey, record.sealed));
          return {seed};
        },
      };
    }
    throw new DeviceKeyError('device-key-corrupt', `Unknown device key kind ${record.kind}`);
  } catch (error) {
    release();
    throw error;
  }
}

/** This browser profile's tab devices, in `localStorage`, which every tab
 * of the profile shares: `{id, certs, exact}`, the profile's own random id and
 * the certificate each tab's device key holds (as the peer keeps it, JSON). The
 * page hands them to the person's record when it lists their devices, so
 * every device of theirs lists this browser as one entry, and taking it back
 * takes back each of its tabs' keys. `exact` keeps each one's text as it was
 * issued, by its key in hex: its nanosecond times are signed, and a number
 * rounds them, so a page that proves the device with it (a workspace's web
 * page, served without the app) reads them from the text. */
export const BROWSER_DEVICES = 'tilo-browser-devices';
/** How many certificates are kept: past it the oldest goes, a key no tab
 * held for long (the pool reuses keys, so it rarely gets there). */
const BROWSER_DEVICES_KEPT = 4 * MAX_SLOTS;
/** Keep `certified` (`{cert, hostCert}` JSON, as a peer keeps it) as one of
 * this browser's tab devices, in place of an older certificate of its key. */
export function rememberBrowserDevice(certified, namespaces = globalThis.localStorage, random = bytes => globalThis.crypto.getRandomValues(bytes)) {
  try {
    const cert = JSON.parse(certified)?.cert;
    const key = JSON.stringify(cert?.device_public_key);
    if (!Array.isArray(cert?.device_public_key)) return;
    const kept = JSON.parse(namespaces?.getItem(BROWSER_DEVICES) ?? 'null');
    const id = typeof kept?.id === 'string' && /^[0-9a-f]{32}$/.test(kept.id) ? kept.id
      : hex(random(new Uint8Array(16)));
    const certs = [...(Array.isArray(kept?.certs) ? kept.certs : []).filter(other => JSON.stringify(other?.device_public_key) !== key), cert]
      .slice(-BROWSER_DEVICES_KEPT);
    const exact = {...(kept?.exact && typeof kept.exact === 'object' ? kept.exact : {}), [hex(cert.device_public_key)]: certified};
    const keys = new Set(certs.map(other => Array.isArray(other?.device_public_key) ? hex(other.device_public_key) : ''));
    for (const other of Object.keys(exact)) if (!keys.has(other)) delete exact[other];
    namespaces?.setItem(BROWSER_DEVICES, JSON.stringify({id, certs, exact}));
  } catch { /* not kept: the person's record lists this tab as a device of its own */ }
}

/** The store a browser kept a device's entries in before that store had today's name. */
const STORE_BEFORE = 'custody';

/**
 * `{get, put, delete}` over one IndexedDB object store, strict durability.
 *
 * A browser that entered before the store had today's name holds its device keys and its
 * person in the older store, and they are read and written there, where they are. Another
 * store can only be made by opening the database at a newer version, and then a page loaded
 * before could not open the database any more, and a page of now would wait for as long as
 * such a page is open in another tab. So the database stays at the version it has.
 */
export function idbStorage({indexedDB = globalThis.indexedDB, name = 'tilo-device', store = 'device-key'} = {}) {
  const open = () => new Promise((resolve, reject) => {
    const request = indexedDB.open(name, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(store);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  let db;
  let kept;
  const run = async (mode, action) => {
    db ??= await open();
    kept ??= store === 'device-key' && !db.objectStoreNames.contains(store) && db.objectStoreNames.contains(STORE_BEFORE)
      ? STORE_BEFORE : store;
    return new Promise((resolve, reject) => {
      const tx = db.transaction(kept, mode, {durability: 'strict'});
      const request = action(tx.objectStore(kept));
      let result;
      request.onsuccess = () => { result = request.result; };
      tx.oncomplete = () => resolve(result);
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
  };
  return {
    get: key => run('readonly', s => s.get(key)),
    put: (key, value) => run('readwrite', s => s.put(value, key)),
    delete: key => run('readwrite', s => s.delete(key)),
  };
}

/** Where the person this browser is is kept, in the same store as the tab's
 * device key. An older page kept its person under `identity`, another store's
 * name: that is not read, so a tab with only it starts as a fresh one. */
export const PERSON_KEY = 'person';
/** The envelope that opens the person's root elsewhere, and the revocations
 * its root signed: what travels, not the root, kept beside it. */
const PERSON_ENVELOPE_KEY = 'person/envelope';
const PERSON_REVOCATIONS_KEY = 'person/revocations';
/** Whether this browser has shown the person their 24 recovery words: the
 * words are shown once, right after they are first let in, and never again
 * unasked. */
export const PERSON_RECOVERY_SHOWN_KEY = 'person/recovery-shown';
/** The colour the identity store gives a person who chose none
 * (`tilo_identity::store`'s `DEFAULT_AVATAR_COLOR`). */
export const DEFAULT_AVATAR_COLOR = [128, 128, 128];

/**
 * The root `seed` and `publicKey` (32 bytes each) as the identity store's
 * `IdentityRecord`: a single-device identity whose `secret_bytes` is the seed,
 * with the person's presentation. Writing it and parsing it back is what
 * `tilo_identity::store` reads and writes for a computer, so a root the tab
 * founds and a root a computer imports are the same record.
 */
export function personRecord({
  seed, publicKey, displayName = '', avatarColor = DEFAULT_AVATAR_COLOR, photo,
  createdAt = new Date().toISOString().replace(/\.\d{3}Z$/, 'Z'),
}) {
  if (seed?.length !== 32 || publicKey?.length !== 32) throw new TypeError('a root is a 32-byte seed and public key');
  return {
    ...(photo ? {profile_photo: photo} : {}),
    display_name: String(displayName ?? '').trim(),
    avatar_color: [...avatarColor],
    threshold_identity: {
      single_device: true,
      group_public_key: [...publicKey],
      threshold: 1,
      total_shares: 1,
      share_index: 1,
      secret_bytes: [...seed],
    },
    created_at: createdAt,
    schema_version: 1,
  };
}

/** The root an `IdentityRecord` keeps: `{seed, publicKey}`, 32 bytes each. */
export function rootOfPerson(record) {
  const identity = record?.threshold_identity;
  if (!identity?.single_device
    || !Array.isArray(identity.secret_bytes) || identity.secret_bytes.length !== 32
    || !Array.isArray(identity.group_public_key) || identity.group_public_key.length !== 32) {
    throw new DeviceKeyError('person-corrupt', 'This browser keeps no single-device root');
  }
  return {seed: Uint8Array.from(identity.secret_bytes), publicKey: Uint8Array.from(identity.group_public_key)};
}

const readJson = async (storage, key) => {
  const text = await storage.get(key);
  return text === undefined || text === null ? null : JSON.parse(text);
};
const writeJson = async (storage, key, value) => {
  if (value === undefined || value === null) await storage.delete(key);
  else await storage.put(key, JSON.stringify(value));
};

/** What this browser keeps of the person, or `null`: `{record, envelope,
 * revocations}`, the record as the identity store's `IdentityRecord` (parsed
 * back from its JSON), the envelope that opens it elsewhere (or `null`), and
 * the revocations its root signed. */
export async function readPerson(storage) {
  const record = await readJson(storage, PERSON_KEY);
  if (!record) return null;
  return {
    record,
    envelope: await readJson(storage, PERSON_ENVELOPE_KEY),
    revocations: (await readJson(storage, PERSON_REVOCATIONS_KEY)) ?? [],
  };
}

/** Keep the person: `record` the identity store's `IdentityRecord`, `envelope`
 * what opens it elsewhere, `revocations` the entries its root signed. The
 * record goes as its JSON text, so what the page keeps is the bytes a computer
 * reads. */
export async function writePerson(storage, {record, envelope, revocations} = {}) {
  await storage.put(PERSON_KEY, JSON.stringify(record));
  await writeJson(storage, PERSON_ENVELOPE_KEY, envelope);
  await writeJson(storage, PERSON_REVOCATIONS_KEY, revocations?.length ? revocations : undefined);
}

/** Forget the person this browser keeps. */
export async function deletePerson(storage) {
  await storage.delete(PERSON_KEY);
  await storage.delete(PERSON_ENVELOPE_KEY);
  await storage.delete(PERSON_REVOCATIONS_KEY);
  await storage.delete(PERSON_RECOVERY_SHOWN_KEY);
}

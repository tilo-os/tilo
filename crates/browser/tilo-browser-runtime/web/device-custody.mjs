/**
 * Device key custody: the page holds exactly one key, a device key, and
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
 * held under an exclusive Web Lock for the custody's whole lifetime:
 * - A tab remembers its slot (`sessionStorage`, which a reload of the same
 *   tab keeps), and takes it back after a reload or a navigation, waiting
 *   for the page before it to let go of it: a reload is the same device.
 * - A tab whose remembered slot another live tab holds (a duplicated tab
 *   copies `sessionStorage`) asks first (`BroadcastChannel`): a holder that
 *   answers is another tab, and this one takes a slot of its own at once.
 * - A new tab takes a free slot that has a key, so keys are reused across
 *   tab sessions, and makes a new one (a new device of the same person) only
 *   when every slot with a key is held by another tab.
 *
 * Storage is `{get(key), put(key, value), delete(key)}`; `idbStorage()`
 * provides it over IndexedDB with strict durability.
 */

export const LOCK_PREFIX = 'tilo-device-custody:';
/** How many device keys (tabs open at once) a profile keeps at most. */
export const MAX_SLOTS = 16;
/** A slot's name within its profile: slot 0 is the profile itself. */
const slotName = (profile, slot) => (slot ? `${profile}#${slot}` : profile);
const keyRecord = (profile, slot = 0) => `device-key:${slotName(profile, slot)}`;

export class CustodyError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'CustodyError';
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
const CHANNEL = 'tilo-device-custody';

/** Takes the lock `name`: `{release, held}`, or `null` when another holds it
 * (or still does once `wait` ms passed, with `wait`). `held` settles once the
 * user agent let go of the lock, after `release()`. */
async function takeLock(locks, name, wait = 0) {
  let release;
  const released = new Promise(resolve => { release = resolve; });
  let held;
  const signal = wait ? AbortSignal.timeout(wait) : undefined;
  const acquired = await new Promise((resolve, reject) => {
    held = locks.request(name, wait ? {mode: 'exclusive', signal} : {mode: 'exclusive', ifAvailable: true}, lock => {
      if (!lock) { resolve(false); return undefined; }
      resolve(true);
      return released;
    });
    held.catch(error => (signal?.aborted ? resolve(false) : reject(error)));
  });
  return acquired ? {release, held} : null;
}

/** The slots this page's custodies hold, which it answers for while it lives. */
const holding = new Set();
let answering = null;
/** Answer other tabs asking whether a live tab holds a slot this page holds,
 * until the page goes (`pagehide`): a page on its way out answers nothing, so
 * the reload after it waits for its slot instead of taking another. */
function answerForSlots(broadcast, events) {
  if (answering !== null || !broadcast) return;
  try { answering = broadcast(CHANNEL); } catch { answering = false; return; }
  // Answering keeps nothing alive by itself (Node's channels would).
  answering.unref?.();
  answering.onmessage = ({data}) => {
    if (typeof data?.ask === 'string' && holding.has(data.ask)) answering.postMessage({live: data.ask});
  };
  events?.addEventListener?.('pagehide', () => { holding.clear(); }, {once: true});
}

/** Whether a live tab answers that it holds the slot lock `name`. */
async function liveHolder(broadcast, name) {
  let channel;
  try { channel = broadcast?.(CHANNEL); } catch { return false; }
  if (!channel) return false;
  try {
    return await new Promise(resolve => {
      const timer = setTimeout(() => resolve(false), LIVE_ASK_MS);
      channel.onmessage = ({data}) => { if (data?.live === name) { clearTimeout(timer); resolve(true); } };
      channel.postMessage({ask: name});
    });
  } finally {
    channel.close();
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

/**
 * The slot of `profile` this tab takes: the one it remembers (`session`)
 * where no other live tab holds it, waiting for the page before it to let go
 * of it; else a free one with a key; else the first free one. With
 * `requireExisting`, only a slot that already has a key counts.
 * Resolves to `{slot, release, held}`.
 */
export async function takeSlot({profile, storage, locks, requireExisting = false, session, broadcast}) {
  const lockName = slot => LOCK_PREFIX + slotName(profile, slot);
  const remembered = readSlot(session, profile);
  if (remembered !== null && (!requireExisting || await storage.get(keyRecord(profile, remembered)))) {
    const lock = await takeLock(locks, lockName(remembered))
      ?? (await liveHolder(broadcast, lockName(remembered)) ? null : await takeLock(locks, lockName(remembered), OWN_SLOT_WAIT_MS));
    if (lock) return {slot: remembered, ...lock};
  }
  const keyed = [];
  for (let slot = 0; slot < MAX_SLOTS; slot++) keyed.push(Boolean(await storage.get(keyRecord(profile, slot))));
  let busy = false;
  // Slots with a key first, so a key is reused before another is made.
  const order = [...keyed.keys()].sort((a, b) => Number(keyed[b]) - Number(keyed[a]) || a - b);
  for (const slot of order) {
    if (requireExisting && !keyed[slot]) continue;
    const lock = await takeLock(locks, lockName(slot));
    if (lock) return {slot, ...lock};
    busy = true;
  }
  if (requireExisting && !busy) throw new CustodyError('custody-missing', 'This account has no existing browser device');
  throw new CustodyError('custody-busy', requireExisting
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
 * With `requireExisting`, rejects with `CustodyError('custody-missing')` when
 * the profile has no key, and `custody-busy` when other tabs hold every one.
 */
export async function openDeviceCustody({
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
  if (!locks?.request) throw new CustodyError('locks-unavailable', 'Web Locks are required for device custody');
  const subtle = crypto.subtle;
  const random = bytes => crypto.getRandomValues(bytes);

  const {slot, release: letGo, held} = await takeSlot({profile, storage, locks, requireExisting, session, broadcast});
  const lockName = LOCK_PREFIX + slotName(profile, slot);
  holding.add(lockName);
  answerForSlots(broadcast, events);
  keepSlot(session, profile, slot);
  const release = () => { holding.delete(lockName); letGo(); };
  try {
    let record = await storage.get(keyRecord(profile, slot));
    const created = !record;
    if (!record) {
      if (requireExisting) throw new CustodyError('custody-missing', 'This account has no existing browser device');
      record = preferWebCrypto && await supportsEd25519(subtle)
        ? await createWebCryptoKey(subtle)
        : await createSealedSeed(subtle, random);
      await storage.put(keyRecord(profile, slot), record);
    }
    let closed = false;
    const live = () => { if (closed) throw new CustodyError('custody-closed', 'Device custody is closed'); };
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
    throw new CustodyError('custody-corrupt', `Unknown device key kind ${record.kind}`);
  } catch (error) {
    release();
    throw error;
  }
}

/** This browser profile's tab devices, in `localStorage`, which every tab
 * of the profile shares: `{id, certs}`, the profile's own random id and the
 * certificate each tab's device key holds (as the peer keeps it, JSON). The
 * page hands them to the person's record when it lists their devices, so
 * every device of theirs lists this browser as one entry, and taking it back
 * takes back each of its tabs' keys. */
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
      : [...random(new Uint8Array(16))].map(byte => byte.toString(16).padStart(2, '0')).join('');
    const certs = (Array.isArray(kept?.certs) ? kept.certs : []).filter(other => JSON.stringify(other?.device_public_key) !== key);
    namespaces?.setItem(BROWSER_DEVICES, JSON.stringify({id, certs: [...certs, cert].slice(-BROWSER_DEVICES_KEPT)}));
  } catch { /* not kept: the person's record lists this tab as a device of its own */ }
}

/** `{get, put, delete}` over one IndexedDB object store, strict durability. */
export function idbStorage({indexedDB = globalThis.indexedDB, name = 'tilo-device', store = 'custody'} = {}) {
  const open = () => new Promise((resolve, reject) => {
    const request = indexedDB.open(name, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(store);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  let db;
  const run = async (mode, action) => {
    db ??= await open();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(store, mode, {durability: 'strict'});
      const request = action(tx.objectStore(store));
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

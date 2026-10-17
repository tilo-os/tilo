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
 * A tab is a device of its own. A profile keeps a small pool of device keys,
 * one per slot, each held under an exclusive Web Lock for the custody's whole
 * lifetime: a tab takes the first slot whose lock is free, so a reload takes
 * its slot back and keys are reused across tab sessions, and a new slot (a new
 * key, to be certified as a new device of the same person) is made only when
 * every existing one is held by another tab.
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

/** Takes the lock `name` if no one holds it: `{release, held}`, else `null`.
 * `held` settles once the user agent let go of the lock, after `release()`. */
async function takeLock(locks, name) {
  let release;
  const released = new Promise(resolve => { release = resolve; });
  let held;
  const acquired = await new Promise((resolve, reject) => {
    held = locks.request(name, {mode: 'exclusive', ifAvailable: true}, lock => {
      if (!lock) { resolve(false); return undefined; }
      resolve(true);
      return released;
    });
    held.catch(reject);
  });
  return acquired ? {release, held} : null;
}

/**
 * The slot of `profile` this tab takes: the first whose lock is free. With
 * `requireExisting`, only a slot that already has a key counts.
 * Resolves to `{slot, release, held}`.
 */
export async function takeSlot({profile, storage, locks, requireExisting = false}) {
  let busy = false;
  for (let slot = 0; slot < MAX_SLOTS; slot++) {
    if (requireExisting && !await storage.get(keyRecord(profile, slot))) continue;
    const lock = await takeLock(locks, LOCK_PREFIX + slotName(profile, slot));
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
}) {
  if (!profile || typeof profile !== 'string') throw new TypeError('profile is required');
  if (!storage) throw new TypeError('storage is required');
  if (!locks?.request) throw new CustodyError('locks-unavailable', 'Web Locks are required for device custody');
  const subtle = crypto.subtle;
  const random = bytes => crypto.getRandomValues(bytes);

  const {slot, release, held} = await takeSlot({profile, storage, locks, requireExisting});
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

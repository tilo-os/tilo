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
 * A browser profile is one device of the person, whatever its tabs and
 * reloads: a profile keeps one device key, held under an exclusive Web Lock
 * for the custody's whole lifetime. A tab that finds it held (another tab of
 * the profile has the workspace open, or the page before a reload has not
 * let go of it yet) waits for it, however long that takes, and says that it
 * waits (`onWait`); it never makes another key, which would be another device
 * of the person with its own certificate and state.
 *
 * Storage is `{get(key), put(key, value), delete(key)}`; `idbStorage()`
 * provides it over IndexedDB with strict durability.
 */

export const LOCK_PREFIX = 'tilo-device-custody:';
const keyRecord = profile => `device-key:${profile}`;

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

/** What a page shows while its device key waits for another tab: the page
 * says so on its root (`data-custody="waiting"`) and tells whoever listens
 * (`tilo-custody-waiting`, with `detail.waiting`). */
export function sayCustodyWaiting(waiting, target = globalThis) {
  const root = target.document?.documentElement;
  if (root) {
    if (waiting) root.dataset.custody = 'waiting';
    else delete root.dataset.custody;
  }
  try { target.dispatchEvent?.(new CustomEvent('tilo-custody-waiting', {detail: {waiting}})); } catch { /* no events here */ }
}

/** Takes the lock `name`, waiting while another holds it: `{release, held}`.
 * `onWait(true)` runs when it has to wait, `onWait(false)` once it has it.
 * `held` settles once the user agent let go of the lock, after `release()`.
 * Rejects with the signal's reason when `signal` aborts the wait. */
async function takeLock(locks, name, {signal, onWait} = {}) {
  let release;
  const released = new Promise(resolve => { release = resolve; });
  const hold = () => released;
  let held;
  const free = await new Promise((resolve, reject) => {
    held = locks.request(name, {mode: 'exclusive', ifAvailable: true}, lock => {
      resolve(Boolean(lock));
      return lock ? hold() : undefined;
    });
    held.catch(reject);
  });
  if (!free) {
    onWait?.(true);
    try {
      await new Promise((resolve, reject) => {
        held = locks.request(name, {mode: 'exclusive', ...(signal ? {signal} : {})}, () => {
          resolve();
          return hold();
        });
        held.catch(reject);
      });
    } finally {
      onWait?.(false);
    }
  }
  return {release, held};
}

/**
 * Open the device key of `profile` (creating it on first use: a new device),
 * waiting for it while another tab of this browser holds it. Resolves to
 * `{kind, created, publicKey?, sign?, unseal?, close, destroy}`:
 * - `created`: the key was made now, so no host has certified it yet.
 * - `webcrypto-ed25519`: `publicKey` (32 bytes) and `sign(message)`.
 * - `sealed-seed`: `unseal()` resolving to `{seed}` for the wasm runtime.
 * With `requireExisting`, rejects with `CustodyError('custody-missing')` when
 * the profile has no key. `onWait` hears whether it waits for another tab
 * (default: the page says so, `sayCustodyWaiting`); `signal` gives up the wait.
 */
export async function openDeviceCustody({
  profile,
  storage,
  crypto = globalThis.crypto,
  locks = globalThis.navigator?.locks,
  preferWebCrypto = true,
  requireExisting = false,
  onWait = sayCustodyWaiting,
  signal,
}) {
  if (!profile || typeof profile !== 'string') throw new TypeError('profile is required');
  if (!storage) throw new TypeError('storage is required');
  if (!locks?.request) throw new CustodyError('locks-unavailable', 'Web Locks are required for device custody');
  const subtle = crypto.subtle;
  const random = bytes => crypto.getRandomValues(bytes);

  if (requireExisting && !await storage.get(keyRecord(profile))) {
    throw new CustodyError('custody-missing', 'This account has no existing browser device');
  }
  const {release, held} = await takeLock(locks, LOCK_PREFIX + profile, {signal, onWait});
  try {
    let record = await storage.get(keyRecord(profile));
    const created = !record;
    if (!record) {
      if (requireExisting) throw new CustodyError('custody-missing', 'This account has no existing browser device');
      record = preferWebCrypto && await supportsEd25519(subtle)
        ? await createWebCryptoKey(subtle)
        : await createSealedSeed(subtle, random);
      await storage.put(keyRecord(profile), record);
    }
    let closed = false;
    const live = () => { if (closed) throw new CustodyError('custody-closed', 'Device custody is closed'); };
    const common = {
      kind: record.kind,
      created,
      /** Lets go of the device key; resolves once another holder may take it. */
      close() { if (!closed) { closed = true; release(); } return held.then(() => undefined, () => undefined); },
      /** Forget the key (after revocation): the next open creates a new device. */
      async destroy() { live(); await storage.delete(keyRecord(profile)); this.close(); },
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

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
 * One tab owns a profile's key at a time: custody is held under an exclusive
 * Web Lock for its whole lifetime, so a second tab gets `custody-busy`
 * instead of racing the first.
 *
 * Storage is `{get(key), put(key, value), delete(key)}`; `idbStorage()`
 * provides it over IndexedDB with strict durability.
 */

export const LOCK_PREFIX = 'tilo-device-custody:';
/** One device key per profile; the lock is per profile too. */
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

/**
 * Open (creating on first use) the device key of `profile`.
 * Resolves to `{kind, publicKey?, sign?, unseal?, close, destroy}`:
 * - `webcrypto-ed25519`: `publicKey` (32 bytes) and `sign(message)`.
 * - `sealed-seed`: `unseal()` resolving to `{seed}` for the wasm runtime.
 * Rejects with `CustodyError('custody-busy')` while another tab holds it.
 */
export async function openDeviceCustody({
  profile,
  storage,
  crypto = globalThis.crypto,
  locks = globalThis.navigator?.locks,
  preferWebCrypto = true,
}) {
  if (!profile || typeof profile !== 'string') throw new TypeError('profile is required');
  if (!storage) throw new TypeError('storage is required');
  if (!locks?.request) throw new CustodyError('locks-unavailable', 'Web Locks are required for device custody');
  const subtle = crypto.subtle;
  const random = bytes => crypto.getRandomValues(bytes);

  let release;
  const released = new Promise(resolve => { release = resolve; });
  const acquired = await new Promise((resolve, reject) => {
    locks.request(LOCK_PREFIX + profile, {mode: 'exclusive', ifAvailable: true}, lock => {
      if (!lock) { resolve(false); return undefined; }
      resolve(true);
      return released;
    }).catch(reject);
  });
  if (!acquired) throw new CustodyError('custody-busy', 'Another tab holds this device key');

  try {
    let record = await storage.get(keyRecord(profile));
    if (!record) {
      record = preferWebCrypto && await supportsEd25519(subtle)
        ? await createWebCryptoKey(subtle)
        : await createSealedSeed(subtle, random);
      await storage.put(keyRecord(profile), record);
    }
    let closed = false;
    const live = () => { if (closed) throw new CustodyError('custody-closed', 'Device custody is closed'); };
    const common = {
      kind: record.kind,
      close() { if (!closed) { closed = true; release(); } },
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

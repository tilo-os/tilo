/**
 * The key that seals a profile's channel state at rest.
 *
 * The browser peer keeps its channel membership (MLS member state) in the
 * profile's durable store, sealed under a 32-byte vault key. The vault key
 * is never stored as bytes: it is sealed under a non-extractable AES-GCM
 * key, and only that `CryptoKey` (IndexedDB's structured clone keeps it
 * non-extractable) and the ciphertext are stored, in a store of their own.
 * `openChannelVault` unseals it for the wasm runtime, which keeps it in wasm
 * memory only.
 *
 * So the profile's durable state, wherever it is copied, does not carry the
 * key that opens it, and no script can export the wrapping key. Script
 * running in the page can still have WebCrypto unseal the vault, and the
 * browser keeps the wrapping key with the origin's IndexedDB data.
 *
 * Storage is `{get(key), put(key, value)}`, as `device-custody.mjs`'s
 * `idbStorage()` provides over IndexedDB with strict durability.
 */

const VAULT_BYTES = 32;
const IV_BYTES = 12;
const encoder = new TextEncoder();

const vaultRecord = (profile, userPublicKey) => `channel-vault:${profile}:${userPublicKey}`;
/** Binds the ciphertext to the profile and user it was sealed for. */
const vaultContext = (profile, userPublicKey) =>
  encoder.encode(`tilo channel vault v1\0${profile}\0${userPublicKey}`);

export class ChannelVaultError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'ChannelVaultError';
    this.code = code;
  }
}

/**
 * The vault key of `profile`'s channel state for the user `userPublicKey`
 * (hex), created on first use. Resolves to its 32 bytes.
 */
export async function openChannelVault({profile, userPublicKey, storage, crypto = globalThis.crypto}) {
  if (!profile || typeof profile !== 'string') throw new TypeError('profile is required');
  if (!userPublicKey || typeof userPublicKey !== 'string') throw new TypeError('userPublicKey is required');
  if (!storage) throw new TypeError('storage is required');
  const subtle = crypto.subtle;
  const key = vaultRecord(profile, userPublicKey);
  const additionalData = vaultContext(profile, userPublicKey);
  const stored = await storage.get(key);
  if (stored) {
    if (stored.wrapKey?.extractable !== false) {
      throw new ChannelVaultError('vault-corrupt', 'The channel vault wrapping key is extractable');
    }
    let vault;
    try {
      vault = new Uint8Array(await subtle.decrypt(
        {name: 'AES-GCM', iv: stored.iv, additionalData}, stored.wrapKey, stored.sealed));
    } catch {
      throw new ChannelVaultError('vault-corrupt', 'The channel vault does not open');
    }
    if (vault.length !== VAULT_BYTES) {
      vault.fill(0);
      throw new ChannelVaultError('vault-corrupt', 'The channel vault has the wrong length');
    }
    return vault;
  }
  const wrapKey = await subtle.generateKey({name: 'AES-GCM', length: 256}, false, ['encrypt', 'decrypt']);
  const vault = crypto.getRandomValues(new Uint8Array(VAULT_BYTES));
  const iv = crypto.getRandomValues(new Uint8Array(IV_BYTES));
  const sealed = new Uint8Array(await subtle.encrypt({name: 'AES-GCM', iv, additionalData}, wrapKey, vault));
  await storage.put(key, {wrapKey, iv, sealed});
  return vault;
}

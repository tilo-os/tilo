/**
 * A user root held by the page: the person's Tilo identity with no other
 * device. The root is an Ed25519 key generated here and stored only wrapped,
 * as a root envelope (JSON, safe to keep anywhere):
 *
 *   {version: 1, root_public_key: hex, wraps: [
 *     {kind: 'passkey', credential_id, rp_id, prf_salt, nonce, ciphertext},
 *     {kind: 'recovery', salt, nonce, ciphertext}]}
 *
 * Binary fields are unpadded base64url. Each wrap is AES-256-GCM over the
 * root's PKCS#8 encoding under HKDF-SHA256(ikm, salt): the WebAuthn PRF
 * output of a passkey (salt `prf_salt`, also the PRF input), or the recovery
 * key (shown once, 32 Crockford base32 symbols). The associated data binds a
 * wrap to its kind, the root public key and the credential. Native code
 * reads the same format (`tilo_user_identity::root_envelope`).
 *
 * Unlocking unwraps the root into a non-extractable WebCrypto key that lives
 * in this page's memory only; `lock()` drops it. Requires WebCrypto Ed25519.
 */

const VERSION = 1;
const LABEL = new TextEncoder().encode('tilo-seal/v1/user-root-wrap');
const PKCS8_PREFIX = Uint8Array.from([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20]);
const RECOVERY_BYTES = 20;
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
const PROBE = new TextEncoder().encode('tilo root envelope probe');

export class IdentityError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'IdentityError';
    this.code = code;
  }
}

export const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
export const unhex = text => {
  if (typeof text !== 'string' || !/^([0-9a-f]{2})*$/i.test(text)) throw new IdentityError('malformed', 'expected hex');
  return Uint8Array.from(text.match(/../g) ?? [], byte => parseInt(byte, 16));
};
export function base64url(bytes) {
  let text = '';
  for (const byte of bytes) text += String.fromCharCode(byte);
  return btoa(text).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
export function unbase64url(text) {
  if (typeof text !== 'string' || !/^[A-Za-z0-9_-]*$/.test(text)) throw new IdentityError('malformed', 'expected base64url');
  const binary = atob(text.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - text.length % 4) % 4));
  return Uint8Array.from(binary, char => char.charCodeAt(0));
}

/** The recovery key as the person writes it down: `XXXX-XXXX-…` (8 groups). */
export function formatRecoveryKey(bytes) {
  let bits = 0, value = 0, out = '';
  for (const byte of bytes) {
    value = (value << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      out += CROCKFORD[(value >>> (bits - 5)) & 31];
      bits -= 5;
    }
    value &= (1 << bits) - 1;
  }
  if (bits > 0) out += CROCKFORD[(value << (5 - bits)) & 31];
  return out.match(/.{1,4}/g).join('-');
}

/** Read a typed recovery key: case, spaces and dashes do not matter, and
 * `O` reads as `0`, `I`/`L` as `1`. */
export function parseRecoveryKey(text) {
  const symbols = String(text).toUpperCase().replace(/[\s-]/g, '').replace(/O/g, '0').replace(/[IL]/g, '1');
  if (symbols.length !== 32) throw new IdentityError('recovery-key-malformed', 'A recovery key has 32 characters');
  const out = new Uint8Array(RECOVERY_BYTES);
  let bits = 0, value = 0, index = 0;
  for (const symbol of symbols) {
    const digit = CROCKFORD.indexOf(symbol);
    if (digit < 0) throw new IdentityError('recovery-key-malformed', `"${symbol}" is not in a recovery key`);
    value = (value << 5) | digit;
    bits += 5;
    if (bits >= 8) {
      out[index++] = (value >>> (bits - 8)) & 0xff;
      bits -= 8;
    }
    value &= (1 << bits) - 1;
  }
  return out;
}

/** The associated data of a wrap: label and record, length-prefixed
 * (`tilo_seal::Context`). */
function associatedData(kind, rootPublicKey, credentialId = new Uint8Array()) {
  const kindBytes = new TextEncoder().encode(kind);
  const record = new Uint8Array(kindBytes.length + 1 + 32 + credentialId.length);
  record.set(kindBytes, 0);
  record.set(rootPublicKey, kindBytes.length + 1);
  record.set(credentialId, kindBytes.length + 33);
  const aad = new Uint8Array(2 + LABEL.length + 4 + record.length);
  const view = new DataView(aad.buffer);
  view.setUint16(0, LABEL.length, true);
  aad.set(LABEL, 2);
  view.setUint32(2 + LABEL.length, record.length, true);
  aad.set(record, 6 + LABEL.length);
  return aad;
}

async function wrappingKey(subtle, ikm, salt) {
  const material = await subtle.importKey('raw', ikm, 'HKDF', false, ['deriveKey']);
  return subtle.deriveKey({name: 'HKDF', hash: 'SHA-256', salt, info: LABEL}, material,
    {name: 'AES-GCM', length: 256}, false, ['wrapKey', 'unwrapKey']);
}

async function seal(env, root, ikm, salt, aad) {
  const key = await wrappingKey(env.subtle, ikm, salt);
  const nonce = env.random(12);
  const ciphertext = new Uint8Array(await env.subtle.wrapKey('pkcs8', root, key, {name: 'AES-GCM', iv: nonce, additionalData: aad}));
  return {nonce: base64url(nonce), ciphertext: base64url(ciphertext)};
}

async function unseal(env, wrap, ikm, salt, aad, extractable) {
  const key = await wrappingKey(env.subtle, ikm, salt);
  try {
    return await env.subtle.unwrapKey('pkcs8', unbase64url(wrap.ciphertext), key,
      {name: 'AES-GCM', iv: unbase64url(wrap.nonce), additionalData: aad}, {name: 'Ed25519'}, extractable, ['sign']);
  } catch {
    throw new IdentityError('rejected', 'This key does not open the identity');
  }
}

/** Whether `root` signs as `publicKey`. */
async function provesRoot(subtle, root, publicKey) {
  const signature = await subtle.sign({name: 'Ed25519'}, root, PROBE);
  const verifier = await subtle.importKey('raw', publicKey, {name: 'Ed25519'}, false, ['verify']);
  return subtle.verify({name: 'Ed25519'}, verifier, signature, PROBE);
}

function environment({crypto = globalThis.crypto, credentials = globalThis.navigator?.credentials} = {}) {
  if (!crypto?.subtle) throw new IdentityError('crypto-unavailable', 'WebCrypto is required');
  return {
    subtle: crypto.subtle,
    random: length => crypto.getRandomValues(new Uint8Array(length)),
    credentials,
  };
}

function rootPublicKeyOf(envelope) {
  if (!envelope || envelope.version !== VERSION) {
    throw new IdentityError('unsupported', `Unsupported root envelope version ${envelope?.version}`);
  }
  const key = unhex(envelope.root_public_key);
  if (key.length !== 32) throw new IdentityError('malformed', 'root_public_key must be 32 bytes');
  return key;
}

const prfFirst = credential => {
  const first = credential?.getClientExtensionResults?.()?.prf?.results?.first;
  if (!first) return undefined;
  return ArrayBuffer.isView(first) ? new Uint8Array(first.buffer, first.byteOffset, first.byteLength).slice() : new Uint8Array(first).slice();
};

/** Ask the authenticator for PRF output: one assertion over `wraps`. */
async function assertPasskey(env, wraps, rpId) {
  if (!env.credentials?.get) throw new IdentityError('passkeys-unavailable', 'Passkeys are unavailable here');
  const evalByCredential = {};
  for (const wrap of wraps) evalByCredential[wrap.credential_id] = {first: unbase64url(wrap.prf_salt)};
  const credential = await env.credentials.get({
    publicKey: {
      challenge: env.random(32),
      rpId,
      allowCredentials: wraps.map(wrap => ({type: 'public-key', id: unbase64url(wrap.credential_id)})),
      userVerification: 'required',
      extensions: {prf: {evalByCredential}},
    },
  });
  if (!credential) throw new IdentityError('passkey-cancelled', 'No passkey was used');
  const output = prfFirst(credential);
  if (!output) throw new IdentityError('prf-unsupported', 'This passkey cannot unlock a Tilo identity (no PRF)');
  return {credentialId: new Uint8Array(credential.rawId), output};
}

/** Create a passkey and read its PRF output for `salt`. */
async function createPasskey(env, {rpId, rpName = 'Tilo', userName, userId, displayName}, salt, exclude = []) {
  if (!env.credentials?.create) throw new IdentityError('passkeys-unavailable', 'Passkeys are unavailable here');
  if (!rpId || !userName || !userId) throw new TypeError('rpId, userName and userId are required');
  const credential = await env.credentials.create({
    publicKey: {
      rp: {id: rpId, name: rpName},
      user: {id: new TextEncoder().encode(String(userId)).slice(0, 64), name: userName, displayName: displayName ?? userName},
      challenge: env.random(32),
      pubKeyCredParams: [{type: 'public-key', alg: -8}, {type: 'public-key', alg: -7}, {type: 'public-key', alg: -257}],
      authenticatorSelection: {residentKey: 'required', userVerification: 'required'},
      // An authenticator that already holds one of this identity's passkeys
      // declines, rather than replacing it (same user handle).
      excludeCredentials: exclude.map(id => ({type: 'public-key', id: unbase64url(id)})),
      extensions: {prf: {eval: {first: salt}}},
    },
  });
  if (!credential) throw new IdentityError('passkey-cancelled', 'No passkey was created');
  const credentialId = new Uint8Array(credential.rawId);
  const prf = credential.getClientExtensionResults?.()?.prf;
  // Some authenticators evaluate the PRF only on assertion.
  let output = prfFirst(credential);
  if (!output && prf?.enabled !== false) {
    output = (await assertPasskey(env, [{credential_id: base64url(credentialId), prf_salt: base64url(salt)}], rpId)).output;
  }
  if (!output) throw new IdentityError('prf-unsupported', 'This passkey cannot unlock a Tilo identity (no PRF)');
  return {credentialId, output};
}

async function passkeyWrap(env, root, rootPublicKey, options, exclude) {
  const salt = env.random(32);
  const {credentialId, output} = await createPasskey(env, options, salt, exclude);
  const sealed = await seal(env, root, output, salt, associatedData('passkey', rootPublicKey, credentialId));
  output.fill(0);
  return {kind: 'passkey', credential_id: base64url(credentialId), rp_id: options.rpId, prf_salt: base64url(salt), ...sealed};
}

/**
 * The unlocked identity: `{kind, publicKey, userPublicKey, sign, addPasskey,
 * lock}`. `kind`/`publicKey`/`sign` are the shape the wasm runtime takes as
 * a signer. `extractable` is the root as unwrapped (it is used once, to keep
 * a copy re-wrappable under a page-only key, and then dropped).
 */
async function unlocked(env, extractable, rootPublicKey) {
  const session = await env.subtle.generateKey({name: 'AES-GCM', length: 256}, false, ['wrapKey', 'unwrapKey']);
  const sessionNonce = env.random(12);
  const kept = await env.subtle.wrapKey('pkcs8', extractable, session, {name: 'AES-GCM', iv: sessionNonce});
  let signer = await env.subtle.unwrapKey('pkcs8', kept, session, {name: 'AES-GCM', iv: sessionNonce}, {name: 'Ed25519'}, false, ['sign']);
  let locked = false;
  const live = () => { if (locked) throw new IdentityError('locked', 'The identity is locked'); };
  const identity = {
    kind: 'webcrypto-ed25519',
    publicKey: rootPublicKey,
    userPublicKey: hex(rootPublicKey),
    async sign(message) {
      live();
      return new Uint8Array(await env.subtle.sign({name: 'Ed25519'}, signer, message));
    },
    /** `envelope` with one more passkey wrap: a new passkey, created on an
     * authenticator that holds none of the envelope's. */
    async addPasskey(envelope, options) {
      live();
      if (hex(rootPublicKeyOf(envelope)) !== identity.userPublicKey) {
        throw new IdentityError('wrong-identity', 'This envelope belongs to another identity');
      }
      const root = await env.subtle.unwrapKey('pkcs8', kept, session, {name: 'AES-GCM', iv: sessionNonce}, {name: 'Ed25519'}, true, ['sign']);
      const exclude = envelope.wraps
        .filter(wrap => wrap.kind === 'passkey' && wrap.rp_id === options.rpId)
        .map(wrap => wrap.credential_id);
      const wrap = await passkeyWrap(env, root, rootPublicKey, options, exclude);
      return {...envelope, wraps: [...envelope.wraps, wrap]};
    },
    /** Forget the root: this object signs nothing more. */
    lock() { locked = true; signer = undefined; },
    get locked() { return locked; },
  };
  return identity;
}

/**
 * Create a new identity. With `passkey` (default) a passkey for `rpId` is
 * created with the PRF extension and wraps the root; a recovery key always
 * does. Resolves to `{envelope, recoveryKey, identity}`: store the envelope,
 * show the recovery key once.
 *
 * @param options.rpId, options.userName, options.userId  the passkey's
 *   relying party and user handle (`userId` ≤ 64 bytes)
 * @param options.displayName, options.rpName  shown by the authenticator
 * @param options.passkey  `false`: recovery key only
 */
export async function createIdentity(options = {}) {
  const env = environment(options);
  const root = await env.subtle.generateKey({name: 'Ed25519'}, true, ['sign', 'verify']);
  const rootPublicKey = new Uint8Array(await env.subtle.exportKey('raw', root.publicKey));
  const wraps = [];
  if (options.passkey !== false) wraps.push(await passkeyWrap(env, root.privateKey, rootPublicKey, options));
  const recovery = env.random(RECOVERY_BYTES);
  const salt = env.random(32);
  wraps.push({kind: 'recovery', salt: base64url(salt),
    ...await seal(env, root.privateKey, recovery, salt, associatedData('recovery', rootPublicKey))});
  const recoveryKey = formatRecoveryKey(recovery);
  recovery.fill(0);
  const envelope = {version: VERSION, root_public_key: hex(rootPublicKey), wraps};
  return {envelope, recoveryKey, identity: await unlocked(env, root.privateKey, rootPublicKey)};
}

/**
 * Unlock `envelope` with a passkey (`{passkey: true, rpId?}`: one assertion
 * over every passkey wrap of `rpId`, default the first wrap's) or with
 * `{recoveryKey}`. Resolves to the unlocked identity.
 */
export async function unlockIdentity(envelope, how = {passkey: true}) {
  const env = environment(how);
  const rootPublicKey = rootPublicKeyOf(envelope);
  let root;
  if (how.recoveryKey !== undefined) {
    const recovery = parseRecoveryKey(how.recoveryKey);
    const wraps = envelope.wraps.filter(wrap => wrap.kind === 'recovery');
    if (!wraps.length) throw new IdentityError('no-wrap', 'This identity has no recovery key');
    for (const wrap of wraps) {
      root = await unseal(env, wrap, recovery, unbase64url(wrap.salt), associatedData('recovery', rootPublicKey), true).catch(() => undefined);
      if (root) break;
    }
    recovery.fill(0);
    if (!root) throw new IdentityError('rejected', 'This recovery key does not open the identity');
  } else if (how.passkey) {
    const passkeys = envelope.wraps.filter(wrap => wrap.kind === 'passkey');
    const rpId = how.rpId ?? passkeys[0]?.rp_id;
    const candidates = passkeys.filter(wrap => wrap.rp_id === rpId);
    if (!candidates.length) throw new IdentityError('no-wrap', 'This identity has no passkey here');
    const {credentialId, output} = await assertPasskey(env, candidates, rpId);
    const wrap = candidates.find(candidate => candidate.credential_id === base64url(credentialId));
    if (!wrap) throw new IdentityError('no-wrap', 'That passkey does not belong to this identity');
    try {
      root = await unseal(env, wrap, output, unbase64url(wrap.prf_salt), associatedData('passkey', rootPublicKey, credentialId), true);
    } finally {
      output.fill(0);
    }
  } else {
    throw new TypeError('unlock with {passkey: true} or {recoveryKey}');
  }
  if (!await provesRoot(env.subtle, root, rootPublicKey)) {
    throw new IdentityError('wrong-root', 'The envelope holds another key than its root');
  }
  return unlocked(env, root, rootPublicKey);
}

/** `envelope` with a new passkey wrap, from an unlocked `options.identity`. */
export function addPasskey(envelope, {identity, ...options}) {
  if (!identity?.addPasskey) throw new TypeError('addPasskey needs an unlocked identity');
  return identity.addPasskey(envelope, options);
}

/**
 * Certify another device with the unlocked root. `role` `node` (default): a
 * native node that hosts and governs the person's workspaces (relays, pins,
 * reads and owns them; admits no devices; one year). `browser`: a member
 * for 30 days. `devicePublicKey` is the device's Ed25519 key (hex or
 * bytes), delivered out of band; never the root's own. `runtime` is the wasm
 * module. Resolves to the `DeviceCert` JSON.
 */
export async function certifyDevice({identity, runtime, devicePublicKey, label = 'Tilo node', role = 'node'}) {
  if (!identity?.sign || identity.locked) throw new IdentityError('locked', 'Unlock the identity first');
  if (!runtime?.issueDeviceCert) throw new TypeError('certifyDevice needs the wasm runtime');
  const key = typeof devicePublicKey === 'string' ? devicePublicKey.toLowerCase() : hex(devicePublicKey ?? []);
  if (unhex(key).length !== 32) throw new IdentityError('malformed', 'A device public key is 32 bytes');
  return runtime.issueDeviceCert(identity, key, label, role);
}

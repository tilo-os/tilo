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
 * The identity created or unlocked last is this page's current one: the
 * calls below that take an `identity` default to it.
 */

const VERSION = 1;
const LABEL = new TextEncoder().encode('tilo-seal/v1/user-root-wrap');
const PKCS8_PREFIX = Uint8Array.from([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20]);
const RECOVERY_BYTES = 20;
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
const PROBE = new TextEncoder().encode('tilo root envelope probe');
let current;

/** The identity this page created or unlocked last, unless locked. */
export function currentIdentity() {
  return current?.locked ? undefined : current;
}

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

/** Bytes from bytes, a buffer or base64url (a copy). */
const bytesOf = value => (typeof value === 'string' ? unbase64url(value)
  : ArrayBuffer.isView(value) ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength).slice()
    : new Uint8Array(value).slice());

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
  const salt = options.prfSalt === undefined ? env.random(32) : bytesOf(options.prfSalt);
  if (salt.length !== 32) throw new TypeError('prfSalt must be 32 bytes');
  const {credentialId, output} = await createPasskey(env, options, salt, exclude);
  const sealed = await seal(env, root, output, salt, associatedData('passkey', rootPublicKey, credentialId));
  output.fill(0);
  return {kind: 'passkey', credential_id: base64url(credentialId), rp_id: options.rpId, prf_salt: base64url(salt), ...sealed};
}

/** The root wrapped under a non-extractable AES-GCM key: how it is held
 * while unlocked (a page-only key) and how this browser keeps it (the key
 * stored in IndexedDB as the CryptoKey itself, so it never leaves). */
async function holdRoot(env, extractable, key) {
  key ??= await env.subtle.generateKey({name: 'AES-GCM', length: 256}, false, ['wrapKey', 'unwrapKey']);
  const nonce = env.random(12);
  const wrapped = new Uint8Array(await env.subtle.wrapKey('pkcs8', extractable, key, {name: 'AES-GCM', iv: nonce}));
  return {key, nonce, wrapped};
}

const releaseRoot = (env, held, extractable) => env.subtle.unwrapKey('pkcs8', held.wrapped, held.key,
  {name: 'AES-GCM', iv: held.nonce}, {name: 'Ed25519'}, extractable, ['sign']);

async function recoveryWrap(env, root, rootPublicKey) {
  const recovery = env.random(RECOVERY_BYTES);
  const salt = env.random(32);
  const wrap = {kind: 'recovery', salt: base64url(salt),
    ...await seal(env, root, recovery, salt, associatedData('recovery', rootPublicKey))};
  const recoveryKey = formatRecoveryKey(recovery);
  recovery.fill(0);
  return {wrap, recoveryKey};
}

/**
 * The unlocked identity: `{kind, publicKey, userPublicKey, sign, addPasskey,
 * addRecoveryKey, lock}`. `kind`/`publicKey`/`sign` are the shape the wasm
 * runtime takes as a signer. `held` is the root wrapped under a
 * non-extractable key (`holdRoot`), released extractable only to wrap it
 * anew.
 */
async function unlocked(env, held, rootPublicKey, account) {
  let signer = await releaseRoot(env, held, false);
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
     * authenticator that holds none of the envelope's. `rpId` defaults to
     * the envelope's passkeys', `userName`/`userId` to the account the
     * identity was created with. */
    async addPasskey(envelope, options = {}) {
      live();
      const first = envelope?.wraps?.find(wrap => wrap.kind === 'passkey');
      options = {
        // One PRF input for all of an identity's passkeys, so a login can
        // evaluate it without knowing which passkey answers.
        rpId: first?.rp_id,
        prfSalt: first?.prf_salt,
        ...account,
        ...Object.fromEntries(Object.entries(options).filter(([, value]) => value !== undefined)),
      };
      if (hex(rootPublicKeyOf(envelope)) !== identity.userPublicKey) {
        throw new IdentityError('wrong-identity', 'This envelope belongs to another identity');
      }
      const root = await releaseRoot(env, held, true);
      const exclude = envelope.wraps
        .filter(wrap => wrap.kind === 'passkey' && wrap.rp_id === options.rpId)
        .map(wrap => wrap.credential_id);
      const wrap = await passkeyWrap(env, root, rootPublicKey, options, exclude);
      return {...envelope, wraps: [...envelope.wraps, wrap]};
    },
    /** `envelope` with a new recovery key wrap in place of any earlier one:
     * resolves to `{envelope, recoveryKey}`; show the key once. */
    async addRecoveryKey(envelope) {
      live();
      if (hex(rootPublicKeyOf(envelope)) !== identity.userPublicKey) {
        throw new IdentityError('wrong-identity', 'This envelope belongs to another identity');
      }
      const {wrap, recoveryKey} = await recoveryWrap(env, await releaseRoot(env, held, true), rootPublicKey);
      return {envelope: {...envelope, wraps: [...envelope.wraps.filter(other => other.kind !== 'recovery'), wrap]}, recoveryKey};
    },
    /** Forget the root: this object signs nothing more. */
    lock() { locked = true; signer = undefined; if (current === identity) current = undefined; },
    get locked() { return locked; },
  };
  current = identity;
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
 * @param options.prfSalt  the PRF input (32 bytes or base64url), default
 *   random; later passkeys reuse it, so a login can evaluate the PRF with
 *   one known input (`extensions.prf.eval`) whichever passkey answers
 */
export async function createIdentity(options = {}) {
  const env = environment(options);
  const root = await env.subtle.generateKey({name: 'Ed25519'}, true, ['sign', 'verify']);
  const rootPublicKey = new Uint8Array(await env.subtle.exportKey('raw', root.publicKey));
  const wraps = [];
  if (options.passkey !== false) wraps.push(await passkeyWrap(env, root.privateKey, rootPublicKey, options));
  const {wrap, recoveryKey} = await recoveryWrap(env, root.privateKey, rootPublicKey);
  wraps.push(wrap);
  const envelope = {version: VERSION, root_public_key: hex(rootPublicKey), wraps};
  const account = options.userName && options.userId
    ? {userName: options.userName, userId: options.userId, displayName: options.displayName, rpName: options.rpName}
    : undefined;
  return {envelope, recoveryKey, identity: await unlocked(env, await holdRoot(env, root.privateKey), rootPublicKey, account)};
}

/**
 * Unlock `envelope` with a passkey (`{passkey: true, rpId?}`: one assertion
 * over every passkey wrap of `rpId`, default the first wrap's), with the PRF
 * output of an assertion the page ran itself (`{prfOutput, credentialId}`,
 * bytes or base64url; evaluated with the wrap's `prf_salt`), or with
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
  } else if (how.prfOutput !== undefined) {
    const credentialId = bytesOf(how.credentialId ?? []);
    const output = bytesOf(how.prfOutput);
    const wrap = envelope.wraps.find(candidate => candidate.kind === 'passkey' && candidate.credential_id === base64url(credentialId));
    if (!wrap) throw new IdentityError('no-wrap', 'That passkey does not belong to this identity');
    try {
      root = await unseal(env, wrap, output, unbase64url(wrap.prf_salt), associatedData('passkey', rootPublicKey, credentialId), true);
    } finally {
      output.fill(0);
    }
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
    throw new TypeError('unlock with {passkey: true}, {prfOutput, credentialId} or {recoveryKey}');
  }
  if (!await provesRoot(env.subtle, root, rootPublicKey)) {
    throw new IdentityError('wrong-root', 'The envelope holds another key than its root');
  }
  return unlocked(env, await holdRoot(env, root), rootPublicKey);
}

/** `envelope` with a new passkey wrap, from `options.identity` (default the
 * current identity); see `RootIdentity.addPasskey` for the defaults. */
export async function addPasskey(envelope, {identity = currentIdentity(), ...options} = {}) {
  if (!identity?.addPasskey) throw new IdentityError('locked', 'addPasskey needs an unlocked identity');
  return identity.addPasskey(envelope, options);
}

/** Where this browser keeps its person (`{get, put, delete}`). */
const BROWSER_RECORD = 'identity';
const PRF_LABEL = new TextEncoder().encode('tilo-seal/v1/user-root-prf');
const ENVELOPE_LABEL = new TextEncoder().encode('tilo root envelope v1\0');
const PROFILE_LABEL = new TextEncoder().encode('tilo person profile v1\0');

/** The PRF input of every passkey that keeps a browser's person: one known
 * value, so signing in on a new browser evaluates the PRF in the same
 * assertion that names the person, before their envelope is fetched. */
async function browserPrfSalt(subtle) {
  return new Uint8Array(await subtle.digest('SHA-256', PRF_LABEL));
}

const emptyEnvelope = rootPublicKey => ({version: VERSION, root_public_key: hex(rootPublicKey), wraps: []});

/** Whether `envelope` can open the root anywhere else: a passkey or a
 * recovery key wraps it. */
export const envelopeKept = envelope => Boolean(envelope?.wraps?.some(wrap => wrap.kind === 'passkey' || wrap.kind === 'recovery'));

async function keepInBrowser(env, storage, rootPublicKey, extractable, envelope) {
  const record = {rootPublicKey, held: await holdRoot(env, extractable), envelope};
  await storage.put(BROWSER_RECORD, record);
  return record;
}

/**
 * Ask the browser to keep this site's storage (`navigator.storage.persist()`)
 * through `storage`, without waiting for the answer. Firefox answers only
 * once the person dismisses its permission prompt, so nothing may await it:
 * it is asked once there is something worth keeping (a person kept with a
 * passkey or a recovery key, or signed in with one), never while a page
 * loads.
 */
export function askToKeepStorage(storage = globalThis.navigator?.storage) {
  try {
    void Promise.resolve(storage?.persist?.()).catch(() => {});
  } catch { /* No storage manager: nothing to ask. */ }
}

/**
 * The person `record` keeps, unlocked: the identity (current from now on)
 * with what keeping it takes. `envelope` is what may travel (the passkey and
 * recovery wraps; empty until the person keeps it), `kept` whether it opens
 * anywhere but here. Keeping it asks the browser to keep this site's
 * storage through `persist` (`askToKeepStorage`).
 */
async function openKept(env, storage, record, persist) {
  const {rootPublicKey} = record;
  const salt = await browserPrfSalt(env.subtle);
  const identity = await unlocked(env, record.held, rootPublicKey, {
    // The passkey's user handle is the root public key: a passkey picked on
    // a new browser names the person whose envelope to fetch.
    userId: hex(rootPublicKey), userName: 'Tilo', rpName: 'Tilo',
  });
  const save = async envelope => {
    record.envelope = envelope;
    await storage.put(BROWSER_RECORD, record);
    askToKeepStorage(persist);
    return envelope;
  };
  // Descriptors, not values: `envelope` and `kept` follow what is kept.
  return Object.defineProperties(identity, Object.getOwnPropertyDescriptors({
    get envelope() { return record.envelope; },
    get kept() { return envelopeKept(record.envelope); },
    /** Add a passkey for `rpId` (default this page's host) that opens this
     * person; `userName` is what the authenticator lists it as. Resolves to
     * the new envelope, also kept here. */
    async keepWithPasskey({rpId = globalThis.location?.hostname, userName, displayName} = {}) {
      const options = {rpId, prfSalt: salt};
      if (userName) Object.assign(options, {userName, displayName: displayName ?? userName});
      return save(await identity.addPasskey(record.envelope, options));
    },
    /** A new recovery key, replacing any earlier one; resolves to the key
     * as the person writes it down (shown once, never stored here). */
    async createRecoveryKey() {
      const {envelope, recoveryKey} = await identity.addRecoveryKey(record.envelope);
      await save(envelope);
      return recoveryKey;
    },
  }));
}

/**
 * The person this browser is. The first call creates one: a user root,
 * never in the clear, held wrapped under a non-extractable key kept in
 * IndexedDB (the CryptoKey itself, which never leaves the browser). It is a
 * real identity from the start: it joins and owns workspaces like any
 * other, and keeping it (`keepWithPasskey`, `createRecoveryKey`) only adds
 * ways to open the same root elsewhere. Creating one asks the browser
 * for nothing: a page that loads never waits on a permission prompt; the
 * site's storage is asked to be kept once the person keeps it
 * (`askToKeepStorage`, through `persist`).
 *
 * Resolves to the identity with `{envelope, kept, keepWithPasskey,
 * createRecoveryKey}`, or `null` with `create: false` and none kept.
 */
export async function browserIdentity({storage, crypto, credentials, create = true, persist = globalThis.navigator?.storage} = {}) {
  storage ??= (await import('./device-custody.mjs')).idbStorage();
  const env = environment({crypto, credentials});
  let record = await storage.get(BROWSER_RECORD);
  if (!record) {
    if (!create) return null;
    const root = await env.subtle.generateKey({name: 'Ed25519'}, true, ['sign', 'verify']);
    const rootPublicKey = new Uint8Array(await env.subtle.exportKey('raw', root.publicKey));
    record = await keepInBrowser(env, storage, rootPublicKey, root.privateKey, emptyEnvelope(rootPublicKey));
  }
  return openKept(env, storage, record, persist);
}

/**
 * The person this browser joins workspaces as: the identity unlocked last,
 * else this browser's own (`browserIdentity`, created on the first visit).
 * Every caller that joins a workspace asks here.
 */
export async function browserPerson(options) {
  return currentIdentity() ?? browserIdentity(options);
}

/** Whether this site's storage survives the browser clearing space. */
export async function storagePersisted(storage = globalThis.navigator?.storage) {
  return Boolean(await storage?.persisted?.().catch(() => false));
}

/**
 * Become, in this browser, a person kept with a passkey elsewhere: the
 * person picks a passkey (a discoverable credential; its user handle names
 * the root), `fetchEnvelope(rootPublicKeyHex)` finds that person's envelope
 * (from a host that keeps it, `fetchEnvelope` below), and the PRF output the
 * same assertion gave opens it. The root is then kept here as
 * `browserIdentity` keeps it, replacing whoever this browser was.
 */
export async function signInWithPasskey({rpId = globalThis.location?.hostname, fetchEnvelope: find, storage, crypto, credentials,
  persist = globalThis.navigator?.storage} = {}) {
  if (typeof find !== 'function') throw new TypeError('signInWithPasskey needs fetchEnvelope');
  storage ??= (await import('./device-custody.mjs')).idbStorage();
  const env = environment({crypto, credentials});
  if (!env.credentials?.get) throw new IdentityError('passkeys-unavailable', 'Passkeys are unavailable here');
  const salt = await browserPrfSalt(env.subtle);
  const credential = await env.credentials.get({
    publicKey: {challenge: env.random(32), rpId, userVerification: 'required', extensions: {prf: {eval: {first: salt}}}},
  });
  if (!credential) throw new IdentityError('passkey-cancelled', 'No passkey was used');
  const output = prfFirst(credential);
  if (!output) throw new IdentityError('prf-unsupported', 'This passkey cannot unlock a Tilo identity (no PRF)');
  try {
    const handle = credential.response?.userHandle;
    const rootHex = handle ? new TextDecoder().decode(handle) : '';
    if (!/^[0-9a-f]{64}$/.test(rootHex)) throw new IdentityError('not-tilo', 'This passkey does not name a Tilo identity');
    const envelope = await find(rootHex);
    if (!envelope) throw new IdentityError('no-envelope', 'No host this browser knows keeps this identity');
    const rootPublicKey = rootPublicKeyOf(envelope);
    if (hex(rootPublicKey) !== rootHex) throw new IdentityError('wrong-identity', 'The host answered with another identity');
    const credentialId = new Uint8Array(credential.rawId);
    const wrap = envelope.wraps.find(candidate => candidate.kind === 'passkey' && candidate.credential_id === base64url(credentialId));
    if (!wrap) throw new IdentityError('no-wrap', 'That passkey does not belong to this identity');
    const root = await unseal(env, wrap, output, unbase64url(wrap.prf_salt), associatedData('passkey', rootPublicKey, credentialId), true);
    if (!await provesRoot(env.subtle, root, rootPublicKey)) throw new IdentityError('wrong-root', 'The envelope holds another key than its root');
    currentIdentity()?.lock();
    const signedIn = await openKept(env, storage, await keepInBrowser(env, storage, rootPublicKey, root, envelope), persist);
    askToKeepStorage(persist);
    return signedIn;
  } finally {
    output.fill(0);
  }
}

/** The longest name a host keeps in a person's profile, in UTF-8 bytes. */
export const MAX_DISPLAY_NAME_BYTES = 128;

/**
 * `name` as a profile keeps it: trimmed, without control characters, and
 * cut at a character boundary to `MAX_DISPLAY_NAME_BYTES` of UTF-8.
 */
export function fitDisplayName(name) {
  const encoder = new TextEncoder();
  let fitted = '';
  let bytes = 0;
  for (const char of String(name ?? '').replace(/\p{Cc}/gu, '').trim()) {
    bytes += encoder.encode(char).length;
    if (bytes > MAX_DISPLAY_NAME_BYTES) break;
    fitted += char;
  }
  return fitted.trim();
}

/** The bytes a root signs to have a host keep one of its records (its
 * envelope, its profile): label, issue time (ms, u64 little-endian), the
 * record's JSON. */
function rootStatement(label, issuedAtMs, json) {
  const body = new TextEncoder().encode(json);
  const out = new Uint8Array(label.length + 8 + body.length);
  out.set(label, 0);
  new DataView(out.buffer).setBigUint64(label.length, BigInt(issuedAtMs), true);
  out.set(body, label.length + 8);
  return out;
}

/** Have the host at `origin` keep a record (`json`) under `field`, signed by
 * `identity`'s root under `label`. */
async function keepSigned({origin, identity, path, field, label, json, now, fetcher}) {
  const signature = await identity.sign(rootStatement(label, now, json));
  const response = await fetcher(new URL(path, origin).href, {
    method: 'POST',
    headers: {'content-type': 'application/json'},
    body: JSON.stringify({[field]: json, issued_at_ms: now, signature: hex(signature)}),
    credentials: 'omit',
  });
  if (!response.ok) throw new IdentityError('publish-refused', (await response.text().catch(() => '')) || `host answered ${response.status}`);
}

/** The record of root `rootPublicKeyHex` at `path` from the first of
 * `origins` that keeps one `accept` takes, or `null`. A host answers
 * `{[field]: record}`, with `null` when it keeps none. */
// The record from the first of `origins` that keeps one; `null` when a host
// answered and none keeps one, `undefined` when no host answered at all, so
// a caller can tell "has none" from "could not ask".
async function fetchKept(path, field, rootPublicKeyHex, origins, accept, fetcher) {
  let answered = false;
  for (const origin of origins) {
    try {
      const response = await fetcher(new URL(path, origin).href, {
        method: 'POST', headers: {'content-type': 'text/plain'}, body: rootPublicKeyHex, credentials: 'omit',
      });
      if (!response.ok) continue;
      const record = (await response.json())?.[field] ?? null;
      answered = true;
      if (record !== null && accept(record)) return record;
    } catch {
      // An unreachable host: ask the next.
    }
  }
  return answered ? null : undefined;
}

/**
 * Have the host at `origin` keep `identity`'s envelope (ciphertext only), so
 * a new browser can fetch it there. The host keeps envelopes of its members
 * alone, and of each the newest the root signed.
 */
export async function publishEnvelope({origin, identity = currentIdentity(), envelope = identity?.envelope, now = Date.now(), fetch: fetcher = (...args) => globalThis.fetch(...args)}) {
  if (!identity?.sign || identity.locked) throw new IdentityError('locked', 'Unlock the identity first');
  if (!envelopeKept(envelope)) throw new IdentityError('not-kept', 'Only a kept identity has an envelope to publish');
  if (hex(rootPublicKeyOf(envelope)) !== identity.userPublicKey) throw new IdentityError('wrong-identity', 'This envelope belongs to another identity');
  await keepSigned({
    origin, identity, path: '/peer/v1/envelope', field: 'envelope', label: ENVELOPE_LABEL, json: JSON.stringify(envelope), now, fetcher,
  });
}

/** The envelope of root `rootPublicKeyHex` from the first of `origins` that
 * keeps it; `null` when they answered without one, `undefined` when none
 * answered. */
export async function fetchEnvelope(rootPublicKeyHex, origins, {fetch: fetcher = (...args) => globalThis.fetch(...args)} = {}) {
  return fetchKept('/peer/v1/envelope/fetch', 'envelope', rootPublicKeyHex, origins, envelope => envelope?.root_public_key === rootPublicKeyHex, fetcher);
}

/**
 * Have the host at `origin` keep `identity`'s profile: the name the person
 * chose, signed by their root, so each of their devices (paired, or signed
 * in on a new browser) shows it. Hosts keep profiles of their members alone,
 * and of each the newest the root signed. Needs no kept identity: a device
 * paired to this one reads it from the same hosts.
 */
export async function publishProfile({origin, identity = currentIdentity(), displayName, photo = null, status = '', sharing = '',
  photoFrom = null, linked = [], now = Date.now(), fetch: fetcher = (...args) => globalThis.fetch(...args)}) {
  if (!identity?.sign || identity.locked) throw new IdentityError('locked', 'Unlock the identity first');
  const name = fitDisplayName(displayName);
  if (!name) throw new IdentityError('malformed', 'A profile needs a name');
  // The picture they chose (a `data:` URL, `profile-photos.mjs`) rides
  // along; without one their initials show. So do what they say they are
  // doing and how they share it, which each of their devices shows, and
  // their linked accounts (`[{service, handle}]`), with the service whose
  // account the picture is (`photoFrom`) when they did not upload it.
  const named = (Array.isArray(linked) ? linked : []).slice(0, 4).map(({service, handle}) => ({service, handle}));
  const json = JSON.stringify({version: 1, user_public_key: identity.userPublicKey, display_name: name,
    ...(photo ? {photo} : {}), ...(status ? {status} : {}), ...(sharing ? {sharing} : {}),
    ...(photo && photoFrom ? {photo_from: photoFrom} : {}), ...(named.length ? {linked: named} : {})});
  await keepSigned({origin, identity, path: '/peer/v1/profile', field: 'profile', label: PROFILE_LABEL, json, now, fetcher});
}

/** The profile (`{version, user_public_key, display_name, photo?}`) of root
 * `rootPublicKeyHex` from the first of `origins` that keeps it; `null` when
 * they answered without one, `undefined` when none answered.
 * Hosts check the root's signature before keeping one. */
export async function fetchProfile(rootPublicKeyHex, origins, {fetch: fetcher = (...args) => globalThis.fetch(...args)} = {}) {
  return fetchKept('/peer/v1/profile/fetch', 'profile', rootPublicKeyHex, origins,
    profile => profile?.user_public_key === rootPublicKeyHex && typeof profile.display_name === 'string' && profile.display_name.length > 0, fetcher);
}

/** The wasm runtime, loaded through the page host. */
async function pageRuntime() {
  return (await import('./host.mjs')).loadRuntime();
}

/**
 * Certify another device with the unlocked root. `role` `node` (default): a
 * native node that hosts and governs the person's workspaces (relays, pins,
 * reads and owns them; admits no devices; one year). `browser`: a member
 * for 30 days. `session`: the session beside a node that runs the person's
 * apps and agents, a member for a year. `devicePublicKey` is the device's Ed25519 key (hex or
 * bytes), delivered out of band; never the root's own. Takes the key alone
 * or `{devicePublicKey, identity?, runtime?, label?, role?, encoding?}`;
 * `identity` defaults to the current one, `runtime` to the page host's.
 * Resolves to the certificate's canonical encoding as base64 (what a native
 * device keeps on disk), or with `encoding: 'json'` to its JSON.
 */
export async function certifyDevice(options) {
  if (typeof options === 'string' || ArrayBuffer.isView(options)) options = {devicePublicKey: options};
  const {identity = currentIdentity(), devicePublicKey, label = 'Tilo node', role = 'node', encoding = 'canonical'} = options ?? {};
  if (!identity?.sign || identity.locked) throw new IdentityError('locked', 'Unlock the identity first');
  const runtime = options.runtime ?? await pageRuntime();
  if (!runtime?.issueDeviceCert) throw new TypeError('certifyDevice needs the wasm runtime');
  const key = typeof devicePublicKey === 'string' ? devicePublicKey.toLowerCase() : hex(devicePublicKey ?? []);
  if (unhex(key).length !== 32) throw new IdentityError('malformed', 'A device public key is 32 bytes');
  const json = await runtime.issueDeviceCert(identity, key, label, role);
  if (encoding === 'json') return json;
  let binary = '';
  for (const byte of runtime.deviceCertCanonical(json)) binary += String.fromCharCode(byte);
  return btoa(binary);
}

/**
 * Hand a node waiting for its certificate (`tilo-relay run
 * --install-token-file`) the certificate `certifyDevice` issued for it, at
 * its HTTPS `origin`, with the install token it was provisioned with, and
 * with it `sessionCertificate`, the one issued for the session beside it
 * (see `nodeInstallInfo`). Resolves to `{device_public_key,
 * user_public_key}`; the node then starts serving as a host of this
 * identity on the same origin.
 */
export async function installNodeCertificate({origin, certificate, sessionCertificate, installToken, fetch: fetcher = (...args) => globalThis.fetch(...args)}) {
  if (!origin || !certificate || !installToken) throw new TypeError('origin, certificate and installToken are required');
  let body, json;
  if (sessionCertificate) {
    // Both in their canonical encoding, base64 (what certifyDevice returns).
    if (![certificate, sessionCertificate].every(canonicalBase64)) throw new TypeError('with a session, both certificates go as certifyDevice returns them (canonical, base64)');
    body = JSON.stringify({certificate, session_certificate: sessionCertificate});
    json = true;
  } else {
    // JSON, or the canonical encoding as base64 (what certifyDevice returns).
    json = typeof certificate !== 'string' || certificate.trimStart().startsWith('{');
    body = json
      ? (typeof certificate === 'string' ? certificate : JSON.stringify(certificate))
      : unbase64url(certificate.replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, ''));
  }
  const response = await fetcher(new URL('/peer/v1/install', origin).href, {
    method: 'POST',
    headers: {'content-type': json ? 'application/json' : 'application/octet-stream', 'x-tilo-install-token': installToken},
    body,
    credentials: 'omit',
  });
  const text = await response.text();
  if (response.status !== 200) {
    throw installRefusal(response.status, text);
  }
  return JSON.parse(text);
}

/** Why a node refused an install request: `install-token-refused` (403),
 * `not-waiting` (a host already serving, which answers no install route), or
 * `install-refused`. */
const installRefusal = (status, text) => new IdentityError(
  status === 403 ? 'install-token-refused' : [404, 405].includes(status) ? 'not-waiting' : 'install-refused',
  text || `node answered ${status}`);

const canonicalBase64 = certificate => typeof certificate === 'string' && /^[A-Za-z0-9+/]+=*$/.test(certificate);

/**
 * Ask a node waiting for its certificate at `origin`, with its install
 * token, what it runs: resolves to `{sessionPublicKey}`, the device key of
 * the session beside it that runs the person's apps and agents (or `null`
 * when it runs none). The node vouches for that key with its own, which
 * must be `devicePublicKey` (hex), the key the page got out of band; the
 * answer is refused otherwise.
 */
export async function nodeInstallInfo({origin, devicePublicKey, installToken, crypto, fetch: fetcher = (...args) => globalThis.fetch(...args)}) {
  if (!origin || !devicePublicKey || !installToken) throw new TypeError('origin, devicePublicKey and installToken are required');
  const env = environment({crypto});
  const response = await fetcher(new URL('/peer/v1/install', origin).href, {
    method: 'GET',
    headers: {'x-tilo-install-token': installToken},
    credentials: 'omit',
  });
  const text = await response.text();
  if (response.status !== 200) {
    throw installRefusal(response.status, text);
  }
  const info = JSON.parse(text);
  const node = devicePublicKey.toLowerCase();
  if (info?.device_public_key !== node) throw new IdentityError('install-refused', 'the node at this origin has another key');
  if (!info.session) return {sessionPublicKey: null};
  const session = unhex(info.session.device_public_key);
  const message = new Uint8Array([...new TextEncoder().encode('tilo node session v1\0'), ...unhex(node), ...session]);
  const key = await env.subtle.importKey('raw', unhex(node), {name: 'Ed25519'}, false, ['verify']);
  if (session.length !== 32 || !await env.subtle.verify({name: 'Ed25519'}, key, unhex(info.session.signature), message)) {
    throw new IdentityError('install-refused', 'the node did not vouch for its session');
  }
  return {sessionPublicKey: hex(session)};
}

/**
 * Certify the node waiting at `origin` (its key `devicePublicKey`, got out
 * of band) and the session beside it, and install both with the node's
 * `installToken`. Takes `identity`, `label` and `runtime` as `certifyDevice`
 * does; `onProgress({phase})` hears `checking`, `certifying` and
 * `installing` as each step starts. Resolves as `installNodeCertificate`.
 */
export async function installNode({origin, devicePublicKey, installToken, identity, label = 'Tilo node', runtime, fetch, onProgress = () => {}}) {
  onProgress({phase: 'checking'});
  const {sessionPublicKey} = await nodeInstallInfo({origin, devicePublicKey, installToken, fetch});
  onProgress({phase: 'certifying'});
  const certify = (key, role, name) => certifyDevice({devicePublicKey: key, identity, runtime, label: name, role});
  const certificate = await certify(devicePublicKey, 'node', label);
  const sessionCertificate = sessionPublicKey ? await certify(sessionPublicKey, 'session', `Apps on ${label}`) : undefined;
  onProgress({phase: 'installing'});
  return installNodeCertificate({origin, certificate, sessionCertificate, installToken, fetch});
}

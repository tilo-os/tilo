/**
 * The person this browser is: their Tilo root, kept here and, once they keep
 * it, openable anywhere by a passkey or a recovery key.
 *
 * The root is an Ed25519 key that never exists in the clear in this page: the
 * runtime (`pkg/`) generates it, seals it in a root envelope (a JSON document
 * safe to keep anywhere) and, in this browser, holds it wrapped under a
 * non-extractable `WebCrypto` key kept in `IndexedDB`. Both the envelope and
 * what its root signs are the shared crates' rules
 * (`tilo_user_identity::root_envelope`, `root_statement`), so a page seals
 * and opens exactly what a native device does; this module makes the
 * `WebAuthn` ceremonies (minting and asserting passkeys) and the HTTP that
 * carries the envelope and the profile to the person's hosts.
 *
 * The identity created or unlocked last is this page's current one: the
 * calls below that take an `identity` default to it.
 */

import {hex, unhex as decodeHex} from './hex.mjs';
import {isInvitedServer} from './site-host.mjs';
import {memberText} from './json-text.mjs';
import {deviceCodeOf, pairSecret} from './pairing.mjs';

const PRF_LABEL = new TextEncoder().encode('tilo-seal/v1/user-root-prf');
/** The one store the page keeps its person in: `device-key.mjs`, which holds
 * the tab's device key beside the person's root as the identity store's
 * `IdentityRecord`. */
const store = () => import('./device-key.mjs');
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

export {hex};
export const unhex = text => {
  try {
    return decodeHex(text);
  } catch {
    throw new IdentityError('malformed', 'expected hex');
  }
};

/** Bytes from bytes or a buffer (a copy). */
const bytesOf = value => (ArrayBuffer.isView(value)
  ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength).slice()
  : new Uint8Array(value).slice());
const random = length => globalThis.crypto.getRandomValues(new Uint8Array(length));
const credentialsOf = ({credentials = globalThis.navigator?.credentials} = {}) => credentials;
/** An envelope as the runtime takes it (JSON text) and as the page keeps it
 * (a plain document). */
const sealed = envelope => JSON.stringify(envelope);
const documentOf = text => JSON.parse(text);

/** The runtime the envelope's crypto runs in (the page host's, loaded on
 * first use: importing this module alone reads no bundle). */
const pageRuntime = async () => (await import('./host.mjs')).loadRuntime();

/** The PRF output a `WebAuthn` answer carries. */
const prfFirst = credential => {
  const first = credential?.getClientExtensionResults?.()?.prf?.results?.first;
  if (!first) return undefined;
  return bytesOf(first);
};

/** How soon a refusal is the place's and not the person's (`asked`). */
export const REFUSED_WITHIN_MS = 500;

/**
 * What a `WebAuthn` call (`ask()`) answers. A browser says the same
 * (`NotAllowedError`) of a person who closed its sheet and of a place where
 * it shows none, so time is what tells them apart: reading a sheet and
 * closing it takes a person a moment. Refused at once, with the page in
 * front, it is the place that cannot use passkeys (`passkeys-refused`): a web
 * view inside an app, which the system hands only the passkeys of the sites
 * that app is associated with, or a frame nobody gave the permission. A page
 * answers that with what to do instead. A refusal the person had time for
 * stays the browser's own error: it is their answer. `now` is the clock, in
 * milliseconds.
 */
async function asked(ask, now = () => globalThis.performance.now()) {
  // A page that is not in front is refused at once too, and may ask again.
  const inFront = globalThis.document?.hasFocus?.() !== false;
  const since = now();
  try {
    return await ask();
  } catch (error) {
    if (error?.name !== 'NotAllowedError' || !inFront || now() - since >= REFUSED_WITHIN_MS) throw error;
    const refused = new IdentityError('passkeys-refused', 'Passkeys cannot be used here');
    refused.cause = error;
    throw refused;
  }
}

/**
 * Ask the authenticator for PRF output: one assertion over `passkeys`
 * (`[{credentialId, prfSalt, rpId}]`, from the runtime), each evaluated with
 * its own PRF input so any of them can answer.
 */
async function assertPasskey(credentials, passkeys, rpId, now) {
  if (!credentials?.get) throw new IdentityError('passkeys-unavailable', 'Passkeys are unavailable here');
  const evalByCredential = Object.fromEntries(passkeys.map(({credentialId, prfSalt}) => [
    base64(credentialId), {first: bytesOf(prfSalt)},
  ]));
  const credential = await asked(() => credentials.get({
    publicKey: {
      challenge: random(32),
      rpId,
      allowCredentials: passkeys.map(({credentialId}) => ({type: 'public-key', id: bytesOf(credentialId)})),
      userVerification: 'required',
      extensions: {prf: {evalByCredential}},
    },
  }), now);
  if (!credential) throw new IdentityError('passkey-cancelled', 'No passkey was used');
  const output = prfFirst(credential);
  if (!output) throw new IdentityError('prf-unsupported', 'This passkey cannot unlock a Tilo identity (no PRF)');
  return {credentialId: new Uint8Array(credential.rawId), output};
}

/** `bytes` as the base64url text `WebAuthn` names credentials by. */
function base64(bytes) {
  let text = '';
  for (const byte of bytes) text += String.fromCharCode(byte);
  return btoa(text).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/** Create a passkey for `options` and read its PRF output for `salt`. */
async function createPasskey(credentials, {rpId, rpName = 'Tilo', userName, userId, displayName, now}, salt, exclude = []) {
  if (!credentials?.create) throw new IdentityError('passkeys-unavailable', 'Passkeys are unavailable here');
  if (!rpId || !userName || !userId) throw new TypeError('rpId, userName and userId are required');
  const credential = await asked(() => credentials.create({
    publicKey: {
      rp: {id: rpId, name: rpName},
      user: {id: new TextEncoder().encode(String(userId)).slice(0, 64), name: userName, displayName: displayName ?? userName},
      challenge: random(32),
      pubKeyCredParams: [{type: 'public-key', alg: -8}, {type: 'public-key', alg: -7}, {type: 'public-key', alg: -257}],
      authenticatorSelection: {residentKey: 'required', userVerification: 'required'},
      // An authenticator that already holds one of this identity's passkeys
      // declines, rather than replacing it (same user handle).
      excludeCredentials: exclude.map(id => ({type: 'public-key', id: bytesOf(id)})),
      extensions: {prf: {eval: {first: bytesOf(salt)}}},
    },
  }), now);
  if (!credential) throw new IdentityError('passkey-cancelled', 'No passkey was created');
  const credentialId = new Uint8Array(credential.rawId);
  const prf = credential.getClientExtensionResults?.()?.prf;
  // Some authenticators evaluate the PRF only on assertion.
  let output = prfFirst(credential);
  if (!output && prf?.enabled !== false) {
    output = (await assertPasskey(credentials, [{credentialId, prfSalt: salt, rpId}], rpId, now)).output;
  }
  if (!output) throw new IdentityError('prf-unsupported', 'This passkey cannot unlock a Tilo identity (no PRF)');
  return {credentialId, output};
}

/**
 * `envelope` with one more passkey wrap: a new passkey on an authenticator
 * that holds none of the envelope's. `rpId` and the PRF input default to the
 * envelope's first passkey's, so one login can evaluate the PRF without
 * knowing which passkey answers; `userName`/`userId` to `options`.
 */
async function withPasskey(identity, envelope, options = {}) {
  const runtime = await pageRuntime();
  const passkeys = runtime.rootEnvelopePasskeys(sealed(envelope));
  const first = passkeys[0];
  const rpId = options.rpId ?? first?.rpId;
  const prfSalt = options.prfSalt === undefined ? first?.prfSalt ?? random(32) : bytesOf(options.prfSalt);
  const exclude = passkeys.filter(passkey => passkey.rpId === rpId).map(passkey => passkey.credentialId);
  const {credentialId, output} = await createPasskey(credentialsOf(options), {
    // The passkey's user handle is the root public key: a passkey picked on
    // a new browser names the person whose envelope to fetch.
    rpName: 'Tilo', userName: 'Tilo', ...options, rpId,
    userId: options.userId ?? identity.userPublicKey,
  }, prfSalt, exclude);
  return documentOf(await identity.wrapPasskey(sealed(envelope), credentialId, rpId, prfSalt, output));
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
 * @param options.prfSalt  the PRF input (32 bytes), default random; later
 *   passkeys reuse it, so a login can evaluate the PRF with one known input
 *   (`extensions.prf.eval`) whichever passkey answers
 */
export async function createIdentity(options = {}) {
  const runtime = await pageRuntime();
  const identity = await runtime.rootNew();
  let envelope = documentOf(runtime.rootEnvelopeEmpty(identity.publicKey));
  if (options.passkey !== false) {
    const prfSalt = options.prfSalt === undefined ? random(32) : bytesOf(options.prfSalt);
    if (prfSalt.length !== 32) throw new TypeError('prfSalt must be 32 bytes');
    const {credentialId, output} = await createPasskey(credentialsOf(options), options, prfSalt);
    envelope = documentOf(await identity.wrapPasskey(sealed(envelope), credentialId, options.rpId, prfSalt, output));
  }
  const wrapped = await identity.wrapRecoveryKey(sealed(envelope));
  current = identity;
  return {envelope: documentOf(wrapped.envelope), recoveryKey: wrapped.recoveryKey, identity};
}

/**
 * Unlock `envelope` with a passkey (`{passkey: true, rpId?}`: one assertion
 * over every passkey wrap of `rpId`, default the first wrap's), with the PRF
 * output of an assertion the page ran itself (`{prfOutput, credentialId}`,
 * bytes; evaluated with the wrap's `prf_salt`), or with `{recoveryKey}`.
 * Resolves to the unlocked identity, current from now on.
 */
export async function unlockIdentity(envelope, how = {passkey: true}) {
  const runtime = await pageRuntime();
  let identity;
  if (how.recoveryKey !== undefined) {
    identity = await runtime.rootOpenRecovery(sealed(envelope), String(how.recoveryKey));
  } else if (how.prfOutput !== undefined) {
    identity = await runtime.rootOpenPasskey(sealed(envelope), bytesOf(how.credentialId ?? []), bytesOf(how.prfOutput));
  } else if (how.passkey) {
    const passkeys = runtime.rootEnvelopePasskeys(sealed(envelope));
    const rpId = how.rpId ?? passkeys[0]?.rpId;
    const candidates = passkeys.filter(passkey => passkey.rpId === rpId);
    if (!candidates.length) throw new IdentityError('no-wrap', 'This identity has no passkey here');
    const {credentialId, output} = await assertPasskey(credentialsOf(how), candidates, rpId, how.now);
    identity = await runtime.rootOpenPasskey(sealed(envelope), credentialId, output);
  } else {
    throw new TypeError('unlock with {passkey: true}, {prfOutput, credentialId} or {recoveryKey}');
  }
  current = identity;
  return identity;
}

/** `envelope` with a new passkey wrap, from `options.identity` (default the
 * current identity); see `withPasskey` for the defaults. */
export async function addPasskey(envelope, {identity = currentIdentity(), ...options} = {}) {
  if (!identity?.wrapPasskey) throw new IdentityError('locked', 'addPasskey needs an unlocked identity');
  return withPasskey(identity, envelope, options);
}

/** Whether `envelope` opens the root anywhere else: a passkey or a recovery
 * key wraps it. */
export async function envelopeKept(envelope) {
  return (await pageRuntime()).rootEnvelopeKept(sealed(envelope));
}

/** The PRF input of every passkey that keeps a browser's person: one known
 * value, so signing in on a new browser evaluates the PRF in the same
 * assertion that names the person, before their envelope is fetched. */
async function browserPrfSalt() {
  return new Uint8Array(await globalThis.crypto.subtle.digest('SHA-256', PRF_LABEL));
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
 * The person `kept` keeps, unlocked and made this page's current identity.
 * `kept` is what the one store hands back (`readPerson`): an `IdentityRecord`
 * whose seed is the root (the bytes a computer reads for the same person), the
 * envelope that opens it elsewhere, and the revocations its root signed. Adds
 * `envelope`/`kept` (following what is kept), the two ways to keep it, and
 * `revocations`/`revokeDevice`, each saved beside the record as it lands.
 * Keeping it asks the browser to keep this site's storage through `persist`
 * (`askToKeepStorage`).
 */
async function openKept(storage, kept, persist, runtime) {
  runtime ??= await pageRuntime();
  const {readPerson, writePerson, rootOfPerson} = await store();
  let {record, envelope, revocations} = kept;
  const {seed, publicKey} = rootOfPerson(record);
  const identity = await runtime.rootHeld(seed, hex(publicKey));
  // A person made before any envelope existed opens with an empty one, kept
  // here once they keep it elsewhere.
  envelope ??= documentOf(runtime.rootEnvelopeEmpty(publicKey));
  const save = async next => {
    envelope = next;
    await writePerson(storage, {record, envelope, revocations});
    askToKeepStorage(persist);
    return next;
  };
  return Object.defineProperties(identity, Object.getOwnPropertyDescriptors({
    get envelope() { return envelope; },
    get kept() { return runtime.rootEnvelopeKept(sealed(envelope)); },
    /** Add a passkey for `rpId` (default this page's host) that opens this
     * person; `userName` is what the authenticator lists it as. Resolves to
     * the new envelope, also kept here. */
    async keepWithPasskey({rpId = globalThis.location?.hostname, userName, displayName, credentials, now} = {}) {
      const options = {rpId, prfSalt: await browserPrfSalt(), credentials, now};
      if (userName) Object.assign(options, {userName, displayName: displayName ?? userName});
      return save(await withPasskey(identity, envelope, options));
    },
    /** A new recovery key, replacing any earlier one; resolves to the key
     * as the person writes it down (shown once, never stored here). */
    async createRecoveryKey() {
      const sealedEnvelope = await identity.wrapRecoveryKey(sealed(envelope));
      await save(documentOf(sealedEnvelope.envelope));
      return sealedEnvelope.recoveryKey;
    },
    /** The revocations this person's root signed, as they were kept here:
     * one per key it no longer trusts, which every session of theirs
     * announces to its host. */
    get revocations() { return revocations ?? []; },
    /** The root revokes `keyHex` — one of this person's devices, or the
     * stand-in approver a host holds for them — from now on: resolves to the
     * entry it signed, which is kept here beside the person and reaches
     * every session of theirs, the open ones at once and later ones as they
     * start. */
    async revokeDevice(keyHex, reason = 'retired') {
      const entry = JSON.parse(await runtime.revokeDevice(identity, keyHex, reason));
      revocations = [...(revocations ?? []), entry];
      await writePerson(storage, {record, envelope, revocations});
      return entry;
    },
  }));
}

/**
 * The person this browser is. The first call creates one: a user root kept as
 * the identity store's `IdentityRecord` beside the tab's device key
 * (`device-key.mjs`), the same record a computer reads for the same person. It
 * is a real identity from the start: it joins and owns workspaces like any
 * other, and keeping it (`keepWithPasskey`, `createRecoveryKey`) only adds
 * ways to open the same root elsewhere. Creating one asks the browser
 * for nothing: a page that loads never waits on a permission prompt; the
 * site's storage is asked to be kept once the person keeps it
 * (`askToKeepStorage`, through `persist`).
 *
 * Resolves to the identity with `{envelope, kept, keepWithPasskey,
 * createRecoveryKey, revocations, revokeDevice}`, or `null` with
 * `create: false` and none kept.
 */
export async function browserIdentity({storage, create = true, persist = globalThis.navigator?.storage} = {}) {
  const {idbStorage, readPerson, writePerson, personRecord} = await store();
  storage ??= idbStorage();
  const runtime = await pageRuntime();
  let kept = await readPerson(storage);
  if (!kept) {
    if (!create) return null;
    const identity = await runtime.rootNew();
    const record = personRecord({seed: identity.seed, publicKey: identity.publicKey});
    kept = {record, envelope: documentOf(runtime.rootEnvelopeEmpty(identity.publicKey)), revocations: []};
    // Kept from the start: the same person on the next load, and in every
    // other tab of this site.
    await writePerson(storage, kept);
  }
  const identity = await openKept(storage, kept, persist);
  current = identity;
  return identity;
}

/**
 * The person this browser keeps, without opening them: their root key, hex,
 * or `null` where none is kept. Opening one reads the runtime, and a page
 * that only has to say whether there is anyone here (`sign-in.mjs`, before
 * the person agrees to anything) asks this instead.
 */
export async function keptIdentity({storage} = {}) {
  const {idbStorage, readPerson} = await store();
  storage ??= idbStorage();
  const kept = await readPerson(storage);
  return kept ? {rootPublicKey: hex(kept.record.threshold_identity.group_public_key)} : null;
}

/**
 * The person this browser joins workspaces as: the identity unlocked last,
 * else this browser's own (`browserIdentity`, created on the first visit).
 * Every caller that joins a workspace asks here.
 */
export async function browserPerson(options) {
  return currentIdentity() ?? browserIdentity(options);
}

/**
 * The 24 words that restore `identity`'s root, in order, as the identity
 * store writes them (`tilo_identity::store::recovery_words`): the root's seed
 * in BIP39 English, so the words alone bring this person back on a device of
 * theirs. The root never leaves this browser: the runtime derives the words
 * here, and the page shows them to the person to keep.
 */
export function recoveryWords(identity = currentIdentity()) {
  if (!identity?.recoveryWords) throw new IdentityError('locked', 'There is no identity to write down');
  return identity.recoveryWords;
}

/**
 * The root public key (hex) the 24 `words` restore: the words read back to a
 * root and its public key derived again, so a person can check a copy before
 * relying on it, or a device can be sure a phrase is one person's. Reads the
 * same BIP39 words as `recoveryWords`.
 */
export async function recoveryWordsPublicKey(words) {
  const runtime = await pageRuntime();
  return runtime.rootWordsPublicKey(String(words));
}

/**
 * Whether this browser has already shown its person the 24 words. They are
 * shown once, right after it is first let in; a browser that keeps no person
 * has shown none.
 */
export async function recoveryWordsShown({storage} = {}) {
  const {idbStorage, PERSON_RECOVERY_SHOWN_KEY} = await store();
  storage ??= idbStorage();
  return (await storage.get(PERSON_RECOVERY_SHOWN_KEY)) != null;
}

/** Keep that this browser showed its person their 24 words. */
export async function keepRecoveryWordsShown({storage} = {}) {
  const {idbStorage, PERSON_RECOVERY_SHOWN_KEY} = await store();
  storage ??= idbStorage();
  await storage.put(PERSON_RECOVERY_SHOWN_KEY, '1');
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
 * `browserIdentity` keeps it, replacing whoever this browser was. Where the
 * place cannot use passkeys at all it rejects with `passkeys-refused`
 * (`asked`; `now` is its clock), which is not the person's answer.
 */
export async function signInWithPasskey({rpId = globalThis.location?.hostname, fetchEnvelope: find, storage, credentials, persist = globalThis.navigator?.storage, runtime, now} = {}) {
  if (typeof find !== 'function') throw new TypeError('signInWithPasskey needs fetchEnvelope');
  const {idbStorage, writePerson, personRecord} = await store();
  storage ??= idbStorage();
  runtime ??= await pageRuntime();
  credentials = credentialsOf({credentials});
  if (!credentials?.get) throw new IdentityError('passkeys-unavailable', 'Passkeys are unavailable here');
  const salt = await browserPrfSalt();
  const credential = await asked(() => credentials.get({
    publicKey: {challenge: random(32), rpId, userVerification: 'required', extensions: {prf: {eval: {first: salt}}}},
  }), now);
  if (!credential) throw new IdentityError('passkey-cancelled', 'No passkey was used');
  const output = prfFirst(credential);
  if (!output) throw new IdentityError('prf-unsupported', 'This passkey cannot unlock a Tilo identity (no PRF)');
  try {
    const handle = credential.response?.userHandle;
    const rootHex = handle ? new TextDecoder().decode(handle) : '';
    if (!/^[0-9a-f]{64}$/.test(rootHex)) throw new IdentityError('not-tilo', 'This passkey does not name a Tilo identity');
    const envelope = await find(rootHex);
    if (!envelope) throw new IdentityError('no-envelope', 'No host this browser knows keeps this identity');
    if (runtime.rootEnvelopePublicKey(sealed(envelope)) !== rootHex) throw new IdentityError('wrong-identity', 'The host answered with another identity');
    const root = await runtime.rootOpenPasskey(sealed(envelope), new Uint8Array(credential.rawId), output);
    currentIdentity()?.lock();
    // Kept as this browser's person, replacing whoever it was: the page loads
    // again as this person, and every other tab of the site is them too.
    const kept = {record: personRecord({seed: root.seed, publicKey: unhex(rootHex)}), envelope, revocations: []};
    await writePerson(storage, kept);
    const signedIn = await openKept(storage, kept, persist, runtime);
    current = signedIn;
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

/** Have the host at `origin` keep a record (`json`) under `field`, signed by
 * `identity`'s root as the runtime's `record` statement. */
async function keepSigned({origin, identity, path, record, field, json, now, fetcher}) {
  const signature = await identity.signRecord(record, json, now);
  const response = await fetcher(new URL(path, origin).href, {
    method: 'POST',
    headers: {'content-type': 'application/json'},
    body: JSON.stringify({[field]: json, issued_at_ms: now, signature: hex(signature)}),
    credentials: 'omit',
  });
  if (!response.ok) throw new IdentityError('publish-refused', (await response.text().catch(() => '')) || `host answered ${response.status}`);
}

/** The record of root `rootPublicKeyHex` at `path` from the first of
 * `origins` that keeps one `accept` takes, or `null`; `undefined` when no
 * host answered at all, so a caller can tell "has none" from "could not
 * ask". A host answers `{[field]: record}`, with `null` when it keeps none. */
async function fetchKept(path, field, rootPublicKeyHex, origins, accept, fetcher) {
  let answered = false;
  for (const origin of origins) {
    // A server only an invitation named answers no HTTP: its origin never resolves.
    if (isInvitedServer(origin)) continue;
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
  const runtime = await pageRuntime();
  if (!runtime.rootEnvelopeKept(sealed(envelope))) throw new IdentityError('not-kept', 'Only a kept identity has an envelope to publish');
  if (runtime.rootEnvelopePublicKey(sealed(envelope)) !== identity.userPublicKey) throw new IdentityError('wrong-identity', 'This envelope belongs to another identity');
  await keepSigned({
    origin, identity, path: '/peer/v1/envelope', record: 'envelope', field: 'envelope', json: JSON.stringify(envelope), now, fetcher,
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
  await keepSigned({origin, identity, path: '/peer/v1/profile', record: 'profile', field: 'profile', json, now, fetcher});
}

/** The profile (`{version, user_public_key, display_name, photo?}`) of root
 * `rootPublicKeyHex` from the first of `origins` that keeps it; `null` when
 * they answered without one, `undefined` when none answered.
 * Hosts check the root's signature before keeping one. */
export async function fetchProfile(rootPublicKeyHex, origins, {fetch: fetcher = (...args) => globalThis.fetch(...args)} = {}) {
  return fetchKept('/peer/v1/profile/fetch', 'profile', rootPublicKeyHex, origins,
    profile => profile?.user_public_key === rootPublicKeyHex && typeof profile.display_name === 'string' && profile.display_name.length > 0, fetcher);
}

/**
 * Certify another device of the person with the unlocked root: a member
 * device that admits no devices. `role` `node` (default) or `session`: a
 * machine the person set up, for a year; `browser`: for 30 days. `devicePublicKey` is the device's Ed25519 key (hex or
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
 * The host at `origin`, as this page signs its person's tabs in there: what calls its ticket
 * actions (`ticketAction`), kept for the page. A page that lists what waits there, and signs one
 * in, calls with the person's root; the host checks.
 */
let approvalHosts;
export async function approvalHost(origin, {fetch: fetcher} = {}) {
  approvalHosts ??= new Map();
  const kept = approvalHosts.get(origin);
  if (kept) return kept;
  const made = (async () => {
    // What signs a tab in is the peer module's (`../src/split.rs`): the
    // page that answers a sign-in is already entering.
    const runtime = await (await import('./host.mjs')).loadRuntime('peer', 'tickets');
    const post = async (path, body) => {
      const response = await (fetcher ?? ((...args) => globalThis.fetch(...args)))(new URL(path, origin).href, {
        method: 'POST', body, credentials: 'omit', cache: 'no-store', redirect: 'error',
      });
      return {status: response.status, body: new Uint8Array(await response.arrayBuffer())};
    };
    /** Call the ticket action `action` with `body` (or its JSON text), signed with `root`: its
     * answer as JSON text, as the host wrote it (a certificate's times are more than a number
     * holds). */
    const actText = async (action, body, root) => {
      const text = typeof body === 'string' ? body : JSON.stringify(body);
      const reply = await runtime.ticketAction(post, action, text, root);
      const refused = JSON.parse(reply)?.refused;
      if (typeof refused === 'string') throw new IdentityError('refused', refused);
      return memberText(reply, 'value') ?? 'null';
    };
    /** The same, its answer read. */
    const act = async (action, body, root) => JSON.parse(await actText(action, body, root));
    return {runtime, act, actText};
  })();
  approvalHosts.set(origin, made);
  made.catch(() => { if (approvalHosts.get(origin) === made) approvalHosts.delete(origin); });
  return made;
}

/**
 * The tabs waiting at `host` to be signed in as devices of the person whose root is `root`: each
 * `{digest, label, device, capability}`, `digest` being what `answerSignIn` names it by.
 */
export async function signInsWaiting(host, root) {
  const listed = await host.act('pending', {user: root.userPublicKey}, root);
  return (listed?.waiting ?? []).map(waiting => ({digest: waiting.secret, label: waiting.label,
    device: waiting.device, capability: waiting.capability, signIn: true}));
}

/**
 * Sign in (`approved`) the waiting tab `request` (as `signInsWaiting` lists it) with `root`, the
 * person's root: it signs the tab's certificate, and the tab reads it when it looks next.
 * Resolves the certificate (JSON), or null when turned away, which leaves the tab's ticket to
 * run out.
 */
export async function answerSignIn(host, root, request, approved) {
  if (!approved) return null;
  const cert = await host.runtime.signInCertificate(root, request.device, request.label);
  // The certificate goes as the root signed it: its nanosecond times are more than a number holds.
  await host.act('approve', `{"user":${JSON.stringify(root.userPublicKey)},"secret":${JSON.stringify(request.digest)},`
    + `"cert":${cert},"label":${JSON.stringify(request.label ?? '')}}`, root);
  return cert;
}

/**
 * The pairing links a page holding the person's root (`root`) makes on the ticket actions of
 * `host` (`approvalHost`), as `createIdentityActions` takes them (`pairing`): each a login ticket
 * the host serves for a browser, which only this page answers. `peer` is this page's own
 * certified peer, which signs its approval (`approveAsked`); `workspace()` names the workspace a
 * device the link adds enters (`null`: none, and no link), `link(pair)` the address that carries
 * the link's `pair` secret.
 */
export function deviceLinks({host, peer, root, workspace, name = () => null, link, origin = null,
  site = globalThis.location?.origin ?? null}) {
  return {
    async issue() {
      const entering = workspace();
      if (!entering) return null;
      const minted = await host.act('issue', {user: root.userPublicKey, workspace: entering, name: name(),
        browser: true, site, host_origin: origin}, root);
      return {id: minted.id, link: link(pairSecret(root.userPublicKey, minted.ticket))};
    },
    async follow(id) {
      const status = await host.actText('follow', {id}, root);
      const asked = memberText(status, 'asked');
      if (asked === undefined) return null;
      const {device_public_key: key, label} = JSON.parse(asked);
      const device = hex(Uint8Array.from(key));
      return {label, device, pairingCode: deviceCodeOf(device), asked};
    },
    async answer(id, request, approved) {
      if (!approved) {
        await host.act('answer', {id, declined: 'Your other device declined.'}, root);
        return null;
      }
      // Signed over the question exactly as the host asked it, and handed back as signed.
      const approval = await peer.approveAsked(root, request.asked);
      await host.act('answer', `{"id":${JSON.stringify(id)},"approved":${approval}}`, root);
      return approval;
    },
  };
}

/**
 * Hand a node waiting for its certificate (`tilo-server run
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

const unbase64url = text => {
  const binary = atob(text.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - text.length % 4) % 4));
  return Uint8Array.from(binary, char => char.charCodeAt(0));
};

/**
 * Ask a node waiting for its certificate at `origin`, with its install
 * token, what it runs: resolves to `{sessionPublicKey}`, the device key of
 * the session beside it that runs the person's apps and agents (or `null`
 * when it runs none). The runtime (`options.runtime`, default the page
 * host's) checks that the node vouched for that key with its own, which
 * must be `devicePublicKey` (hex), the key the page got out of band; the
 * answer is refused otherwise.
 */
export async function nodeInstallInfo({origin, devicePublicKey, installToken, runtime, fetch: fetcher = (...args) => globalThis.fetch(...args)}) {
  if (!origin || !devicePublicKey || !installToken) throw new TypeError('origin, devicePublicKey and installToken are required');
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
  // The node signs the two keys together (`tilo-server-protocol`'s
  // `node_session_message`): only a session its own key vouches for is one
  // this person certifies.
  const session = String(info.session.device_public_key).toLowerCase();
  runtime ??= await pageRuntime();
  if (!runtime.nodeSessionVouched(node, session, String(info.session.signature))) {
    throw new IdentityError('install-refused', 'the node did not vouch for its session');
  }
  return {sessionPublicKey: session};
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
  const {sessionPublicKey} = await nodeInstallInfo({origin, devicePublicKey, installToken, runtime, fetch});
  onProgress({phase: 'certifying'});
  const certify = (key, role, name) => certifyDevice({devicePublicKey: key, identity, runtime, label: name, role});
  const certificate = await certify(devicePublicKey, 'node', label);
  const sessionCertificate = sessionPublicKey ? await certify(sessionPublicKey, 'session', `Apps on ${label}`) : undefined;
  onProgress({phase: 'installing'});
  return installNodeCertificate({origin, certificate, sessionCertificate, installToken, fetch});
}

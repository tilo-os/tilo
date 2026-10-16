import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {
  addPasskey, base64url, browserIdentity, certifyDevice, createIdentity, currentIdentity, fetchEnvelope, fetchProfile, fitDisplayName, hex, MAX_DISPLAY_NAME_BYTES, publishEnvelope, publishProfile, signInWithPasskey, installNode, installNodeCertificate, formatRecoveryKey, IdentityError, nodeInstallInfo,
  parseRecoveryKey, unbase64url, unhex, unlockIdentity,
} from './root-identity.mjs';
import {fakeAuthenticator} from './testing/fake-authenticator.mjs';

const FIXTURE = new URL('../../../collab/tilo-user-identity/tests/fixtures/root-envelope.json', import.meta.url);
const OPTIONS = {rpId: 'tilo.so', userName: 'ada@example.com', userId: 'user-1'};

async function signsAsRoot(identity, publicKeyHex) {
  const message = new TextEncoder().encode('certificate');
  const signature = await identity.sign(message);
  const key = await crypto.subtle.importKey('raw', unhex(publicKeyHex), {name: 'Ed25519'}, false, ['verify']);
  return crypto.subtle.verify({name: 'Ed25519'}, key, signature, message);
}

test('a new identity opens with its passkey or its recovery key', async () => {
  const credentials = fakeAuthenticator();
  const {envelope, recoveryKey, identity} = await createIdentity({...OPTIONS, credentials});
  assert.equal(envelope.version, 1);
  assert.deepEqual(envelope.wraps.map(wrap => wrap.kind), ['passkey', 'recovery']);
  assert.equal(envelope.wraps[0].rp_id, 'tilo.so');
  assert.match(recoveryKey, /^([0-9A-HJKMNP-TV-Z]{4}-){7}[0-9A-HJKMNP-TV-Z]{4}$/);
  assert.equal(identity.userPublicKey, envelope.root_public_key);
  assert.ok(await signsAsRoot(identity, envelope.root_public_key));
  // Nothing secret in the stored form.
  assert.ok(!JSON.stringify(envelope).includes(recoveryKey.replace(/-/g, '')));

  const byPasskey = await unlockIdentity(envelope, {passkey: true, credentials});
  assert.ok(await signsAsRoot(byPasskey, envelope.root_public_key));
  const byRecovery = await unlockIdentity(envelope, {recoveryKey: recoveryKey.toLowerCase().replace(/-/g, ' ')});
  assert.ok(await signsAsRoot(byRecovery, envelope.root_public_key));

  byRecovery.lock();
  await assert.rejects(byRecovery.sign(new Uint8Array(1)), {code: 'locked'});
});

test('an authenticator that reports PRF only on assertion still wraps the root', async () => {
  const credentials = fakeAuthenticator({prfAtCreate: false});
  const {envelope} = await createIdentity({...OPTIONS, credentials});
  assert.deepEqual(credentials.log.map(entry => entry.op), ['create', 'get']);
  const identity = await unlockIdentity(envelope, {passkey: true, credentials});
  assert.ok(await signsAsRoot(identity, envelope.root_public_key));
});

test('a chosen PRF input is used', async () => {
  const prfSalt = base64url(new Uint8Array(32).fill(5));
  const {envelope} = await createIdentity({...OPTIONS, prfSalt, credentials: fakeAuthenticator()});
  assert.equal(envelope.wraps[0].prf_salt, prfSalt);
  await assert.rejects(createIdentity({...OPTIONS, prfSalt: 'AAAA', credentials: fakeAuthenticator()}), TypeError);
});

test('a passkey without PRF is refused before anything is stored', async () => {
  await assert.rejects(createIdentity({...OPTIONS, credentials: fakeAuthenticator({prf: false})}), {code: 'prf-unsupported'});
  const {envelope} = await createIdentity({...OPTIONS, passkey: false});
  assert.deepEqual(envelope.wraps.map(wrap => wrap.kind), ['recovery']);
});

test('another passkey can be added and opens alone', async () => {
  const credentials = fakeAuthenticator();
  const {envelope, identity} = await createIdentity({...OPTIONS, credentials});
  const more = await addPasskey(envelope, {identity, ...OPTIONS, credentials});
  assert.deepEqual(credentials.log.at(-1).exclude, [envelope.wraps[0].credential_id]);
  assert.equal(more.wraps.length, 3);
  assert.deepEqual(more.wraps.slice(0, 2), envelope.wraps);
  credentials.remove(unbase64url(envelope.wraps[0].credential_id));
  const unlocked = await unlockIdentity(more, {passkey: true, credentials});
  assert.ok(await signsAsRoot(unlocked, envelope.root_public_key));
  await assert.rejects(unlockIdentity(envelope, {passkey: true, credentials}), /no passkey/);

  const other = await createIdentity({...OPTIONS, passkey: false});
  await assert.rejects(addPasskey(envelope, {identity: other.identity, ...OPTIONS, credentials}), {code: 'wrong-identity'});
});

test('the root stays non-extractable after unlocking', async () => {
  const {envelope, recoveryKey} = await createIdentity({...OPTIONS, passkey: false});
  const identity = await unlockIdentity(envelope, {recoveryKey});
  assert.equal(Object.keys(identity).some(key => /private|seed|secret/i.test(key)), false);
  assert.ok(!('privateKey' in identity));
});

test('a tampered envelope or a wrong key does not open', async () => {
  const credentials = fakeAuthenticator();
  const {envelope, recoveryKey} = await createIdentity({...OPTIONS, credentials});
  const other = await createIdentity({...OPTIONS, passkey: false});

  await assert.rejects(unlockIdentity(envelope, {recoveryKey: other.recoveryKey}), {code: 'rejected'});
  const swapped = {...envelope, root_public_key: other.envelope.root_public_key};
  await assert.rejects(unlockIdentity(swapped, {recoveryKey}), {code: 'rejected'});
  const flip = wrap => {
    const bytes = unbase64url(wrap.ciphertext);
    bytes[5] ^= 1;
    return {...wrap, ciphertext: base64url(bytes)};
  };
  await assert.rejects(unlockIdentity({...envelope, wraps: envelope.wraps.map(flip)}, {recoveryKey}), {code: 'rejected'});
  await assert.rejects(unlockIdentity({...envelope, wraps: envelope.wraps.map(flip)}, {passkey: true, credentials}), {code: 'rejected'});
  const resalted = envelope.wraps.map(wrap => (wrap.kind === 'passkey' ? {...wrap, prf_salt: base64url(new Uint8Array(32))} : wrap));
  await assert.rejects(unlockIdentity({...envelope, wraps: resalted}, {passkey: true, credentials}), {code: 'rejected'});
  await assert.rejects(unlockIdentity({...envelope, version: 2}, {recoveryKey}), {code: 'unsupported'});
  await assert.rejects(unlockIdentity(envelope, {recoveryKey: 'ABCD'}), {code: 'recovery-key-malformed'});
  assert.ok(new IdentityError('x', 'y') instanceof Error);
});

test('recovery keys read back with the usual confusions', () => {
  const bytes = Uint8Array.from({length: 20}, (_, index) => index * 13);
  const shown = formatRecoveryKey(bytes);
  assert.equal(shown.length, 39);
  assert.deepEqual(parseRecoveryKey(shown.toLowerCase()), bytes);
  assert.deepEqual(parseRecoveryKey('0000-OOOO-1111-IIII-LLLL-llll-oooo-0000'),
    parseRecoveryKey('0000-0000-1111-1111-1111-1111-0000-0000'));
  assert.throws(() => parseRecoveryKey(`${shown}U`), {code: 'recovery-key-malformed'});
});

test('an envelope stored earlier still opens (native code reads the same one)', async () => {
  const fixture = JSON.parse(readFileSync(FIXTURE, 'utf8'));
  const byRecovery = await unlockIdentity(fixture.envelope, {recoveryKey: fixture.recoveryKey});
  assert.ok(await signsAsRoot(byRecovery, fixture.envelope.root_public_key));
  const credentials = fakeAuthenticator({seed: fixture.authenticatorSeed});
  // Recreate the fixture's passkey (same seed, same order) to answer the assertion.
  await credentials.create({publicKey: {rp: {id: fixture.envelope.wraps[0].rp_id}, extensions: {}}});
  const byPasskey = await unlockIdentity(fixture.envelope, {passkey: true, credentials});
  assert.ok(await signsAsRoot(byPasskey, fixture.envelope.root_public_key));
});

test('certifying a device hands the wasm runtime the root as a signer', async () => {
  const {identity} = await createIdentity({...OPTIONS, passkey: false});
  const seen = [];
  const runtime = {
    issueDeviceCert: async (...args) => { seen.push(args); return '{"cert":1}'; },
    deviceCertCanonical: json => (json === '{"cert":1}' ? Uint8Array.from([0xfb, 0xff, 1]) : undefined),
  };
  const node = 'AB'.repeat(32);
  assert.equal(await certifyDevice({identity, runtime, devicePublicKey: node}), '+/8B', 'canonical, base64');
  assert.equal(await certifyDevice({identity, runtime, devicePublicKey: node, encoding: 'json'}), '{"cert":1}');
  assert.deepEqual(seen[0].slice(1), ['ab'.repeat(32), 'Tilo node', 'node']);
  assert.equal(seen[0][0], identity);
  await certifyDevice({identity, runtime, devicePublicKey: new Uint8Array(32).fill(1), label: 'Phone', role: 'browser'});
  assert.deepEqual(seen[2].slice(1), ['01'.repeat(32), 'Phone', 'browser']);
  await assert.rejects(certifyDevice({identity, runtime, devicePublicKey: 'abcd'}), {code: 'malformed'});
  identity.lock();
  await assert.rejects(certifyDevice({identity, runtime, devicePublicKey: node}), {code: 'locked'});
});

test('an assertion the page ran itself unlocks with its PRF output', async () => {
  const credentials = fakeAuthenticator();
  const {envelope} = await createIdentity({...OPTIONS, credentials});
  const wrap = envelope.wraps[0];
  const assertion = await credentials.get({publicKey: {
    rpId: 'tilo.so', allowCredentials: [{type: 'public-key', id: unbase64url(wrap.credential_id)}],
    extensions: {prf: {evalByCredential: {[wrap.credential_id]: {first: unbase64url(wrap.prf_salt)}}}},
  }});
  const prfOutput = assertion.getClientExtensionResults().prf.results.first;
  const identity = await unlockIdentity(envelope, {prfOutput, credentialId: assertion.rawId});
  assert.ok(await signsAsRoot(identity, envelope.root_public_key));
  const again = await unlockIdentity(envelope, {prfOutput: base64url(new Uint8Array(prfOutput)), credentialId: wrap.credential_id});
  assert.ok(await signsAsRoot(again, envelope.root_public_key));
  await assert.rejects(unlockIdentity(envelope, {prfOutput: new Uint8Array(32), credentialId: wrap.credential_id}), {code: 'rejected'});
  await assert.rejects(unlockIdentity(envelope, {prfOutput, credentialId: new Uint8Array(16)}), {code: 'no-wrap'});
});

test('the identity created or unlocked last is the default for the calls that take one', async () => {
  const credentials = fakeAuthenticator();
  const {envelope, identity} = await createIdentity({...OPTIONS, credentials});
  assert.equal(currentIdentity(), identity);
  // rpId, userName and userId come from the envelope and the account it was created with.
  const more = await addPasskey(envelope);
  assert.equal(more.wraps.length, 3);
  assert.equal(more.wraps[2].prf_salt, envelope.wraps[0].prf_salt, 'one PRF input for every passkey');
  assert.equal(credentials.log.at(-1).rpId, 'tilo.so');
  const seen = [];
  const runtime = {issueDeviceCert: async (...args) => { seen.push(args); return '{}'; }};
  await certifyDevice({devicePublicKey: 'ab'.repeat(32), runtime, encoding: 'json'});
  assert.equal(seen[0][0], identity);
  identity.lock();
  assert.equal(currentIdentity(), undefined);
  await assert.rejects(addPasskey(envelope), {code: 'locked'});
  const unlocked = await unlockIdentity(more, {passkey: true, credentials});
  assert.equal(currentIdentity(), unlocked);
  await assert.rejects(addPasskey(more), TypeError, 'an unlocked identity has no account to name a new passkey for');
});

test('a node certificate is installed with its install token', async () => {
  const calls = [];
  const fetch = async (url, init) => {
    calls.push({url, init});
    const ok = init.headers['x-tilo-install-token'] === 'token';
    return {status: ok ? 200 : 403, text: async () => (ok ? '{"device_public_key":"aa","user_public_key":"bb"}' : 'wrong install token')};
  };
  const answer = await installNodeCertificate({origin: 'https://node.example', certificate: '{"cert":1}', installToken: 'token', fetch});
  assert.deepEqual(answer, {device_public_key: 'aa', user_public_key: 'bb'});
  assert.equal(calls[0].url, 'https://node.example/peer/v1/install');
  assert.equal(calls[0].init.body, '{"cert":1}');
  await installNodeCertificate({origin: 'https://node.example', certificate: '+/8B', installToken: 'token', fetch});
  assert.deepEqual([...calls[1].init.body], [0xfb, 0xff, 1]);
  assert.equal(calls[1].init.headers['content-type'], 'application/octet-stream');
  await assert.rejects(installNodeCertificate({origin: 'https://node.example', certificate: {}, installToken: 'other', fetch}), {code: 'install-token-refused'});
});

/** A node waiting for its certificate, with a session beside it. */
async function waitingNode({vouchFor} = {}) {
  const node = await crypto.subtle.generateKey({name: 'Ed25519'}, true, ['sign', 'verify']);
  const nodeKey = hex(new Uint8Array(await crypto.subtle.exportKey('raw', node.publicKey)));
  const session = 'cd'.repeat(32);
  const message = new Uint8Array([...new TextEncoder().encode('tilo node session v1\0'), ...unhex(nodeKey), ...unhex(vouchFor ?? session)]);
  const signature = hex(new Uint8Array(await crypto.subtle.sign({name: 'Ed25519'}, node.privateKey, message)));
  const calls = [];
  const fetch = async (url, init) => {
    calls.push({url, init});
    if (init.headers['x-tilo-install-token'] !== 'token') return {status: 403, text: async () => 'wrong install token'};
    const info = {device_public_key: nodeKey, session: {device_public_key: session, signature}};
    return {status: 200, text: async () => JSON.stringify(init.method === 'GET' ? info : {device_public_key: nodeKey, user_public_key: 'bb'})};
  };
  return {nodeKey, session, calls, fetch};
}

test('a node names the session beside it, vouched for by the key the page got out of band', async () => {
  const {nodeKey, session, calls, fetch} = await waitingNode();
  const origin = 'https://node.example';
  assert.deepEqual(await nodeInstallInfo({origin, devicePublicKey: nodeKey.toUpperCase(), installToken: 'token', fetch}), {sessionPublicKey: session});
  assert.equal(calls[0].init.method, 'GET');
  await assert.rejects(nodeInstallInfo({origin, devicePublicKey: 'ab'.repeat(32), installToken: 'token', fetch}), {code: 'install-refused'});
  await assert.rejects(nodeInstallInfo({origin, devicePublicKey: nodeKey, installToken: 'other', fetch}), {code: 'install-token-refused'});
  const forged = await waitingNode({vouchFor: 'ef'.repeat(32)});
  await assert.rejects(nodeInstallInfo({origin, devicePublicKey: forged.nodeKey, installToken: 'token', fetch: forged.fetch}), {code: 'install-refused'});
});

test('installing a node certifies it and its session and hands both over', async () => {
  const credentials = fakeAuthenticator();
  const {identity} = await createIdentity({...OPTIONS, credentials});
  const {nodeKey, session, calls, fetch} = await waitingNode();
  const issued = [];
  const runtime = {
    issueDeviceCert: async (_, key, label, role) => { issued.push({key, label, role}); return JSON.stringify({key, role}); },
    // Stands in for the canonical encoding.
    deviceCertCanonical: json => new TextEncoder().encode(json),
  };
  const answer = await installNode({origin: 'https://node.example', devicePublicKey: nodeKey, installToken: 'token', identity, label: 'Acme node', runtime, fetch});
  assert.deepEqual(answer, {device_public_key: nodeKey, user_public_key: 'bb'});
  assert.deepEqual(issued, [{key: nodeKey, label: 'Acme node', role: 'node'}, {key: session, label: 'Apps on Acme node', role: 'session'}]);
  const posted = calls.at(-1);
  assert.equal(posted.init.method, 'POST');
  assert.equal(posted.init.headers['content-type'], 'application/json');
  const sent = JSON.parse(posted.init.body);
  assert.deepEqual(JSON.parse(atob(sent.certificate)), {key: nodeKey, role: 'node'});
  assert.deepEqual(JSON.parse(atob(sent.session_certificate)), {key: session, role: 'session'});
  await assert.rejects(installNodeCertificate({origin: 'https://node.example', certificate: '+/8B', sessionCertificate: '{}', installToken: 'token', fetch}), TypeError);
});

function memoryStorage() {
  const map = new Map();
  // Structured clone keeps CryptoKeys (and their extractability), as IndexedDB does.
  return {map, get: async key => (map.has(key) ? structuredClone(map.get(key)) : undefined), put: async (key, value) => { map.set(key, structuredClone(value)); }, delete: async key => { map.delete(key); }};
}

test('a browser creates its person once, holds the root only wrapped, and asks to keep its storage', async () => {
  const storage = memoryStorage();
  let persistAsked = 0;
  const persist = {persist: async () => { persistAsked += 1; return true; }};
  assert.equal(await browserIdentity({storage, create: false}), null);
  const first = await browserIdentity({storage, persist});
  assert.equal(persistAsked, 1);
  assert.match(first.userPublicKey, /^[0-9a-f]{64}$/);
  assert.equal(first.kept, false);
  assert.deepEqual(first.envelope.wraps, []);
  const kept = storage.map.get('identity');
  assert.equal(kept.held.key.extractable, false);
  assert.ok(await signsAsRoot(first, first.userPublicKey));
  const again = await browserIdentity({storage, persist});
  assert.equal(again.userPublicKey, first.userPublicKey);
  assert.equal(persistAsked, 1);
  assert.equal(currentIdentity(), again);
});

test('keeping a person with a passkey lets a new browser sign in as them', async () => {
  const credentials = fakeAuthenticator();
  const storage = memoryStorage();
  const person = await browserIdentity({storage, persist: null, credentials});
  const envelope = await person.keepWithPasskey({rpId: 'example.com', userName: 'Brave Otter'});
  assert.equal(person.kept, true);
  assert.deepEqual(envelope.wraps.map(wrap => wrap.kind), ['passkey']);
  assert.deepEqual(storage.map.get('identity').envelope, envelope);
  assert.equal(new TextDecoder().decode([...credentials.passkeys.values()][0].userHandle), person.userPublicKey);

  const fresh = memoryStorage();
  const asked = [];
  const signedIn = await signInWithPasskey({rpId: 'example.com', storage: fresh, credentials,
    fetchEnvelope: async root => { asked.push(root); return envelope; }});
  assert.deepEqual(asked, [person.userPublicKey]);
  assert.equal(signedIn.userPublicKey, person.userPublicKey);
  assert.equal(signedIn.kept, true);
  assert.ok(await signsAsRoot(signedIn, person.userPublicKey));
  const reopened = await browserIdentity({storage: fresh});
  assert.equal(reopened.userPublicKey, person.userPublicKey);

  await assert.rejects(signInWithPasskey({rpId: 'example.com', storage: memoryStorage(), credentials, fetchEnvelope: async () => null}), {code: 'no-envelope'});
  const other = await browserIdentity({storage: memoryStorage(), persist: null});
  await assert.rejects(signInWithPasskey({rpId: 'example.com', storage: memoryStorage(), credentials,
    fetchEnvelope: async () => ({...envelope, root_public_key: other.userPublicKey})}), {code: 'wrong-identity'});
});

test('a recovery key opens a browser person, and a new one replaces the old', async () => {
  const person = await browserIdentity({storage: memoryStorage(), persist: null});
  const first = await person.createRecoveryKey();
  assert.equal(person.kept, true);
  const second = await person.createRecoveryKey();
  assert.deepEqual(person.envelope.wraps.map(wrap => wrap.kind), ['recovery']);
  await assert.rejects(unlockIdentity(person.envelope, {recoveryKey: first}), {code: 'rejected'});
  const opened = await unlockIdentity(person.envelope, {recoveryKey: second});
  assert.equal(opened.userPublicKey, person.userPublicKey);
});

test('a host is handed the envelope signed by its root, and a new browser fetches it back', async () => {
  const person = await browserIdentity({storage: memoryStorage(), persist: null});
  await assert.rejects(publishEnvelope({origin: 'https://host.example', identity: person, fetch: async () => ({ok: true})}), {code: 'not-kept'});
  await person.createRecoveryKey();
  const kept = new Map();
  const fetch = async (url, init = {}) => {
    const path = new URL(url).pathname;
    if (path === '/peer/v1/envelope') {
      const body = JSON.parse(init.body);
      const envelope = JSON.parse(body.envelope);
      const label = new TextEncoder().encode('tilo root envelope v1\0');
      const json = new TextEncoder().encode(body.envelope);
      const statement = new Uint8Array(label.length + 8 + json.length);
      statement.set(label);
      new DataView(statement.buffer).setBigUint64(label.length, BigInt(body.issued_at_ms), true);
      statement.set(json, label.length + 8);
      const key = await crypto.subtle.importKey('raw', unhex(envelope.root_public_key), {name: 'Ed25519'}, false, ['verify']);
      const valid = await crypto.subtle.verify({name: 'Ed25519'}, key, unhex(body.signature), statement);
      if (valid) kept.set(envelope.root_public_key, envelope);
      return {ok: valid, status: valid ? 204 : 403, text: async () => ''};
    }
    assert.equal(path, '/peer/v1/envelope/fetch');
    return {ok: true, json: async () => ({envelope: kept.get(init.body) ?? null})};
  };
  await publishEnvelope({origin: 'https://host.example', identity: person, fetch});
  assert.deepEqual(kept.get(person.userPublicKey), person.envelope);
  const either = async (url, init) => {
    if (url.startsWith('https://down.example')) throw new Error('unreachable');
    return fetch(url, init);
  };
  assert.deepEqual(await fetchEnvelope(person.userPublicKey, ['https://down.example', 'https://host.example'], {fetch: either}), person.envelope);
  assert.equal(await fetchEnvelope('ab'.repeat(32), ['https://host.example'], {fetch}), null);
});

test('a profile names the person, signed by their root, and any of their devices fetches it back', async () => {
  const person = await browserIdentity({storage: memoryStorage(), persist: null});
  const kept = new Map();
  const fetch = async (url, init = {}) => {
    const path = new URL(url).pathname;
    if (path === '/peer/v1/profile') {
      const body = JSON.parse(init.body);
      const profile = JSON.parse(body.profile);
      const label = new TextEncoder().encode('tilo person profile v1\0');
      const json = new TextEncoder().encode(body.profile);
      const statement = new Uint8Array(label.length + 8 + json.length);
      statement.set(label);
      new DataView(statement.buffer).setBigUint64(label.length, BigInt(body.issued_at_ms), true);
      statement.set(json, label.length + 8);
      const key = await crypto.subtle.importKey('raw', unhex(profile.user_public_key), {name: 'Ed25519'}, false, ['verify']);
      const valid = await crypto.subtle.verify({name: 'Ed25519'}, key, unhex(body.signature), statement);
      if (valid) kept.set(profile.user_public_key, profile);
      return {ok: valid, status: valid ? 204 : 403, text: async () => ''};
    }
    assert.equal(path, '/peer/v1/profile/fetch');
    return {ok: true, json: async () => ({profile: kept.get(init.body) ?? null})};
  };
  await assert.rejects(publishProfile({origin: 'https://host.example', identity: person, displayName: '  ', fetch}), {code: 'malformed'});
  await publishProfile({origin: 'https://host.example', identity: person, displayName: ' Ada Lovelace ', fetch});
  assert.deepEqual(kept.get(person.userPublicKey), {version: 1, user_public_key: person.userPublicKey, display_name: 'Ada Lovelace'});
  assert.equal((await fetchProfile(person.userPublicKey, ['https://host.example'], {fetch})).display_name, 'Ada Lovelace');
  assert.equal(await fetchProfile('ab'.repeat(32), ['https://host.example'], {fetch}), null);
  const refusing = async () => ({ok: false, status: 403, text: async () => 'this host keeps this for its members only'});
  await assert.rejects(publishProfile({origin: 'https://host.example', identity: person, displayName: 'Ada', fetch: refusing}), {code: 'publish-refused'});
});

test('a name is fitted as hosts keep it: 128 bytes of UTF-8, cut between characters', () => {
  assert.equal(MAX_DISPLAY_NAME_BYTES, 128);
  const bytes = text => new TextEncoder().encode(text).length;
  assert.equal(fitDisplayName('  Ada\u0007 Lovelace\n '), 'Ada Lovelace');
  assert.equal(fitDisplayName('a'.repeat(200)), 'a'.repeat(128));
  // 64 characters of three bytes each: 42 fit, none cut in half.
  const wide = fitDisplayName('名'.repeat(64));
  assert.equal(wide, '名'.repeat(42));
  assert.ok(bytes(wide) <= 128);
  const emoji = fitDisplayName('😀'.repeat(40));
  assert.equal(emoji, '😀'.repeat(32));
  assert.equal(fitDisplayName(null), '');
});

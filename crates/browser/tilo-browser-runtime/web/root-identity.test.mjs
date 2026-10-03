import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {
  addPasskey, base64url, certifyDevice, createIdentity, formatRecoveryKey, IdentityError, parseRecoveryKey, unbase64url, unhex,
  unlockIdentity,
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
  const runtime = {issueDeviceCert: async (...args) => { seen.push(args); return '{"cert":1}'; }};
  const node = 'AB'.repeat(32);
  assert.equal(await certifyDevice({identity, runtime, devicePublicKey: node}), '{"cert":1}');
  assert.deepEqual(seen[0].slice(1), ['ab'.repeat(32), 'Tilo node', 'node']);
  assert.equal(seen[0][0], identity);
  await certifyDevice({identity, runtime, devicePublicKey: new Uint8Array(32).fill(1), label: 'Phone', role: 'browser'});
  assert.deepEqual(seen[1].slice(1), ['01'.repeat(32), 'Phone', 'browser']);
  await assert.rejects(certifyDevice({identity, runtime, devicePublicKey: 'abcd'}), {code: 'malformed'});
  identity.lock();
  await assert.rejects(certifyDevice({identity, runtime, devicePublicKey: node}), {code: 'locked'});
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {installSecret, readInstallSecret, setUpFailure, setUpMachine} from './machine-setup.mjs';
import {hex, unhex} from './root-identity.mjs';

const TOKEN = 'provisioned-install-token-0123';
const person = {userPublicKey: 'aa'.repeat(32), sign: async () => new Uint8Array(64), locked: false};
// Certificates as a stand-in runtime issues them: the canonical encoding is the JSON.
const runtime = {
  issueDeviceCert: async (_, key, label, role) => JSON.stringify({key, role}),
  deviceCertCanonical: json => new TextEncoder().encode(json),
};

/** A node waiting for its owner, as `tilo-relay`'s install route answers;
 * once installed it serves as a host, which has no install route. */
async function waitingNode({owner = person.userPublicKey} = {}) {
  const keys = await crypto.subtle.generateKey({name: 'Ed25519'}, true, ['sign', 'verify']);
  const nodeKey = hex(new Uint8Array(await crypto.subtle.exportKey('raw', keys.publicKey)));
  const session = 'cd'.repeat(32);
  const vouch = new Uint8Array([...new TextEncoder().encode('tilo node session v1\0'), ...unhex(nodeKey), ...unhex(session)]);
  const signature = hex(new Uint8Array(await crypto.subtle.sign({name: 'Ed25519'}, keys.privateKey, vouch)));
  let installed = false;
  const fetch = async (_url, init) => {
    const answer = (status, body) => ({status, text: async () => (typeof body === 'string' ? body : JSON.stringify(body))});
    if (installed) return answer(405, 'POST only');
    if (init.headers['x-tilo-install-token'] !== TOKEN) return answer(403, 'wrong install token');
    if (init.method === 'GET') return answer(200, {device_public_key: nodeKey, session: {device_public_key: session, signature}});
    installed = true;
    return answer(200, {device_public_key: nodeKey, user_public_key: owner});
  };
  return {details: {origin: 'https://node.example:8443', devicePublicKey: nodeKey, installToken: TOKEN}, fetch};
}

test('a setup link carries the machine through the fragment, and nothing malformed passes', () => {
  const details = {origin: 'https://Node.Example:8443/', devicePublicKey: 'AB'.repeat(32), installToken: ` ${TOKEN} `};
  const secret = installSecret(details);
  assert.match(secret, /^[A-Za-z0-9_-]+$/, 'a link secret is base64url');
  assert.deepEqual(readInstallSecret(secret), {origin: 'https://node.example:8443', devicePublicKey: 'ab'.repeat(32), installToken: TOKEN});
  for (const bad of ['%%%', installSecret(details).slice(0, 12)]) assert.throws(() => readInstallSecret(bad), {code: 'malformed'});
  assert.throws(() => installSecret({...details, origin: 'http://node.example'}), {code: 'malformed'});
  assert.doesNotThrow(() => installSecret({...details, origin: 'http://localhost:8443'}));
  assert.throws(() => installSecret({...details, devicePublicKey: 'ab'}), {code: 'malformed'});
  assert.throws(() => installSecret({...details, installToken: 'short'}), {code: 'malformed'});
});

test('the person\'s root makes a waiting machine theirs once, and says why it could not', async () => {
  const node = await waitingNode();
  const phases = [];
  const wrong = await setUpMachine({details: {...node.details, installToken: `${TOKEN}x`}, person: async () => person, runtime, fetch: node.fetch})
    .catch(error => error);
  assert.equal(wrong.code, 'install-token-refused');
  assert.match(setUpFailure(wrong, node.details.origin), /node\.example:8443 refused the install token/);

  const machine = await setUpMachine({details: node.details, person: async () => person, runtime, fetch: node.fetch,
    onProgress: ({phase}) => phases.push(phase)});
  assert.deepEqual(machine, {origin: node.details.origin, devicePublicKey: node.details.devicePublicKey, userPublicKey: person.userPublicKey});
  assert.deepEqual(phases, ['checking', 'certifying', 'installing']);

  const again = await setUpMachine({details: node.details, person: async () => person, runtime, fetch: node.fetch}).catch(error => error);
  assert.equal(again.code, 'not-waiting');
  assert.match(setUpFailure(again, node.details.origin), /already set up/);

  const locked = await setUpMachine({details: node.details, person: async () => ({...person, locked: true}), runtime, fetch: node.fetch})
    .catch(error => error);
  assert.equal(locked.code, 'locked');

  const stolen = await waitingNode({owner: 'bb'.repeat(32)});
  await assert.rejects(setUpMachine({details: stolen.details, person: async () => person, runtime, fetch: stolen.fetch}), {code: 'other-owner'});
});

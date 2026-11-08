// What the page keeps around the runtime's envelope crypto: the names it
// formats, the hex it reads, and the HTTP that carries a person's envelope,
// profile and device certificates to their hosts. The crypto itself is the
// runtime's (`Root`), proven in a browser by the peer's page driver.
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  fetchEnvelope, fetchProfile, fitDisplayName, hex, IdentityError, installNodeCertificate, MAX_DISPLAY_NAME_BYTES,
  nodeInstallInfo, publishProfile, unhex,
} from './identity.mjs';

/** What resolved a promise: its value, or the error it refused with. */
const settled = promise => promise.then(value => ({value}), error => ({error}));

/** An unlocked identity whose root signs (a host would check the signature;
 * these tests look at what travels). */
function signingIdentity(publicKey = 'ab'.repeat(32)) {
  return {
    userPublicKey: publicKey,
    locked: false,
    signed: [],
    async signRecord(record, json, issuedAtMs) {
      this.signed.push([record, json, issuedAtMs]);
      return new Uint8Array(64).fill(7);
    },
    async sign() { return new Uint8Array(64).fill(7); },
  };
}

/** `fetch`, answering each URL from `routes` (`{status, body}`), and
 * recording the requests. */
function fakeFetch(routes) {
  const requests = [];
  const fetcher = async (url, init = {}) => {
    requests.push({url, init});
    const route = typeof routes === 'function' ? routes(url, init) : routes[url];
    if (!route) throw new TypeError('fetch failed');
    return {
      ok: (route.status ?? 200) < 400,
      status: route.status ?? 200,
      text: async () => JSON.stringify(route.body),
      json: async () => route.body,
    };
  };
  fetcher.requests = requests;
  return fetcher;
}

test('a display name is fitted to what a profile keeps: trimmed, no control characters, 128 bytes of UTF-8', () => {
  assert.equal(fitDisplayName('  Ada Lovelace \n'), 'Ada Lovelace');
  assert.equal(fitDisplayName('A\u0000d\u0007a'), 'Ada');
  assert.equal(fitDisplayName(undefined), '');
  assert.equal(fitDisplayName('É'.repeat(100)).length, 64, 'two bytes each');
  assert.equal(new TextEncoder().encode(fitDisplayName('É'.repeat(100))).length, MAX_DISPLAY_NAME_BYTES);
  assert.deepEqual([...fitDisplayName('a'.repeat(200))].length, MAX_DISPLAY_NAME_BYTES, 'never half a character');
});

test('hex reads and writes bytes, and refuses anything else', () => {
  assert.equal(hex(Uint8Array.from([0, 15, 255])), '000fff');
  assert.deepEqual([...unhex('00ff10')], [0, 255, 16]);
  assert.deepEqual([...unhex('')], []);
  for (const text of ['0', 'zz', '0g']) assert.throws(() => unhex(text), IdentityError);
});

test('an envelope is looked for at every host in turn, and "keeps none" is told apart from "could not ask"', async () => {
  const root = 'ab'.repeat(32);
  const envelope = {version: 1, root_public_key: root, wraps: []};
  const origins = ['https://a.example', 'https://b.example'];
  let fetcher = fakeFetch({
    'https://b.example/peer/v1/envelope/fetch': {body: {envelope}},
  });
  assert.deepEqual(await fetchEnvelope(root, origins, {fetch: fetcher}), envelope);
  assert.deepEqual(fetcher.requests.map(request => request.url),
    ['https://a.example/peer/v1/envelope/fetch', 'https://b.example/peer/v1/envelope/fetch']);
  assert.equal(fetcher.requests[0].init.method, 'POST');
  assert.equal(fetcher.requests[0].init.body, root);

  // A host that answers null keeps none.
  fetcher = fakeFetch({'https://a.example/peer/v1/envelope/fetch': {body: {envelope: null}}});
  assert.equal(await fetchEnvelope(root, ['https://a.example'], {fetch: fetcher}), null);
  // Nobody answered: the caller can ask again later.
  assert.equal(await fetchEnvelope(root, origins, {fetch: fakeFetch({})}), undefined);
  // An answer naming another root is not this person's envelope.
  fetcher = fakeFetch({'https://a.example/peer/v1/envelope/fetch': {body: {envelope: {...envelope, root_public_key: 'cd'.repeat(32)}}}});
  assert.equal(await fetchEnvelope(root, ['https://a.example'], {fetch: fetcher}), null);
});

test('a profile is looked for the same way, and only one naming the person counts', async () => {
  const root = 'ab'.repeat(32);
  const profile = {version: 1, user_public_key: root, display_name: 'Ada'};
  const fetcher = fakeFetch({
    'https://a.example/peer/v1/profile/fetch': {body: {profile: {...profile, user_public_key: 'cd'.repeat(32)}}},
    'https://b.example/peer/v1/profile/fetch': {body: {profile}},
  });
  assert.deepEqual(await fetchProfile(root, ['https://a.example', 'https://b.example'], {fetch: fetcher}), profile);
  assert.equal(await fetchProfile(root, ['https://a.example'], {fetch: fetcher}), null);
});

test('a profile is signed by the root and carries the name, picture and links the person chose', async () => {
  const identity = signingIdentity();
  const fetcher = fakeFetch({'https://a.example/peer/v1/profile': {body: {}}});
  await publishProfile({origin: 'https://a.example', identity, displayName: '  Ada  ',
    photo: 'data:image/png;base64,AA', photoFrom: 'Discord', status: 'shipping', sharing: 'teams',
    linked: [{service: 'Discord', handle: 'ada', extra: 'dropped'}], now: 1_700, fetch: fetcher});
  const [{init}] = fetcher.requests;
  const body = JSON.parse(init.body);
  assert.deepEqual(JSON.parse(body.profile), {
    version: 1, user_public_key: identity.userPublicKey, display_name: 'Ada', photo: 'data:image/png;base64,AA',
    status: 'shipping', sharing: 'teams', photo_from: 'Discord', linked: [{service: 'Discord', handle: 'ada'}],
  });
  assert.equal(body.issued_at_ms, 1_700);
  assert.equal(body.signature, '07'.repeat(64));
  assert.deepEqual(identity.signed, [['profile', body.profile, 1_700]]);

  // Without a picture, its source has nothing to say.
  const plain = signingIdentity();
  const none = fakeFetch({'https://a.example/peer/v1/profile': {body: {}}});
  await publishProfile({origin: 'https://a.example', identity: plain, displayName: 'Ada', photoFrom: 'Discord', fetch: none});
  assert.deepEqual(Object.keys(JSON.parse(JSON.parse(none.requests[0].init.body).profile)),
    ['version', 'user_public_key', 'display_name']);
});

test('a profile needs an unlocked root and a name, and a refusal says so', async () => {
  const identity = signingIdentity();
  await assert.rejects(publishProfile({origin: 'https://a.example', identity: {...identity, locked: true}, displayName: 'Ada', fetch: fakeFetch({})}),
    {code: 'locked'});
  await assert.rejects(publishProfile({origin: 'https://a.example', identity, displayName: '  ', fetch: fakeFetch({})}), {code: 'malformed'});
  const refused = await settled(publishProfile({origin: 'https://a.example', identity, displayName: 'Ada',
    fetch: fakeFetch({'https://a.example/peer/v1/profile': {status: 403, body: {error: 'not a member'}}})}));
  assert.equal(refused.error.code, 'publish-refused');
  assert.match(refused.error.message, /not a member/);
});

test('a certificate is installed as JSON or as the canonical encoding, and the node says why it refused', async () => {
  const canonical = 'gAAB';
  const route = {'https://node.example/peer/v1/install': {body: {device_public_key: 'ab', user_public_key: 'cd'}}};
  let fetcher = fakeFetch(route);
  assert.deepEqual(await installNodeCertificate({origin: 'https://node.example', certificate: canonical, installToken: 't', fetch: fetcher}),
    {device_public_key: 'ab', user_public_key: 'cd'});
  assert.equal(fetcher.requests[0].init.headers['content-type'], 'application/octet-stream');
  assert.equal(fetcher.requests[0].init.headers['x-tilo-install-token'], 't');
  assert.deepEqual([...fetcher.requests[0].init.body], [128, 0, 1]);

  fetcher = fakeFetch(route);
  await installNodeCertificate({origin: 'https://node.example', certificate: '{"cert":{}}', installToken: 't', fetch: fetcher});
  assert.equal(fetcher.requests[0].init.headers['content-type'], 'application/json');
  assert.equal(fetcher.requests[0].init.body, '{"cert":{}}');

  // A node and its session together go as the canonical encodings.
  fetcher = fakeFetch(route);
  await installNodeCertificate({origin: 'https://node.example', certificate: canonical, sessionCertificate: canonical, installToken: 't', fetch: fetcher});
  assert.deepEqual(JSON.parse(fetcher.requests[0].init.body), {certificate: canonical, session_certificate: canonical});
  await assert.rejects(installNodeCertificate({origin: 'https://node.example', certificate: 'not base64!', sessionCertificate: canonical, installToken: 't', fetch: fetcher}), TypeError);

  assert.equal((await settled(installNodeCertificate({origin: 'https://node.example', certificate: canonical, installToken: 't',
    fetch: fakeFetch({'https://node.example/peer/v1/install': {status: 403, body: {error: 'no'}}})}))).error.code, 'install-token-refused');
  assert.equal((await settled(installNodeCertificate({origin: 'https://node.example', certificate: canonical, installToken: 't',
    fetch: fakeFetch({'https://node.example/peer/v1/install': {status: 404, body: {error: 'serving'}}})}))).error.code, 'not-waiting');
  await assert.rejects(installNodeCertificate({origin: 'https://node.example', installToken: 't'}), TypeError);
});

test('a node is asked what it runs, and only a session its own key vouches for is taken', async () => {
  const node = 'ab'.repeat(32);
  const session = 'cd'.repeat(32);
  const answer = body => fakeFetch({'https://node.example/peer/v1/install': {body}});
  assert.deepEqual(await nodeInstallInfo({origin: 'https://node.example', devicePublicKey: node, installToken: 't',
    fetch: answer({device_public_key: node})}), {sessionPublicKey: null});
  // Another node at the origin: the key the page got out of band is not it.
  assert.equal((await settled(nodeInstallInfo({origin: 'https://node.example', devicePublicKey: node, installToken: 't',
    fetch: answer({device_public_key: 'ee'.repeat(32)})}))).error.code, 'install-refused');
  // The request names the token the node was provisioned with.
  const asked = answer({device_public_key: node});
  await nodeInstallInfo({origin: 'https://node.example', devicePublicKey: node, installToken: 't', fetch: asked});
  assert.equal(asked.requests[0].init.method, 'GET');
  assert.equal(asked.requests[0].init.headers['x-tilo-install-token'], 't');
});

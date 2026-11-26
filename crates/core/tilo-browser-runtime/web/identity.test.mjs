// What the page keeps around the runtime's envelope crypto: the names it
// formats, the hex it reads, and the HTTP that carries a person's envelope,
// profile and device certificates to their hosts. The crypto itself is the
// runtime's (`Root`), proven in a browser by the peer's page driver.
import test from 'node:test';
import assert from 'node:assert/strict';
import {hex as sharedHex} from './hex.mjs';
import {
  answerSignIn, fetchEnvelope, fetchProfile, fitDisplayName, hex, IdentityError, installNodeCertificate,
  keptIdentity, MAX_DISPLAY_NAME_BYTES, nodeInstallInfo, publishProfile, REFUSED_WITHIN_MS, signInsWaiting,
  signInWithPasskey, unhex,
} from './identity.mjs';
import {personRecord, PERSON_KEY} from './device-key.mjs';

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
  assert.equal(hex, sharedHex);
  assert.equal(hex(Uint8Array.from([0, 15, 255])), '000fff');
  assert.deepEqual([...unhex('00ff10')], [0, 255, 16]);
  assert.deepEqual([...unhex('00FF10')], [0, 255, 16]);
  assert.deepEqual([...unhex('')], []);
  for (const text of ['0', 'abc', 'zz', '1g', '0g']) {
    assert.throws(() => unhex(text), error => error instanceof IdentityError && error.code === 'malformed');
  }
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
  // A server only an invitation named has no web origin: it is never asked over HTTP.
  const invited = 'https://0221e6218cbf08506ee73f14758b8f79.iroh.invalid';
  const asked = fakeFetch({'https://b.example/peer/v1/profile/fetch': {body: {profile}}});
  assert.deepEqual(await fetchProfile(root, [invited, 'https://b.example'], {fetch: asked}), profile);
  assert.deepEqual(asked.requests.map(request => request.url), ['https://b.example/peer/v1/profile/fetch']);
  assert.equal(await fetchProfile(root, [invited], {fetch: asked}), undefined, 'nobody answered');
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

test('a page lists the tabs waiting to sign in as its person, and signs in the one it is told to', async () => {
  const root = {userPublicKey: 'ab'.repeat(32)};
  const cert = '{"device_public_key":[221],"issued_at_ns":1792023733547123457}';
  const asked = [];
  const host = {
    runtime: {async signInCertificate(signer, device, label) { asked.push(['sign', signer, device, label]); return cert; }},
    async act(action, body, signer) {
      assert.equal(signer, root);
      asked.push([action, body]);
      return action === 'pending'
        ? {waiting: [{secret: 's1', label: 'Firefox on Linux', device: 'dd'.repeat(32), capability: 'a browser of yours'}]}
        : null;
    },
  };
  const waiting = await signInsWaiting(host, root);
  assert.deepEqual(waiting, [{digest: 's1', label: 'Firefox on Linux', device: 'dd'.repeat(32), capability: 'a browser of yours', signIn: true}]);
  assert.equal(await answerSignIn(host, root, waiting[0], false), null, 'turned away, nothing is signed');
  assert.deepEqual(asked, [['pending', {user: root.userPublicKey}]]);
  assert.equal(await answerSignIn(host, root, waiting[0], true), cert);
  assert.deepEqual(asked.slice(1, 2), [['sign', root, 'dd'.repeat(32), 'Firefox on Linux']]);
  // The certificate goes byte for byte: its nanoseconds are more than a JS number holds.
  assert.equal(asked[2][0], 'approve');
  assert.ok(asked[2][1].includes(`"cert":${cert}`), asked[2][1]);
  assert.deepEqual(JSON.parse(asked[2][1]).secret, 's1');
});

/** A browser's IndexedDB store, in memory. */
function memoryStorage(entries = {}) {
  const kept = new Map(Object.entries(entries));
  return {
    get: async key => kept.get(key),
    put: async (key, value) => { kept.set(key, value); },
    delete: async key => { kept.delete(key); },
  };
}

/** What the one store keeps for the person whose root is `rootHex`: the
 * identity store's record, as the page writes it. */
const storedPerson = rootHex => ({[PERSON_KEY]: JSON.stringify(personRecord({seed: new Uint8Array(32).fill(1), publicKey: unhex(rootHex)}))});

test('a place that refuses a passkey at once is told apart from a person who closes the sheet', async () => {
  const refusal = () => Object.assign(new Error('The request is not allowed'), {name: 'NotAllowedError'});
  const stored = memoryStorage(storedPerson('ef'.repeat(32)));
  let clock = 1000;
  // `took` milliseconds pass before the browser answers with `error`.
  const signIn = (took, error = refusal()) => signInWithPasskey({rpId: 'tilo.test', fetchEnvelope: async () => null, storage: stored,
    credentials: {get: async () => { clock += took; throw error; }}, persist: null, runtime: {}, now: () => clock});
  // A web view inside an app that may not use this site's passkeys answers in a few
  // milliseconds, before anything is shown: the place cannot, and the page says what can.
  const refused = await signIn(3).then(() => null, error => error);
  assert.equal(refused?.code, 'passkeys-refused');
  assert.equal(refused.cause?.name, 'NotAllowedError', 'what the browser said stays readable');
  await assert.rejects(signIn(REFUSED_WITHIN_MS - 1), {code: 'passkeys-refused'});
  // A person needs a moment to read a sheet and close it: that stays their own answer.
  await assert.rejects(signIn(REFUSED_WITHIN_MS), {name: 'NotAllowedError'});
  await assert.rejects(signIn(4000), {name: 'NotAllowedError'});
  // Anything else the browser says at once is what it is.
  await assert.rejects(signIn(1, Object.assign(new Error('insecure'), {name: 'SecurityError'})), {name: 'SecurityError'});
  await assert.rejects(signIn(1, Object.assign(new Error('gone'), {name: 'AbortError'})), {name: 'AbortError'});
  // A page that is not in front is refused at once as well, and may simply ask again.
  globalThis.document = {hasFocus: () => false};
  try {
    await assert.rejects(signIn(1), {name: 'NotAllowedError'});
  } finally {
    delete globalThis.document;
  }
  assert.deepEqual(await keptIdentity({storage: stored}), {rootPublicKey: 'ef'.repeat(32)},
    'a refusal leaves whoever this browser was');
});

test('a person signed in with a passkey is the person this browser keeps, replacing whoever it was', async () => {
  const root = 'cd'.repeat(32);
  // The browser was someone else first: a visitor it made at the entry.
  const storage = memoryStorage(storedPerson('ef'.repeat(32)));
  const envelope = {root, wraps: [{kind: 'passkey'}]};
  const runtime = {
    rootEnvelopePublicKey: () => root,
    rootOpenPasskey: async () => ({seed: new Uint8Array(32).fill(5), publicKey: unhex(root)}),
    rootHeld: async (seed, publicKey) => ({userPublicKey: publicKey, seed, lock() {}}),
    rootEnvelopeKept: () => true,
  };
  const credentials = {get: async () => ({
    rawId: new Uint8Array([1, 2, 3]).buffer,
    response: {userHandle: new TextEncoder().encode(root).buffer},
    getClientExtensionResults: () => ({prf: {results: {first: new Uint8Array(32).fill(9).buffer}}}),
  })};
  const signedIn = await signInWithPasskey({rpId: 'tilo.test', fetchEnvelope: async () => envelope, storage, credentials,
    persist: null, runtime});
  assert.equal(signedIn.userPublicKey, root);
  assert.deepEqual(await keptIdentity({storage}), {rootPublicKey: root},
    'the next load opens the person who signed in, not the visitor the browser was');
});

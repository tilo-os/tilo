import test from 'node:test';
import assert from 'node:assert/strict';
import {createIdentityActions, KEEP, NAME, PAIR, RECOVERY, SIGN_IN, syncProfile} from './identity-actions.mjs';
import {browserIdentity, IdentityError, unlockIdentity} from './root-identity.mjs';
import {fakeAuthenticator} from './testing/fake-authenticator.mjs';

function memoryStorage() {
  const map = new Map();
  return {get: async key => (map.has(key) ? structuredClone(map.get(key)) : undefined), put: async (key, value) => { map.set(key, structuredClone(value)); }, delete: async key => { map.delete(key); }};
}

function fakeMount() {
  return {
    actions: [], offers: [],
    setHostActions(actions) { this.actions = actions.map(action => action.id); },
    offerHostAction(offer) { this.offers.push(offer.action.id); },
  };
}

test('a new person is offered to keep their identity when storage may go, and once on first post', async () => {
  const identity = await browserIdentity({storage: memoryStorage(), persist: null});
  const mount = fakeMount();
  const actions = createIdentityActions({identity, persisted: async () => false});
  await actions.attach(mount);
  assert.deepEqual(mount.actions, [KEEP.id, RECOVERY.id, SIGN_IN.id]);
  assert.deepEqual(mount.offers, [KEEP.id]);
  actions.authored();
  assert.deepEqual(mount.offers, [KEEP.id], 'offered once per page');

  const quiet = fakeMount();
  const persistent = createIdentityActions({identity, persisted: async () => true});
  await persistent.attach(quiet);
  assert.deepEqual(quiet.offers, []);
  persistent.authored();
  assert.deepEqual(quiet.offers, [KEEP.id]);
});

test('keeping adds a passkey, publishes the envelope to every host, then offers a recovery key', async () => {
  const credentials = fakeAuthenticator();
  const identity = await browserIdentity({storage: memoryStorage(), persist: null, credentials});
  const mount = fakeMount();
  const published = [];
  const errors = [];
  const actions = createIdentityActions({identity, rpId: 'example.com', hosts: () => ['https://a.example', 'https://b.example'],
    persisted: async () => true, publish: async ({origin, identity: who}) => { published.push([origin, who.envelope.wraps.length]); },
    onError: error => errors.push(error)});
  await actions.attach(mount);
  await actions.perform(KEEP.id);
  assert.deepEqual(errors, []);
  assert.equal(identity.kept, true);
  assert.deepEqual(mount.actions, [RECOVERY.id]);
  assert.deepEqual(published, [['https://a.example', 1], ['https://b.example', 1]]);
  assert.deepEqual(mount.offers, [RECOVERY.id]);

  let saved;
  const recovering = createIdentityActions({identity, persisted: async () => true, publish: async () => {},
    save: async key => { saved = key; return 'file'; }});
  await recovering.perform(RECOVERY.id);
  const opened = await unlockIdentity(identity.envelope, {recoveryKey: saved});
  assert.equal(opened.userPublicKey, identity.userPublicKey);
});

test('signing in asks every known host for the envelope, then reloads as that person', async () => {
  const identity = await browserIdentity({storage: memoryStorage(), persist: null});
  const asked = [];
  let reloaded = false;
  const actions = createIdentityActions({identity, rpId: 'example.com', lookups: () => ['https://a.example', 'https://a.example', 'https://host.example'],
    find: async (root, origins) => { asked.push(origins); return {root}; },
    signIn: async ({fetchEnvelope}) => fetchEnvelope('ab'.repeat(32)),
    reload: () => { reloaded = true; }});
  await actions.perform(SIGN_IN.id);
  assert.deepEqual(asked, [['https://a.example', 'https://host.example']]);
  assert.equal(reloaded, true);
  const errors = [];
  await createIdentityActions({identity, onError: error => errors.push(error)}).perform('nope');
  assert.equal(errors[0].code, 'unknown-action');
});

test('setting a name names the person and offers to keep them', async () => {
  const identity = await browserIdentity({storage: memoryStorage(), persist: null});
  const mount = fakeMount();
  const named = [];
  const actions = createIdentityActions({identity, persisted: async () => true, name: () => 'Brave Otter',
    naming: {ask: async current => { assert.equal(current, 'Brave Otter'); return 'Ada'; }, set: chosen => named.push(chosen)}});
  await actions.attach(mount);
  assert.deepEqual(mount.actions, [NAME.id, KEEP.id, RECOVERY.id, SIGN_IN.id]);
  await actions.perform(NAME.id);
  assert.deepEqual(named, ['Ada']);
  assert.deepEqual(mount.offers, [KEEP.id]);

  const kept = fakeMount();
  const declined = createIdentityActions({identity, persisted: async () => true,
    naming: {ask: async () => null, set: chosen => named.push(chosen)}});
  await declined.attach(kept);
  await declined.perform(NAME.id);
  assert.deepEqual(named, ['Ada'], 'keeping the name asks nothing');
  assert.deepEqual(kept.offers, []);

  // Offered already when the page opened: naming is its own moment.
  const early = fakeMount();
  const fragile = createIdentityActions({identity, persisted: async () => false,
    naming: {ask: async () => 'Grace', set: () => {}}});
  await fragile.attach(early);
  await fragile.perform(NAME.id);
  assert.deepEqual(early.offers, [KEEP.id, KEEP.id]);
});

test('a chosen name goes to every host the person joined, as their profile', async () => {
  const identity = await browserIdentity({storage: memoryStorage(), persist: null});
  const published = [];
  const actions = createIdentityActions({identity, persisted: async () => true, hosts: () => ['https://a.example', 'https://b.example'],
    profile: async ({origin, identity: who, displayName}) => {
      assert.equal(who, identity);
      published.push([origin, displayName]);
      if (origin === 'https://b.example') throw new Error('down');
    },
    onError: () => {},
    naming: {ask: async (current, save) => save(' Ada '), set: () => 'Ada'}});
  await actions.attach(fakeMount());
  await actions.perform(NAME.id);
  assert.deepEqual(published, [['https://a.example', 'Ada'], ['https://b.example', 'Ada']]);
});

test('a name a host refuses is said in the form, which stays open, and is not taken', async () => {
  const identity = await browserIdentity({storage: memoryStorage(), persist: null});
  const set = [];
  const said = [];
  const errors = [];
  let refuse = true;
  const actions = createIdentityActions({identity, persisted: async () => true, hosts: () => ['https://a.example'],
    profile: async () => {
      if (refuse) throw new IdentityError('publish-refused', 'not a profile');
    },
    onError: error => errors.push(error),
    naming: {
      // The form: a refusal is shown, the person tries again.
      ask: async (current, save) => {
        try { await save('Ada'); } catch (error) { said.push(error.message); }
        refuse = false;
        return save('Ada Lovelace');
      },
      set: chosen => { set.push(chosen); return chosen; },
    }});
  await actions.attach(fakeMount());
  await actions.perform(NAME.id);
  assert.deepEqual(said, ['This name was not kept: not a profile']);
  assert.deepEqual(set, ['Ada Lovelace']);
  assert.deepEqual(errors, []);

  // An unreachable host is no refusal: the name is kept, the error reported.
  const down = createIdentityActions({identity, persisted: async () => true, hosts: () => ['https://a.example'],
    profile: async () => { throw new TypeError('fetch failed'); }, onError: error => errors.push(error),
    naming: {ask: async (current, save) => save('Grace'), set: chosen => { set.push(chosen); return chosen; }}});
  await down.attach(fakeMount());
  await down.perform(NAME.id);
  assert.deepEqual(set, ['Ada Lovelace', 'Grace']);
  assert.equal(errors.length, 1);
});

test('a device takes the name its person keeps on their hosts, and a root holder publishes one chosen only here', async () => {
  const identity = await browserIdentity({storage: memoryStorage(), persist: null});
  const userPublicKey = identity.userPublicKey;
  const applied = [];
  const published = [];
  const kept = {version: 1, user_public_key: userPublicKey, display_name: 'Ada Lovelace'};
  const asked = [];
  const find = async (root, origins) => { asked.push([root, origins]); return kept; };
  const publish = async ({origin, displayName}) => { published.push([origin, displayName]); };
  // A paired device: no root here, the kept name arrives.
  assert.equal(await syncProfile({userPublicKey, origins: ['https://a.example', 'https://a.example'], apply: name => applied.push(name),
    find, publish}), 'Ada Lovelace');
  assert.deepEqual(asked, [[userPublicKey, ['https://a.example']]]);
  assert.deepEqual(applied, ['Ada Lovelace']);
  // The kept name wins over an older one chosen here.
  await syncProfile({userPublicKey, origins: ['https://a.example'], identity, chosen: 'Ada', apply: name => applied.push(name), find, publish});
  assert.deepEqual(applied, ['Ada Lovelace', 'Ada Lovelace']);
  await syncProfile({userPublicKey, origins: ['https://a.example'], identity, chosen: 'Ada Lovelace', apply: name => applied.push(name), find, publish});
  assert.equal(applied.length, 2, 'the same name is not applied again');
  assert.deepEqual(published, []);
  // None kept yet: only the root holder publishes what it chose.
  const none = async () => null;
  await syncProfile({userPublicKey, origins: ['https://a.example', 'https://b.example'], chosen: 'Ada', apply: () => {}, find: none, publish});
  assert.deepEqual(published, []);
  await syncProfile({userPublicKey, origins: ['https://a.example', 'https://b.example'], identity, chosen: 'Ada', apply: () => {}, find: none, publish});
  assert.deepEqual(published, [['https://a.example', 'Ada'], ['https://b.example', 'Ada']]);
  await syncProfile({userPublicKey, origins: ['https://a.example'], identity, chosen: null, apply: () => {}, find: none, publish});
  assert.equal(published.length, 2, 'a name nobody chose is not published');
});

test('pairing copies a link, then offers each waiting device for approval and approves the one chosen', async () => {
  const identity = await browserIdentity({storage: memoryStorage(), persist: null});
  const mount = fakeMount();
  const copied = [];
  const answered = [];
  let pending = [];
  let poll;
  let clock = 0;
  const errors = [];
  const actions = createIdentityActions({identity, persisted: async () => true, onError: error => errors.push(error),
    copy: async text => copied.push(text), now: () => clock,
    every: run => { poll = run; return () => { poll = null; }; },
    pairing: {link: () => 'https://tilo.example/#/@rooms.example/w/ROOM?pair=ab', pending: async () => pending,
      answer: async (digest, approved) => answered.push([digest, approved])}});
  await actions.attach(mount);
  assert.ok(mount.actions.includes(PAIR.id));
  await actions.perform(PAIR.id);
  assert.deepEqual(copied, ['https://tilo.example/#/@rooms.example/w/ROOM?pair=ab']);
  assert.deepEqual(mount.offers, [PAIR.id]);

  pending = [{digest: 'd1', pairingCode: '123456', label: 'Browser on Linux', device: 'ee'.repeat(32)}];
  poll();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.deepEqual(mount.offers, [PAIR.id, 'approve-pairing:d1']);
  poll();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(mount.offers.length, 2, 'a waiting device is offered once');

  await actions.perform('approve-pairing:d1');
  assert.deepEqual(answered, [['d1', true]]);
  assert.deepEqual(mount.offers, [PAIR.id, 'approve-pairing:d1', PAIR.id]);
  assert.deepEqual(errors, []);

  clock = 11 * 60 * 1000;
  poll();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(poll, null, 'a stale pairing link is no longer watched');
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {setupOrigin,
  ACTION_GAP_MS, ASK_AFTER_ACTIONS, CONNECT_AGENT, createIdentityActions, LINKED_ID, NAME, PAIR, PHOTO,
  RESET_PHOTO, REVOKE_DEVICE, syncProfile,
} from './identity-actions.mjs';
import {openInvites} from './pairing.mjs';
import {IdentityError} from './identity.mjs';
import {fakeIdentity} from './testing/fake-identity.mjs';

function fakeMount() {
  return {
    actions: [], offers: [], subtitles: {}, said: [], acknowledged: [],
    acknowledge(text) { this.acknowledged.push(text); },
    setHostActions(actions) {
      this.actions = actions.map(action => action.id);
      for (const action of actions) this.subtitles[action.id] = action.subtitle;
    },
    offerHostAction(offer) { this.offers.push(offer.action.id); this.said.push(offer); },
  };
}

test('a key taken back is revoked by the root this tab holds, for the reason chosen', async () => {
  const revoked = [];
  const identity = {...fakeIdentity(), revokeDevice: async (key, reason) => { revoked.push([key, reason]); return {key}; }};
  const errors = [];
  const actions = createIdentityActions({identity, session: memorySession(),
    onError: error => errors.push(error)});
  await actions.perform(REVOKE_DEVICE, {key: 'ab'.repeat(32), reason: 'stolen'});
  assert.deepEqual(revoked, [['ab'.repeat(32), 'stolen']]);
  assert.deepEqual(errors, []);
});

function memorySession() {
  const map = new Map();
  return {getItem: key => map.get(key) ?? null, setItem: (key, value) => { map.set(key, value); }};
}

test('setting a name names the person', async () => {
  const identity = fakeIdentity();
  const mount = fakeMount();
  const named = [];
  const actions = createIdentityActions({identity, name: () => 'Brave Otter',
    naming: {ask: async current => { assert.equal(current, 'Brave Otter'); return 'Ada'; }, set: chosen => named.push(chosen)}});
  await actions.attach(mount);
  assert.deepEqual(mount.actions, [NAME.id]);
  await actions.perform(NAME.id);
  assert.deepEqual(named, ['Ada']);

  const kept = fakeMount();
  const declined = createIdentityActions({identity,
    naming: {ask: async () => null, set: chosen => named.push(chosen)}});
  await declined.attach(kept);
  await declined.perform(NAME.id);
  assert.deepEqual(named, ['Ada'], 'declining names nobody');
});

test('a chosen name goes to every host the person joined, as their profile', async () => {
  const identity = fakeIdentity();
  const published = [];
  const actions = createIdentityActions({identity, hosts: () => ['https://a.example', 'https://b.example'],
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

test('a chosen picture is kept in the profile with the name, worn at once, and can be taken away', async () => {
  const identity = fakeIdentity();
  const published = [];
  let current = null;
  const photos = {current: () => current, choose: async () => 'data:image/webp;base64,UklGRg==', show: url => { current = url; }};
  const mount = fakeMount();
  const actions = createIdentityActions({identity, hosts: () => ['https://a.example'], name: () => 'Ada',
    profile: async ({displayName, photo}) => { published.push([displayName, photo]); },
    photos, naming: {ask: async () => null, set: chosen => chosen}});
  await actions.attach(mount);
  assert.deepEqual(mount.actions, [NAME.id, PHOTO.id]);
  await actions.perform(PHOTO.id);
  assert.deepEqual(published, [['Ada', 'data:image/webp;base64,UklGRg==']]);
  assert.equal(current, 'data:image/webp;base64,UklGRg==');
  assert.deepEqual(mount.actions, [NAME.id, PHOTO.id, RESET_PHOTO.id]);
  // A new name keeps the picture.
  await actions.perform(NAME.id, 'Ada L');
  assert.deepEqual(published.at(-1), ['Ada L', 'data:image/webp;base64,UklGRg==']);
  await actions.perform(RESET_PHOTO.id);
  assert.deepEqual(published.at(-1), ['Ada', null]);
  assert.equal(current, null);
  // Choosing no file changes nothing.
  photos.choose = async () => null;
  await actions.perform(PHOTO.id);
  assert.equal(published.length, 3);
});

test('a linked account names itself in the profile and lends its picture until one is uploaded', async () => {
  const identity = fakeIdentity();
  const published = [];
  let current = null;
  const photos = {current: () => current, choose: async () => 'data:image/webp;base64,UklGRg==', show: url => { current = url; }};
  const actions = createIdentityActions({identity, hosts: () => ['https://a.example'], name: () => 'Ada',
    profile: async ({photo, photoFrom, linked}) => { published.push({photo, photoFrom, linked}); },
    photos, naming: {ask: async () => null, set: chosen => chosen}});
  await actions.attach(fakeMount());
  const avatar = 'data:image/png;base64,iVBORw0KGgo=';
  const discord = {service: 'Discord', handle: 'ada', picture: avatar};
  // Before the kept profile says whose the picture is, nothing changes.
  await actions.perform(LINKED_ID, {accounts: [discord], keep: true});
  assert.deepEqual(published, []);
  await actions.perform(LINKED_ID, {photoFrom: null, linked: [], keep: false});
  assert.deepEqual(published, [{photo: avatar, photoFrom: 'Discord', linked: [{service: 'Discord', handle: 'ada'}]}]);
  assert.equal(current, avatar);
  // The same answer again keeps nothing new.
  await actions.perform(LINKED_ID, {accounts: [discord], keep: true});
  assert.equal(published.length, 1);
  // A new avatar replaces the old.
  const newer = 'data:image/png;base64,AAAA';
  await actions.perform(LINKED_ID, {accounts: [{...discord, picture: newer}], keep: true});
  assert.equal(published.at(-1).photo, newer);
  // An uploaded picture wins over the account's.
  await actions.perform(PHOTO.id);
  assert.deepEqual(published.at(-1), {photo: 'data:image/webp;base64,UklGRg==', photoFrom: null,
    linked: [{service: 'Discord', handle: 'ada'}]});
  await actions.perform(LINKED_ID, {accounts: [discord], keep: true});
  assert.equal(current, 'data:image/webp;base64,UklGRg==');
  // Unlinking drops the handle, and keeps the upload.
  await actions.perform(LINKED_ID, {accounts: [], keep: true});
  assert.deepEqual(published.at(-1), {photo: 'data:image/webp;base64,UklGRg==', photoFrom: null, linked: []});
});

test('a new member with no saved profile wears their linked account’s picture', async () => {
  const identity = fakeIdentity();
  const published = [];
  let current = null;
  const photos = {current: () => current, choose: async () => null, show: url => { current = url; }};
  const actions = createIdentityActions({identity, hosts: () => ['https://a.example'], name: () => 'Ada',
    profile: async ({photo, photoFrom, linked}) => { published.push({photo, photoFrom, linked}); },
    photos, naming: {ask: async () => null, set: chosen => chosen}});
  await actions.attach(fakeMount());
  const avatar = 'data:image/png;base64,iVBORw0KGgo=';
  await actions.perform(LINKED_ID, {accounts: [{service: 'Discord', handle: 'ada', picture: avatar}], keep: true});
  assert.deepEqual(published, [], 'not before the hosts were asked');
  // Their hosts answered and keep no profile: read, so the picture goes on.
  const nones = [];
  await syncProfile({userPublicKey: identity.userPublicKey, origins: ['https://a.example'], apply: () => {},
    find: async () => null, onNone: () => nones.push('none')});
  // No host answered: not read, nothing is said.
  await syncProfile({userPublicKey: identity.userPublicKey, origins: ['https://a.example'], apply: () => {},
    find: async () => undefined, onNone: () => nones.push('unreachable')});
  assert.deepEqual(nones, ['none']);
  await actions.perform(LINKED_ID, {photoFrom: null, linked: [], keep: false});
  assert.deepEqual(published, [{photo: avatar, photoFrom: 'Discord', linked: [{service: 'Discord', handle: 'ada'}]}]);
  assert.equal(current, avatar);
});

test('unlinking takes away the picture the account lent', async () => {
  const identity = fakeIdentity();
  const published = [];
  const avatar = 'data:image/png;base64,iVBORw0KGgo=';
  let current = avatar;
  const photos = {current: () => current, choose: async () => null, show: url => { current = url; }};
  const actions = createIdentityActions({identity, hosts: () => ['https://a.example'], name: () => 'Ada',
    profile: async ({photo, photoFrom, linked}) => { published.push({photo, photoFrom, linked}); },
    photos, naming: {ask: async () => null, set: chosen => chosen}});
  await actions.attach(fakeMount());
  await actions.perform(LINKED_ID, {photoFrom: 'Discord', linked: [{service: 'Discord', handle: 'ada'}], keep: false});
  await actions.perform(LINKED_ID, {accounts: [], keep: true});
  assert.deepEqual(published, [{photo: null, photoFrom: null, linked: []}]);
  assert.equal(current, null);
});

test('a name typed in the desktop is kept as typed, with no form', async () => {
  const identity = fakeIdentity();
  const published = [];
  const set = [];
  const actions = createIdentityActions({identity, hosts: () => ['https://a.example'],
    profile: async ({displayName}) => { published.push(displayName); },
    naming: {ask: async () => assert.fail('the desktop already asked'), set: chosen => set.push(chosen)}});
  await actions.attach(fakeMount());
  await actions.perform(NAME.id, '  Kim  ');
  assert.deepEqual(published, ['Kim']);
  assert.deepEqual(set, ['Kim']);
});

test('a name a host refuses is said in the form, which stays open, and is not taken', async () => {
  const identity = fakeIdentity();
  const set = [];
  const said = [];
  const errors = [];
  let refuse = true;
  const actions = createIdentityActions({identity, hosts: () => ['https://a.example'],
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
  const down = createIdentityActions({identity, hosts: () => ['https://a.example'],
    profile: async () => { throw new TypeError('fetch failed'); }, onError: error => errors.push(error),
    naming: {ask: async (current, save) => save('Grace'), set: chosen => { set.push(chosen); return chosen; }}});
  await down.attach(fakeMount());
  await down.perform(NAME.id);
  assert.deepEqual(set, ['Ada Lovelace', 'Grace']);
  assert.equal(errors.length, 1);
});

test('a device takes the name its person keeps on their hosts, and a root holder publishes one chosen only here', async () => {
  const identity = fakeIdentity();
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

const INVITE_A = '0a'.repeat(16);
const INVITE_B = '0b'.repeat(16);

/** Pairing wired to fakes: the host's requests (`host.pending`), its
 * answers, the sheets and asks shown, and a clock and poll to drive. */
async function pairingFixture({invites = [INVITE_A, INVITE_B]} = {}) {
  const identity = fakeIdentity();
  const mount = fakeMount();
  const host = {pending: [], answers: [], fail: null};
  const sheets = [];
  const asks = [];
  const errors = [];
  const fixture = {identity, mount, host, sheets, asks, errors, clock: 0, poll: null};
  const ids = [...invites];
  const random = bytes => bytes.fill(Number.parseInt(ids.shift().slice(0, 2), 16));
  fixture.actions = createIdentityActions({identity, onError: error => errors.push(error),
    now: () => fixture.clock, invites: openInvites({now: () => fixture.clock, random}),
    copy: async () => {},
    every: run => { fixture.poll = run; return () => { fixture.poll = null; }; },
    pairing: {
      link: (invite, workspace = 'WORKSPACE') => `https://tilo.example/#/@workspaces.example/w/${workspace}?pair=${identity.userPublicKey}${invite}`,
      pending: async () => host.pending,
      answer: async (digest, approved) => {
        if (host.fail) throw host.fail;
        host.answers.push([digest, approved]);
        return approved ? '{"cert":{}}' : null;
      },
    },
    showPairLink: sheet => { const shown = {...sheet, closed: false, close() { shown.closed = true; }}; sheets.push(shown); return shown; },
    askPairing: (request, decide) => {
      const shown = {request, decide, closed: false, close() { shown.closed = true; }};
      asks.push(shown);
      return shown;
    }});
  fixture.tick = async () => { fixture.poll?.(); await new Promise(resolve => setTimeout(resolve, 0)); };
  await fixture.actions.attach(mount);
  return fixture;
}

const request = (digest, invite, extra = {}) => ({digest, pairingCode: '123456', label: 'Safari on iPhone', device: 'ee'.repeat(32),
  invite, requestedAtMs: 0, ...extra});

test('the card offers the agent setup, and choosing it shows that setup again', async () => {
  const identity = fakeIdentity();
  const mount = fakeMount();
  let shown = 0;
  const actions = createIdentityActions({identity, session: memorySession(),
    showAgentSetup: () => { shown += 1; }});
  await actions.attach(mount);
  assert.ok(mount.actions.includes(CONNECT_AGENT.id), 'the card offers it beside the devices');
  assert.equal(mount.subtitles[CONNECT_AGENT.id], CONNECT_AGENT.subtitle);
  await actions.perform(CONNECT_AGENT.id);
  assert.equal(shown, 1, 'the first screen\'s own setup, with a fresh code');
});

test('someone who arrived by an invitation is asked to set up their own Tilo once they used the workspace', async () => {
  const identity = fakeIdentity();
  const mount = fakeMount();
  let clock = 0;
  let shown = 0;
  let due = true;
  const actions = createIdentityActions({identity, session: memorySession(), now: () => clock,
    showAgentSetup: () => { shown += 1; }, setupDue: () => due});
  await actions.attach(mount);
  assert.deepEqual(mount.offers, [], 'joining is not interrupted');
  for (let action = 1; action <= ASK_AFTER_ACTIONS; action += 1) { clock += ACTION_GAP_MS; actions.authored(action); }
  assert.deepEqual(mount.offers, [CONNECT_AGENT.id], 'the setup');
  const [ask] = mount.said;
  assert.equal(ask.title, 'Want a workspace of your own?');
  assert.equal(ask.action.title, 'Set up with your agent');
  assert.equal(ask.action.source, 'identity',
    'asked from their avatar, never as a banner over the note they are in');
  assert.match(ask.body, /Ctrl\+K, “Connect an agent or computer”/, 'says where to find it later');
  await actions.perform(CONNECT_AGENT.id);
  assert.equal(shown, 1, 'the first screen’s setup, over the workspace they are in');
  // Set up already: nothing is asked.
  due = false;
  const later = fakeMount();
  const next = createIdentityActions({identity, session: memorySession(), now: () => clock,
    showAgentSetup: () => {}, setupDue: () => due});
  await next.attach(later);
  for (let action = 1; action <= ASK_AFTER_ACTIONS; action += 1) { clock += ACTION_GAP_MS; next.authored(action); }
  assert.deepEqual(later.offers, []);
});

test('without a page that can show the setup, the card offers nothing it cannot do', async () => {
  const identity = fakeIdentity();
  const mount = fakeMount();
  const actions = createIdentityActions({identity, session: memorySession()});
  await actions.attach(mount);
  assert.ok(!mount.actions.includes(CONNECT_AGENT.id));
});

test('each pairing link names a fresh invite, shown on a sheet that can copy it, open for ten minutes', async () => {
  const pairing = await pairingFixture();
  assert.ok(pairing.mount.actions.includes(PAIR.id));
  await pairing.actions.perform(PAIR.id);
  await pairing.actions.perform(PAIR.id);
  assert.deepEqual(pairing.sheets.map(sheet => sheet.link), [
    `https://tilo.example/#/@workspaces.example/w/WORKSPACE?pair=${pairing.identity.userPublicKey}${INVITE_A}`,
    `https://tilo.example/#/@workspaces.example/w/WORKSPACE?pair=${pairing.identity.userPublicKey}${INVITE_B}`,
  ]);
  assert.deepEqual(pairing.sheets.map(sheet => sheet.expiresAt), [10 * 60 * 1000, 10 * 60 * 1000]);
  assert.equal(await pairing.sheets[0].copy(), true);
  assert.deepEqual(pairing.mount.offers, [], 'no banner: the sheet is the pairing');

  pairing.clock = 10 * 60 * 1000;
  await pairing.tick();
  assert.ok(pairing.sheets.every(sheet => sheet.closed), 'an expired link’s sheet closes');
  assert.equal(pairing.poll, null, 'nothing is watched once no link is open');
  assert.deepEqual(pairing.errors, []);
});

test('only a request answering one of this page’s open invites is asked about, once per invite', async () => {
  const pairing = await pairingFixture();
  await pairing.actions.perform(PAIR.id);
  pairing.host.pending = [
    request('d0', null), request('d1', '0c'.repeat(16)), request('d2', INVITE_A), request('d3', INVITE_A),
  ];
  await pairing.tick();
  await pairing.tick();
  assert.deepEqual(pairing.asks.map(ask => ask.request.digest), ['d2'], 'the first device naming this page’s invite, once');
  assert.ok(pairing.sheets[0].closed, 'the ask takes the sheet’s place');
  assert.deepEqual(pairing.host.answers, [], 'nothing is answered without the person');
});

test('approving answers the request and spends its invite; a second device naming it is never asked about', async () => {
  const pairing = await pairingFixture();
  await pairing.actions.perform(PAIR.id);
  pairing.host.pending = [request('d1', INVITE_A)];
  await pairing.tick();
  assert.deepEqual(await pairing.asks[0].decide(true), {approved: true, label: 'Safari on iPhone'});
  assert.deepEqual(pairing.host.answers, [['d1', true]]);
  pairing.host.pending = [request('d2', INVITE_A)];
  await pairing.tick();
  assert.equal(pairing.asks.length, 1, 'the link worked once');
  assert.equal(pairing.poll, null, 'no link is open any more');
  assert.deepEqual(pairing.errors, []);
});

test('declining answers no and spends the invite too; an answer the host did not take can be given again', async () => {
  const pairing = await pairingFixture();
  await pairing.actions.perform(PAIR.id);
  await pairing.actions.perform(PAIR.id);
  pairing.host.pending = [request('d1', INVITE_A), request('d2', INVITE_B)];
  await pairing.tick();
  assert.equal(pairing.asks.length, 2);
  pairing.host.fail = new Error('offline');
  await assert.rejects(pairing.asks[1].decide(true), /offline/);
  pairing.host.fail = null;
  assert.deepEqual(await pairing.asks[0].decide(false), {approved: false, label: 'Safari on iPhone'});
  assert.deepEqual(await pairing.asks[1].decide(true), {approved: true, label: 'Safari on iPhone'});
  assert.deepEqual(pairing.host.answers, [['d1', false], ['d2', true]]);
  await pairing.tick();
  assert.equal(pairing.asks.length, 2);
  assert.equal(pairing.poll, null);
});

test('an ask still open when its link expires closes', async () => {
  const pairing = await pairingFixture();
  await pairing.actions.perform(PAIR.id);
  pairing.host.pending = [request('d1', INVITE_A)];
  await pairing.tick();
  pairing.clock = 11 * 60 * 1000;
  await pairing.tick();
  assert.ok(pairing.asks[0].closed);
  assert.equal(pairing.poll, null);
});

test('an agent setup code comes from the workspace in front, else the home host', () => {
  const home = {host: {origin: 'https://eu1.tilo.sh'}};
  assert.equal(setupOrigin({peer: {origin: 'https://other.example'}, home}), 'https://other.example');
  // The person's own desktop has no workspace peer: the card still offers
  // the setup, from the home host.
  assert.equal(setupOrigin({peer: null, home}), 'https://eu1.tilo.sh');
  assert.equal(setupOrigin({peer: null, home: null}), null);
});

test('a workspace reached only over iroh asks its codes at the site’s home server, when that is the same one', () => {
  const ENDPOINT = 'b'.repeat(64);
  // The site's entry names its home server, as entry.json does.
  const home = {host: {origin: 'http://127.0.0.1:24300', info: JSON.stringify({
    hostCert: {}, address: {endpointAddr: {id: ENDPOINT}},
  })}};
  // Its invitation named the server by its endpoint alone: the address the
  // page reached it on has no HTTP, so the site's home server answers, as it
  // does for a hosted site's page.
  const invited = {origin: 'https://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.iroh.invalid',
    host: {address: {endpointAddr: {id: ENDPOINT}}}};
  assert.equal(setupOrigin({peer: invited, home}), 'http://127.0.0.1:24300');
  // Another endpoint is another server: its workspace has no HTTP address here.
  const elsewhere = {origin: invited.origin, host: {address: {endpointAddr: {id: 'c'.repeat(64)}}}};
  assert.equal(setupOrigin({peer: elsewhere, home}), null);
});

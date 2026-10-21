import test from 'node:test';
import assert from 'node:assert/strict';
import {chooseDisplayName, chooseWorkspace, chosenDisplayName, chosenWorkspace, doorRefused, enteredWorkspaces, forgetGoneRooms, HostAdmissionError, hostDeviceOf, joinViaHost, keptMembership, listedWorkspaces, markUnavailable, nameEntered, pairViaHost, defaultDisplayName, randomDisplayName, rememberEntered, removedFrom, settleFailedRoom, spectate, workspaceActions, workspaceAt, workspaceChoices} from './workspaces.mjs';
import {CustodyError} from './device-custody.mjs';
import {HOST_CHANGED} from './peer.mjs';

const identity = key => ({kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(key), userPublicKey: key.toString(16).padStart(2, '0').repeat(32), sign: async () => new Uint8Array(64)});

const commons = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
const KEY = 'tilo-host-memberships';

const reply = ({status = 200, headers = {}, body = {}}) => ({
  ok: status === 200, status, headers: new Map(Object.entries(headers)), json: async () => body,
  text: async () => (typeof body === 'string' ? body : JSON.stringify(body)),
});

/** A device key of `profile` as `openDeviceCustody` gives it: `created` for
 * one no host has seen yet (a new tab's). */
const custodyOf = (calls, created) => async profile => {
  const custody = {profile, slot: created ? 1 : 0, created, closed: false, close: async () => { custody.closed = true; }};
  calls.custodies.push(custody);
  return custody;
};

function harness({person = identity(0x22), membership = null, joinReply = {status: 200, body: {invitation: 'INV', admissionToken: 'TOKEN', origin: 'https://rooms.example'}}, connectFails = false, created = false} = {}) {
  const calls = {posted: [], joined: [], connected: [], paired: [], custodies: []};
  const store = new Map(membership ? [[KEY, JSON.stringify({'https://rooms.example': membership})]] : []);
  return {
    calls,
    options: {
      workspace: commons,
      identity: person,
      displayName: 'Brave Otter',
      memberships: {getItem: key => store.get(key) ?? null, setItem: (key, value) => store.set(key, value), removeItem: key => store.delete(key)},
      fetch: async (url, init) => {
        calls.posted.push({url: String(url), body: JSON.parse(init.body)});
        return reply(joinReply);
      },
      join: async options => { calls.joined.push(options); options.onProgress?.({phase: 'admitted', room: 'ROOM'}); return {room: 'ROOM', dispose: async () => {}}; },
      connect: async options => { calls.connected.push(options); if (connectFails) throw new Error('refused: not a member'); return {room: options.room}; },
      pair: async options => { calls.paired.push(options); calls.offer = await options.admission(); return {room: options.room}; },
      openCustody: custodyOf(calls, created),
    },
    store,
  };
}

test('a person new to the host is pre-admitted by it and joins with the pre-admission', async () => {
  const {calls, options, store} = harness();
  await joinViaHost(options);
  assert.equal(calls.posted[0].url, 'https://rooms.example/join');
  assert.deepEqual(calls.posted[0].body, {userPublicKey: '22'.repeat(32), displayName: 'Brave Otter'});
  assert.equal(calls.joined[0].invitation, 'INV');
  assert.equal(calls.joined[0].admissionToken, 'TOKEN');
  assert.equal(calls.joined[0].origin, 'https://rooms.example');
  assert.equal(calls.joined[0].identity.userPublicKey, '22'.repeat(32));
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '22'.repeat(32)});
});

test('a host refusing the door\'s own invitation is asked once more, then named without blaming an invitation', async () => {
  const refusal = () => Object.assign(new Error('refused'), {name: 'AdmissionError', code: 'refused', reason: 'no seat'});
  const {calls, options} = harness();
  let refusals = 1;
  options.join = async joined => {
    calls.joined.push(joined);
    if (refusals-- > 0) throw refusal();
    return {room: 'ROOM'};
  };
  assert.deepEqual(await joinViaHost(options), {room: 'ROOM'});
  assert.equal(calls.posted.length, 2, 'a fresh invitation from the door');

  const again = harness();
  again.options.join = async () => { throw refusal(); };
  await assert.rejects(joinViaHost(again.options), error => {
    assert.equal(error.message, doorRefused('Commons'));
    assert.doesNotMatch(error.message, /invit/i);
    assert.equal(error.reason, 'no seat');
    return true;
  });
  assert.equal(again.calls.posted.length, 2);
});

test('a person joins at once under a random adjective-animal name when none is given', async () => {
  const {calls, options} = harness();
  delete options.displayName;
  await joinViaHost(options);
  assert.match(calls.posted[0].body.displayName, /^[A-Z][a-z]+ [A-Z][a-z]+$/);
  assert.equal(calls.joined[0].displayName, calls.posted[0].body.displayName);
});

test('a person who joined the host before reconnects without asking it', async () => {
  const {calls, options} = harness({person: identity(0x11), membership: {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '11'.repeat(32)}});
  await joinViaHost(options);
  assert.equal(calls.posted.length, 0);
  assert.equal(calls.connected[0].identity.userPublicKey, '11'.repeat(32));
  assert.equal(calls.connected[0].room, 'ROOM');
});

test('a person who was pruned is pre-admitted again as the same person', async () => {
  const {calls, options} = harness({person: identity(0x11), membership: {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '11'.repeat(32)}, connectFails: true});
  await joinViaHost(options);
  assert.equal(calls.posted[0].body.userPublicKey, '11'.repeat(32));
});

test('a kept membership the host refuses once the session starts is joined again as the same person', async () => {
  // A host that started its room anew answers the old certificate only when
  // the session begins, after connecting resolved.
  const membership = {origin: 'https://rooms.example', room: 'OLD', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  let disposed = false;
  options.connect = async connected => {
    calls.connected.push(connected);
    return {room: connected.room, dispose: async () => { disposed = true; },
      whenReady: async () => { throw new Error('This workspace’s host no longer has you as a member.'); }};
  };
  const peer = await joinViaHost(options);
  assert.ok(disposed);
  assert.equal(calls.posted[0].body.userPublicKey, '11'.repeat(32));
  assert.equal(peer.room, 'ROOM');
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '11'.repeat(32)});
});

test('a person the host removed is told so, asks the door once and is not let in again', async () => {
  // Removed and pruned look alike to the returning browser: the host no
  // longer has the person. The door tells them apart, turning away the
  // address a removed person joined from.
  const membership = {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership,
    joinReply: {status: 403, body: {error: 'this address is cooling down after a removal'}}});
  options.connect = async connected => {
    calls.connected.push(connected);
    return {room: connected.room, dispose: async () => {},
      whenReady: async () => { throw new Error('device was removed from workspace membership'); }};
  };
  await assert.rejects(joinViaHost(options), {name: 'AdmissionError', code: 'removed', message: removedFrom('Commons')});
  assert.equal(calls.posted.length, 1);
  assert.equal(calls.joined.length, 0);
  assert.equal(keptMembership(commons, {getItem: key => store.get(key)}), null);
  assert.match(removedFrom('Commons'), /^You were removed from Commons\. .*new invitation/);
  assert.match(removedFrom(null), /^You were removed from this workspace\./);
});

test('an address cooling down after a removal turns away someone who kept no membership, as the door said', async () => {
  const {calls, options} = harness({person: identity(0x22), joinReply: {status: 403, body: {error: 'cooling down'}}});
  await assert.rejects(joinViaHost(options), {name: 'HostAdmissionError', code: 'cooling-down'});
  assert.equal(calls.posted.length, 1);
  assert.equal(calls.connected.length, 0);
});

test('a host that has not answered a kept membership yet keeps it and is not asked to admit again', async () => {
  const membership = {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.connect = async connected => ({room: connected.room, dispose: async () => {},
    whenReady: async () => { throw new DOMException('Room admission timed out', 'TimeoutError'); }});
  await assert.rejects(joinViaHost(options), {name: 'TimeoutError'});
  assert.equal(calls.posted.length, 0);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
});

test('a second tab is a new device of the person, admitted through the door once', async () => {
  // Its device key is one no host has seen: the room admits each device
  // once, so it asks the door for the same person rather than fail to connect.
  const membership = {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership, created: true});
  const peer = await joinViaHost(options);
  assert.equal(calls.connected.length, 0, 'no connection the room would refuse');
  assert.deepEqual(calls.posted.map(post => post.body.userPublicKey), ['11'.repeat(32)]);
  assert.equal(calls.joined[0].custody, calls.custodies[0], 'joined with the new tab\u2019s own key');
  assert.equal(calls.custodies[0].profile, `tilo-identity:${'11'.repeat(32)}@rooms.example`);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), {...membership, room: 'ROOM'});
  // The tab's key is let go with its peer.
  assert.equal(calls.custodies[0].closed, false);
  await peer.dispose();
  assert.equal(calls.custodies[0].closed, true);
});

test('a second tab of a paired device pairs as a new device, approved on one of the person\u2019s devices', async () => {
  const person = '33'.repeat(32);
  const membership = {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: person, paired: true};
  const {calls, options} = harness({membership, created: true});
  await joinViaHost({...options, identity: undefined, onPairingCode: () => {}});
  assert.equal(calls.connected.length, 0);
  assert.equal(calls.paired[0].userPublicKey, person);
  assert.equal(calls.paired[0].custody, calls.custodies[0]);
  assert.equal(typeof calls.paired[0].onPairingCode, 'function');
  assert.deepEqual(calls.posted, [{url: 'https://rooms.example/join', body: {userPublicKey: person, displayName: 'Brave Otter'}}],
    'the new device joins the person\u2019s workspace through the door once paired');
});

test('every device key held by other tabs keeps the membership and asks the host nothing', async () => {
  // Not the host's refusal: this browser has as many tabs open as device
  // keys. Joining again would spend a seat and the address's join window,
  // then fail the same way.
  const membership = {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.openCustody = async () => { throw new CustodyError('custody-busy', 'Every device key is held by another tab'); };
  await assert.rejects(joinViaHost(options), {name: 'CustodyError', code: 'custody-busy'});
  assert.equal(calls.posted.length, 0);
  assert.equal(calls.joined.length, 0);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
});

test('a host set up again is joined again through its door once, without asking', async () => {
  // The peer refuses what this browser kept for the previous host before
  // any session starts (peer.mjs); the person simply joins the new host.
  const membership = {origin: 'https://rooms.example', room: 'OLD', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.connect = async connected => {
    calls.connected.push(connected);
    throw Object.assign(new Error(HOST_CHANGED), {name: 'AdmissionError', code: 'host-changed'});
  };
  const peer = await joinViaHost(options);
  assert.equal(calls.connected.length, 1, 'the kept membership was tried once');
  assert.equal(calls.posted.length, 1, 'exactly one join at the door');
  assert.equal(calls.posted[0].body.userPublicKey, '11'.repeat(32));
  assert.equal(calls.joined.length, 1);
  assert.equal(calls.joined[0].identity.userPublicKey, '11'.repeat(32));
  assert.equal(peer.room, 'ROOM');
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}),
    {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '11'.repeat(32)});
});

test('a membership kept for another person is not reused for this one', async () => {
  const {calls, options} = harness({person: identity(0x33), membership: {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '11'.repeat(32)}});
  await joinViaHost(options);
  assert.equal(calls.connected.length, 0);
  assert.equal(calls.posted[0].body.userPublicKey, '33'.repeat(32));
});

test('a workspace whose host needs an invitation, or a call without a person, is refused before anything is asked', async () => {
  const {calls, options} = harness();
  await assert.rejects(joinViaHost({...options, workspace: {name: 'Team', host: {origin: 'https://team.example'}}}),
    error => error instanceof HostAdmissionError && error.code === 'invitation-only');
  await assert.rejects(joinViaHost({...options, identity: undefined}), TypeError);
  assert.equal(calls.posted.length, 0);
});

test('a full room and a cooldown are told apart', async () => {
  const full = harness({joinReply: {status: 503, headers: {'retry-after': '60'}, body: {error: 'the workspace is full'}}});
  await assert.rejects(joinViaHost(full.options), error => error instanceof HostAdmissionError && error.code === 'full' && error.retryAfter === 60);
  const down = harness({joinReply: {status: 503, body: {error: 'the room is starting'}}});
  await assert.rejects(joinViaHost(down.options), error => error.code === 'unavailable');
  const cooling = harness({joinReply: {status: 403, body: {error: 'removed'}}});
  await assert.rejects(joinViaHost(cooling.options), error => error.code === 'cooling-down');
});

test('display names are an adjective and an animal', () => {
  assert.match(randomDisplayName(() => 0), /^[A-Z][a-z]+ [A-Z][a-z]+$/);
});

test('a person goes by the same name at every visit until they choose one', () => {
  const key = 'a1'.repeat(32);
  const name = defaultDisplayName(key, () => 0);
  assert.match(name, /^[A-Z][a-z]+ [A-Z][a-z]+$/);
  assert.equal(defaultDisplayName(key, () => 0.99), name);
  assert.notEqual(defaultDisplayName('07'.repeat(32)), name);
  assert.match(defaultDisplayName(undefined, () => 0), /^[A-Z][a-z]+ [A-Z][a-z]+$/);
});

test('the home workspace is listed first, then the person\u2019s own, each host once', () => {
  const team = {name: 'Team', host: {origin: 'https://team.example'}, room: 'r'};
  const again = {name: 'Commons again', host: {origin: 'https://rooms.example/'}};
  assert.deepEqual(listedWorkspaces({stored: [team]}), [team]);
  assert.deepEqual(listedWorkspaces({home: commons, stored: [again, team]}), [commons, team]);
  assert.deepEqual(listedWorkspaces(), []);
  assert.equal(workspaceAt(listedWorkspaces({stored: [team]}), 'https://team.example'), team);
  assert.equal(workspaceAt([team], 'https://elsewhere.example'), null);
});

function spectating(spectateReply) {
  const calls = {posted: [], connected: []};
  // A real clock reading, beyond 2^53: it must reach the runtime unrounded.
  const certificate = '{"user_public_key":[' + Array(32).fill(0x5e).join(',') + '],"device_public_key":[' + Array(32).fill(1).join(',')
    + '],"device_label":"Spectator","issued_at_ns":1791962530023125779}';
  return {
    calls,
    certificate,
    options: {
      workspace: commons,
      fetch: async (url, init) => {
        calls.posted.push({url: String(url), body: JSON.parse(init.body)});
        return reply(spectateReply ?? {body: {certificate, origin: 'https://rooms.example', room: 'ROOM'}});
      },
      runtime: {hostInfo: async () => JSON.stringify({hostCert: {device_public_key: Array(32).fill(9)}})},
      connect: async options => { calls.connected.push(options); return {room: options.room}; },
    },
  };
}

test('a spectator certifies an in-memory device key and connects without a person or storage', async () => {
  const {calls, certificate, options} = spectating();
  await spectate(options);
  assert.equal(calls.posted[0].url, 'https://rooms.example/spectate');
  const {devicePublicKey} = calls.posted[0].body;
  assert.match(devicePublicKey, /^[0-9a-f]{64}$/);
  const [connected] = calls.connected;
  assert.equal(connected.identity, undefined);
  assert.equal(connected.profile, undefined);
  assert.equal(connected.origin, 'https://rooms.example');
  assert.equal(connected.room, 'ROOM');
  assert.equal(connected.userPublicKey, '5e'.repeat(32));
  assert.equal(connected.watching, true, 'the page only watches');
  assert.equal(Array.from(connected.custody.publicKey, b => b.toString(16).padStart(2, '0')).join(''), devicePublicKey);
  assert.equal(connected.deviceCertificate, `{"cert":${certificate},"hostCert":{"device_public_key":[${Array(32).fill(9).join(',')}]}}`);
  assert.match(connected.deviceCertificate, /"issued_at_ns":1791962530023125779[,}]/);
  // The key signs, and cannot be exported: nothing about it outlives the page.
  assert.equal((await connected.custody.sign(new Uint8Array([1]))).length, 64);
  await connected.store.put('k', 'v');
  assert.equal(await connected.store.get('k'), 'v');
});

test("the host's certificate reaches a spectator's session byte for byte, its nanosecond timestamps intact", async () => {
  const nanos = '1792023733547123457';
  const certificate = `{"user_public_key":[${Array(32).fill(94)}],"device_public_key":[1],"issued_at_ns":${nanos}}`;
  const {calls, options} = spectating({body: {certificate, origin: 'https://rooms.example', room: 'ROOM'}});
  options.runtime = {hostInfo: async () => `{"hostCert":{"issued_at_ns":${nanos}}}`};
  await spectate(options);
  assert.equal(calls.connected[0].deviceCertificate, `{"cert":${certificate},"hostCert":{"issued_at_ns":${nanos}}}`);
  assert.equal(calls.connected[0].userPublicKey, '5e'.repeat(32));
});

test('above the spectator cap the embed is told the room is full', async () => {
  const {calls, options} = spectating({status: 503, headers: {'retry-after': '30'}, body: {error: 'too many people are watching'}});
  await assert.rejects(spectate(options), error => error instanceof HostAdmissionError && error.code === 'full' && error.retryAfter === 30);
  assert.equal(calls.connected.length, 0);
});

test('a paired device is kept as its person and reconnects as them, never as a person of its own', async () => {
  const {calls, options, store} = harness();
  const person = '33'.repeat(32);
  const codes = [];
  await pairViaHost({origin: 'https://rooms.example', room: 'ROOM', userPublicKey: person, invite: '0f'.repeat(16), workspace: commons,
    displayName: 'Ada', memberships: options.memberships, fetch: options.fetch, onPairingCode: code => codes.push(code),
    pair: async pairing => { calls.connected.push(pairing); calls.offer = await pairing.admission(); return {room: 'ROOM'}; }});
  const paired = calls.connected[0];
  assert.equal(paired.userPublicKey, person);
  assert.equal(paired.invite, '0f'.repeat(16), 'the request answers the link\u2019s invite');
  assert.equal(paired.identity, undefined, 'the root stays on the device that approves');
  assert.equal(paired.profile, `tilo-identity:${person}@rooms.example`);
  assert.equal(typeof paired.onPairingCode, 'function');
  assert.deepEqual(calls.posted, [{url: 'https://rooms.example/join', body: {userPublicKey: person, displayName: 'Ada'}}],
    'the new device joins the person\u2019s workspace through the host\u2019s door');
  assert.equal(calls.offer.invitation, 'INV');
  calls.posted.length = 0;
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}),
    {origin: 'https://rooms.example', room: 'ROOM', userPublicKey: person, paired: true});

  await joinViaHost({...options, identity: undefined});
  assert.equal(calls.posted.length, 0, 'the host is not asked to admit another person');
  assert.equal(calls.connected[1].userPublicKey, person);
  assert.equal(calls.connected[1].profile, paired.profile);
  assert.equal(calls.connected[1].room, 'ROOM');
});

test('a pairing link that names no person is refused before anything is asked', async () => {
  const {calls, options} = harness();
  await assert.rejects(pairViaHost({origin: 'https://rooms.example', room: 'ROOM', userPublicKey: 'nobody', pair: options.connect,
    memberships: options.memberships}), {code: 'invalid'});
  await assert.rejects(pairViaHost({origin: 'https://rooms.example', room: 'ROOM', userPublicKey: '33'.repeat(32), invite: 'short',
    pair: options.connect, memberships: options.memberships}), {code: 'invalid'});
  assert.equal(calls.connected.length, 0);
});

test('a chosen name is kept per person in this browser', () => {
  const store = new Map();
  const names = {getItem: key => store.get(key) ?? null, setItem: (key, value) => store.set(key, value)};
  assert.equal(chosenDisplayName('aa', names), null);
  assert.equal(chooseDisplayName('aa', '  Ada Lovelace ', names), 'Ada Lovelace');
  assert.equal(chosenDisplayName('aa', names), 'Ada Lovelace');
  assert.equal(chosenDisplayName('bb', names), null);
  chooseDisplayName('aa', '', names);
  assert.equal(chosenDisplayName('aa', names), null);
});

/** The host that held the rooms these tests remember. */
const HOST = 'ab'.repeat(32);

const memoryStore = () => {
  const map = new Map();
  return {getItem: key => map.get(key) ?? null, setItem: (key, value) => map.set(key, String(value)), removeItem: key => map.delete(key)};
};

test('rooms entered as a member are remembered per host and room, most recently opened first', () => {
  const store = memoryStore();
  rememberEntered({hostDevice: HOST, origin: 'https://acme.example/', room: 'A', name: 'Acme'}, store);
  rememberEntered({hostDevice: HOST, origin: 'https://globex.example', room: 'G'}, store);
  // Two rooms on one host are two workspaces.
  rememberEntered({hostDevice: HOST, origin: 'https://globex.example', room: 'H', name: 'Hank'}, store);
  assert.deepEqual(enteredWorkspaces(store).map(entry => [entry.origin, entry.room, entry.name]), [
    ['https://globex.example', 'H', 'Hank'], ['https://globex.example', 'G', null], ['https://acme.example', 'A', 'Acme'],
  ]);
  // Opening one again moves it to the front and keeps the name it had.
  rememberEntered({hostDevice: HOST, origin: 'https://acme.example', room: 'A'}, store);
  assert.deepEqual(enteredWorkspaces(store)[0], {origin: 'https://acme.example', room: 'A', hostDevice: HOST, name: 'Acme', unavailable: false});
  // The desktop names a room only when nothing named it yet.
  nameEntered({origin: 'https://globex.example', room: 'G', name: 'Globex'}, store);
  nameEntered({origin: 'https://acme.example', room: 'A', name: 'Front workspace'}, store);
  assert.deepEqual(enteredWorkspaces(store).map(entry => entry.name), ['Acme', 'Hank', 'Globex']);
});

test('the remembered rooms are bounded, and damaged or roomless entries are never offered', () => {
  const store = memoryStore();
  for (let index = 0; index < 40; index += 1) rememberEntered({hostDevice: HOST, origin: `https://h${index}.example`, room: `R${index}`}, store);
  const entered = enteredWorkspaces(store);
  assert.equal(entered.length, 32);
  assert.equal(entered[0].room, 'R39');
  rememberEntered({hostDevice: HOST, origin: 'https://nowhere.example', room: null}, store);
  assert.equal(enteredWorkspaces(store).length, 32);
  // One that names no host that held it could never be told gone: not offered.
  store.setItem('tilo-entered-workspaces', JSON.stringify([{origin: 'not a url', room: 'X', hostDevice: HOST},
    {origin: 'https://ok.example', room: 'OK', hostDevice: HOST}, {origin: 'https://old.example', room: 'O'}, 7]));
  assert.deepEqual(enteredWorkspaces(store), [{origin: 'https://ok.example', room: 'OK', hostDevice: HOST, name: null, unavailable: false}]);
  store.setItem('tilo-entered-workspaces', '{broken');
  assert.deepEqual(enteredWorkspaces(store), []);
});

test('a bare entry reopens the chosen room until it refuses this browser, and choosing the home forgets it', () => {
  const store = memoryStore();
  rememberEntered({hostDevice: HOST, origin: 'https://acme.example', room: 'A', name: 'Acme'}, store);
  // A choice this browser never entered is not reopened.
  chooseWorkspace({origin: 'https://other.example', room: 'O'}, store);
  assert.equal(chosenWorkspace(store), null);
  chooseWorkspace({origin: 'https://acme.example/', room: 'A'}, store);
  assert.deepEqual(chosenWorkspace(store), {origin: 'https://acme.example', room: 'A'});
  chooseWorkspace(null, store);
  assert.equal(chosenWorkspace(store), null);
  chooseWorkspace({origin: 'https://acme.example', room: 'A'}, store);
  markUnavailable({origin: 'https://acme.example', room: 'A'}, store);
  assert.equal(chosenWorkspace(store), null);
  assert.equal(enteredWorkspaces(store)[0].unavailable, true);
  // Entering it again clears the mark.
  rememberEntered({hostDevice: HOST, origin: 'https://acme.example', room: 'A'}, store);
  assert.equal(enteredWorkspaces(store)[0].unavailable, false);
});

test('a remembered room whose origin answers with another host is forgotten; one whose host is not reached stays', async () => {
  const store = memoryStore();
  const NEW_HOST = 'cd'.repeat(32);
  // Two rooms on a host since set up again on fresh state, and one on a host that is offline.
  rememberEntered({hostDevice: HOST, origin: 'https://lounge.example', room: 'OLD', name: 'Lounge'}, store);
  rememberEntered({hostDevice: HOST, origin: 'https://lounge.example', room: 'OLDER', name: 'Lounge'}, store);
  rememberEntered({hostDevice: HOST, origin: 'https://away.example', room: 'AWAY', name: 'Away'}, store);
  chooseWorkspace({origin: 'https://lounge.example', room: 'OLD'}, store);
  const asked = [];
  const hostInfo = async origin => {
    asked.push(origin);
    if (origin === 'https://lounge.example') return {device_public_key: Array(32).fill(0xcd), user_public_key: Array(32).fill(1)};
    throw new TypeError('Failed to fetch');
  };
  assert.equal(await settleFailedRoom({origin: 'https://lounge.example/', room: 'OLD'}, {hostInfo, store}), 'gone');
  // The other room the previous host held went with it; the offline host's room stays, marked.
  assert.deepEqual(enteredWorkspaces(store).map(entry => entry.room), ['AWAY']);
  assert.equal(chosenWorkspace(store), null);
  assert.equal(await settleFailedRoom({origin: 'https://away.example', room: 'AWAY'}, {hostInfo, store}), 'unavailable');
  assert.deepEqual(enteredWorkspaces(store).map(entry => [entry.room, entry.unavailable]), [['AWAY', true]]);
  assert.deepEqual(asked, ['https://lounge.example', 'https://away.example']);
  // The same host answering, or an answer that names no host, is no news about the room either.
  rememberEntered({hostDevice: NEW_HOST, origin: 'https://lounge.example', room: 'NEW', name: 'Lounge'}, store);
  for (const answer of [{device_public_key: NEW_HOST}, {}, null]) {
    assert.equal(await settleFailedRoom({origin: 'https://lounge.example', room: 'NEW'}, {hostInfo: async () => answer, store}), 'unavailable');
  }
  assert.deepEqual(enteredWorkspaces(store).map(entry => entry.room), ['NEW', 'AWAY']);
  assert.equal(await settleFailedRoom({origin: 'https://nowhere.example', room: 'X'}, {hostInfo, store}), null);
});

test('the host a page is connected to forgets the rooms another host held at its origin', () => {
  const store = memoryStore();
  const NEW_HOST = hostDeviceOf({device_public_key: Array(32).fill(0xcd)});
  assert.equal(NEW_HOST, 'cd'.repeat(32));
  assert.equal(hostDeviceOf({device_public_key: 'not hex'}), null);
  assert.equal(hostDeviceOf(undefined), null);
  rememberEntered({hostDevice: HOST, origin: 'https://lounge.example', room: 'OLD', name: 'Lounge'}, store);
  rememberEntered({hostDevice: HOST, origin: 'https://team.example', room: 'TEAM', name: 'Team'}, store);
  rememberEntered({hostDevice: NEW_HOST, origin: 'https://lounge.example', room: 'SIDE', name: 'Side'}, store);
  // Remembering a room under the new host already forgot the one before it.
  assert.deepEqual(enteredWorkspaces(store).map(entry => entry.room), ['SIDE', 'TEAM']);
  rememberEntered({hostDevice: HOST, origin: 'https://lounge.example', room: 'OLD', name: 'Lounge'}, store);
  assert.deepEqual(forgetGoneRooms({origin: 'https://lounge.example/', hostDevice: NEW_HOST}, store),
    [{origin: 'https://lounge.example', room: 'OLD'}]);
  // Rooms of that host, and of other origins, stay; a host that names no key forgets nothing.
  assert.deepEqual(enteredWorkspaces(store).map(entry => entry.room), ['TEAM']);
  assert.deepEqual(forgetGoneRooms({origin: 'https://team.example', hostDevice: null}, store), []);
  assert.deepEqual(forgetGoneRooms({origin: 'https://team.example', hostDevice: HOST}, store), []);
  assert.deepEqual(enteredWorkspaces(store).map(entry => entry.room), ['TEAM']);
});

test('workspaces that share a name are told apart in the menu, others keep their plain name', () => {
  const choice = (origin, room, name) => ({id: `open-workspace:${origin}/${room}`, name, origin, room, home: false, unavailable: false});
  const labels = (choices, current) => workspaceActions(choices, {current}).map(action => action.label);
  // The common case: distinct names, nothing added.
  assert.deepEqual(labels([choice('https://a.example', 'A', 'Acme'), choice('https://b.example', 'B', 'Lounge')], 'Team'), ['Acme', 'Lounge']);
  // Two Lounges on two hosts, beside a third in front: each says its host.
  assert.deepEqual(labels([choice('https://a.example', 'A', 'Lounge'), choice('https://b.example', 'B', 'Lounge')], 'Lounge'),
    ['Lounge · a.example', 'Lounge · b.example']);
  assert.deepEqual(labels([choice('https://a.example', 'A', 'Lounge')], 'Lounge'), ['Lounge · a.example']);
  // Two on one host: their rooms too.
  const one = [choice('https://a.example', '5e8d4510-aaaa', 'Lounge'), choice('https://a.example', '84c928ed-bbbb', 'Lounge')];
  assert.deepEqual(labels(one, null), ['Lounge · a.example · 5e8d4510', 'Lounge · a.example · 84c928ed']);
  assert.equal(workspaceActions(one)[0].title, 'Open Lounge · a.example · 5e8d4510');
});

test('the person switches between the home and the workspaces they entered, never to the one in front', () => {
  const home = {name: 'Lounge', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const entered = [
    {origin: 'https://acme.example', room: 'A', name: 'Acme', unavailable: false},
    {origin: 'https://globex.example', room: 'G', name: null, unavailable: true},
    // The room the home's door admitted this browser to is the home itself.
    {origin: 'https://rooms.example', room: 'LOUNGE', name: 'Lounge', unavailable: false},
    // Another room on the home's host is a workspace of its own.
    {origin: 'https://rooms.example', room: 'TEAM', name: 'Team', unavailable: false},
  ];
  const fromAcme = workspaceChoices({home, homeRoom: 'LOUNGE', entered, current: {kind: 'room', host: 'https://acme.example', room: 'A'}});
  assert.deepEqual(fromAcme.map(choice => [choice.id, choice.name, choice.room, choice.home, choice.unavailable]), [
    ['open-workspace:home', 'Lounge', null, true, false],
    ['open-workspace:https://globex.example/G', 'globex.example', 'G', false, true],
    ['open-workspace:https://rooms.example/TEAM', 'Team', 'TEAM', false, false],
  ]);
  // In the home (before or after its room is known), the home is not offered.
  for (const room of [null, 'LOUNGE']) {
    const fromHome = workspaceChoices({home, homeRoom: 'LOUNGE', entered, current: {kind: 'room', host: 'https://rooms.example', room}});
    assert.deepEqual(fromHome.map(choice => choice.name), ['Acme', 'globex.example', 'Team']);
  }
  // Without a home, the person's own desktop offers every room entered.
  assert.equal(workspaceChoices({entered, current: {kind: 'own'}}).length, 4);
  assert.deepEqual(workspaceActions(fromAcme), [
    {id: 'open-workspace:home', title: 'Open Lounge', label: 'Lounge', source: 'workspaces', subtitle: 'Workspace · rooms.example'},
    {id: 'open-workspace:https://globex.example/G', title: 'Open globex.example', label: 'globex.example', source: 'workspaces',
      subtitle: 'Workspace · globex.example · did not open last time'},
    {id: 'open-workspace:https://rooms.example/TEAM', title: 'Open Team', label: 'Team', source: 'workspaces', subtitle: 'Workspace · rooms.example'},
  ]);
});

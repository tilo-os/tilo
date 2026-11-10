import test from 'node:test';
import assert from 'node:assert/strict';
import {chooseDisplayName, chooseWorkspace, chosenDisplayName, chosenWorkspace, enteredWorkspaces, forgetGoneWorkspaces, heldWorkspaces, HostAdmissionError, joinGranted, hostDeviceOf, joinViaHost, keepJoinedHost, keptMembership, listedAt, listedWorkspaces, markUnavailable, nameEntered, ownDesktopOpen, pairViaHost, defaultDisplayName, randomDisplayName, redrawnDisplayName, rememberEntered, removedFrom, settleFailedWorkspace, workspaceActions, workspaceAt, workspaceChoices} from './workspaces.mjs';
import {CustodyError} from './device-custody.mjs';
import {HOST_CHANGED} from './peer.mjs';

const identity = key => ({kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(key), userPublicKey: key.toString(16).padStart(2, '0').repeat(32), sign: async () => new Uint8Array(64)});

const commons = {name: 'Commons', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
const KEY = 'tilo-host-memberships';

/** A device key of `profile` as `openDeviceCustody` gives it: `created` for
 * one no host has seen yet (a new tab's). */
const custodyOf = (calls, created) => async profile => {
  const custody = {profile, slot: created ? 1 : 0, created, closed: false, destroyed: false,
    close: async () => { custody.closed = true; }, destroy: async () => { custody.destroyed = true; custody.closed = true; }};
  calls.custodies.push(custody);
  return custody;
};

/** A door's refusal as the shared entry reads it (`HostRefusal`, peer.mjs):
 * the door's code (`DoorRefusalCode`) and the wait it asked for. */
const doorRefusal = ({code, error = code, retryAfter}) => Object.assign(new Error(error), {name: 'HostRefusal', code, retryAfter});

/** The door as the shared entry goes through it (`enterThroughDoor`): each
 * join through `door` asks it once for the person, and `answers` says what it
 * answers, one per ask (`null` lets them in), the last one ever after. */
const knocking = (calls, answers) => options => {
  calls.posted.push({url: new URL('join', options.door).href,
    body: {userPublicKey: options.identity?.userPublicKey ?? options.userPublicKey, displayName: options.displayName}});
  const answer = answers.length > 1 ? answers.shift() : answers[0];
  if (answer) throw doorRefusal(answer);
};

function harness({person = identity(0x22), membership = null, door = [null], connectFails = false, created = false} = {}) {
  const calls = {posted: [], joined: [], connected: [], paired: [], custodies: [], renewed: [], waited: [], progress: []};
  const store = new Map(membership ? [[KEY, JSON.stringify({'https://rooms.example': membership})]] : []);
  const knock = knocking(calls, door);
  return {
    calls,
    knock,
    options: {
      workspace: commons,
      identity: person,
      displayName: 'Brave Otter',
      memberships: {getItem: key => store.get(key) ?? null, setItem: (key, value) => store.set(key, value), removeItem: key => store.delete(key)},
      join: async options => {
        knock(options);
        calls.joined.push(options);
        options.onProgress?.({phase: 'admitted', room: 'WORKSPACE'});
        return {workspace: 'WORKSPACE', dispose: async () => {}};
      },
      connect: async options => { calls.connected.push(options); if (connectFails) throw new Error('refused: not a member'); return {workspace: options.workspace}; },
      pair: async options => { calls.paired.push(options); if (options.door) knock(options); return {workspace: options.workspace}; },
      openCustody: custodyOf(calls, created),
      renew: async (custody, profile) => {
        const renewed = {profile, slot: 0, created: true, renewed: calls.renewed.length + 1, closed: false, close: async () => { renewed.closed = true; }};
        calls.renewed.push({custody, profile, renewed});
        await custody.destroy();
        return renewed;
      },
      wait: async next => { calls.waited.push(next); },
      onProgress: event => calls.progress.push(event.phase),
    },
    store,
  };
}

test('a person new to the host is pre-admitted by it and joins with the pre-admission', async () => {
  const {calls, options, store} = harness();
  await joinViaHost(options);
  assert.equal(calls.posted[0].url, 'https://rooms.example/join');
  assert.deepEqual(calls.posted[0].body, {userPublicKey: '22'.repeat(32), displayName: 'Brave Otter'});
  assert.equal(calls.joined[0].door, commons.admission, 'through the workspace\'s door, by the shared entry');
  assert.equal(calls.joined[0].origin, 'https://rooms.example');
  assert.equal(calls.joined[0].identity.userPublicKey, '22'.repeat(32));
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '22'.repeat(32)});
});

/** The host turning away the device a join comes with (`peer.mjs`). */
const refusal = reason => Object.assign(new Error('refused'), {name: 'AdmissionError', code: 'refused', reason});

/** A join through the door that the workspace then refuses with `reasons`,
 * one per try, then admits. */
const refusing = (calls, reasons, knock = knocking(calls, [null])) => async joined => {
  knock(joined);
  calls.joined.push(joined);
  if (reasons.length) throw refusal(reasons.shift());
  joined.onProgress?.({phase: 'admitted', room: 'WORKSPACE'});
  return {workspace: 'WORKSPACE', dispose: async () => { await joined.custody?.close(); }};
};

test('a device the host knows under a certificate this browser lost joins as a new device of the same person', async () => {
  // The browser kept its device key but not the certificate the workspace
  // admitted it under (storage cleared, or a join the host admitted never
  // finished here): the workspace never admits that key under another.
  const membership = {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.connect = async connected => { calls.connected.push(connected); throw new Error('certification denied: host certificate is not a host of this user'); };
  options.join = refusing(calls, ['certificate conflict']);
  const peer = await joinViaHost(options);
  assert.equal(peer.workspace, 'WORKSPACE');
  assert.deepEqual(calls.posted.map(post => post.body.userPublicKey), ['11'.repeat(32), '11'.repeat(32)], 'the same person, a fresh invitation');
  assert.equal(calls.joined[0].custody, calls.custodies[0], 'first with the key the host knows');
  assert.equal(calls.renewed.length, 1);
  assert.equal(calls.renewed[0].custody, calls.custodies[0], 'that key is replaced');
  assert.ok(calls.custodies[0].destroyed);
  assert.equal(calls.renewed[0].profile, `tilo-identity:${'11'.repeat(32)}@rooms.example`);
  assert.equal(calls.joined[1].custody, calls.renewed[0].renewed, 'then as a new device');
  assert.deepEqual(calls.waited, [], 'at once');
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
  await peer.dispose();
  assert.ok(calls.renewed[0].renewed.closed, 'the new key is let go with its peer');
});

test('a host started anew that knows this device under another certificate is joined as a new device', async () => {
  // The host at the origin was set up again with the same workspace root: the
  // kept membership is the old host's, and the device key a stale one.
  const membership = {origin: 'https://rooms.example', room: 'OLD', userPublicKey: '11'.repeat(32)};
  const {calls, options} = harness({person: identity(0x11), membership});
  options.connect = async connected => {
    calls.connected.push(connected);
    throw Object.assign(new Error(HOST_CHANGED), {name: 'AdmissionError', code: 'host-changed'});
  };
  options.join = refusing(calls, ['governed certificate conflict']);
  assert.equal((await joinViaHost(options)).workspace, 'WORKSPACE');
  assert.equal(calls.posted.length, 2);
  assert.equal(calls.renewed[0].custody, calls.custodies[0]);
});

test('a browser whose person is gone joins as the person it holds now, on a new device when refused', async () => {
  // The membership names a person whose identity this browser no longer
  // holds: the door is asked for the one it does, and the key the host
  // turns away is replaced.
  const {calls, options, store} = harness({person: identity(0x33), membership: {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)}});
  options.join = refusing(calls, ['certificate conflict']);
  assert.equal((await joinViaHost(options)).workspace, 'WORKSPACE');
  assert.equal(calls.connected.length, 0);
  assert.equal(calls.joined[0].custody, undefined, 'first with the key joining opens');
  assert.equal(calls.custodies[0].profile, `tilo-identity:${'33'.repeat(32)}@rooms.example`, 'that key, opened to be replaced');
  assert.equal(calls.renewed[0].custody, calls.custodies[0]);
  assert.equal(calls.joined[1].custody, calls.renewed[0].renewed);
  assert.equal(keptMembership(commons, {getItem: key => store.get(key)}).userPublicKey, '33'.repeat(32));
});

test('a person the workspace bans is told they were removed, not asked to reload', async () => {
  const {calls, options} = harness({door: [{code: 'banned', error: 'you were banned from this workspace'}]});
  await assert.rejects(joinViaHost(options), error => {
    assert.equal(error.code, 'removed');
    assert.equal(error.message, removedFrom('Commons'));
    return true;
  });
  assert.equal(calls.posted.length, 1);
});

test('a refusal a new device does not end says it is reconnecting and asks again, later each time', async () => {
  const {calls, options} = harness();
  options.join = refusing(calls, ['the room stopped admitting', 'the room stopped admitting', 'join was not answered',
    'join was not answered', 'join was not answered', 'join was not answered']);
  assert.equal((await joinViaHost(options)).workspace, 'WORKSPACE');
  assert.equal(calls.posted.length, 7);
  assert.deepEqual(calls.waited.map(next => next.attempt), [0, 1, 2, 3, 4], 'the reconnect screen\'s backoff, try by try');
  assert.deepEqual(calls.waited.map(next => next.error.reason), ['the room stopped admitting', 'join was not answered',
    'join was not answered', 'join was not answered', 'join was not answered'], 'each with the refusal it follows');
  assert.ok(calls.waited.every(next => next.since === calls.waited[0].since && Number.isFinite(next.since)));
  assert.equal(calls.progress.at(-1), 'admitted');
  assert.equal(calls.renewed.length, 6, 'each try after the first as a new device');
  assert.equal(calls.renewed[1].custody.profile, `tilo-identity:${'22'.repeat(32)}@rooms.example`,
    'a later try replaces the key it reopens, never one let go');
});

test('a page that leaves while reconnecting stops asking', async () => {
  const {calls, options} = harness();
  const leaving = new AbortController();
  options.join = refusing(calls, ['join was not answered', 'join was not answered', 'join was not answered']);
  options.wait = async () => { leaving.abort(new DOMException('left', 'AbortError')); throw leaving.signal.reason; };
  await assert.rejects(joinViaHost({...options, signal: leaving.signal}), {name: 'AbortError'});
  assert.equal(calls.posted.length, 2);
});

test('the own desktop opens for a browser some host let in, and asks no door', async () => {
  // The door's join is the answer to who it lets in (admission.mjs): the
  // check reads only what this browser kept, and makes no request of its
  // own (it once read the door's `presence`, a route the host does not
  // serve, which answered 405).
  const requests = [];
  const fetched = globalThis.fetch;
  globalThis.fetch = async url => { requests.push(String(url)); return new Response(null, {status: 405}); };
  try {
    const none = {getItem: () => null};
    const kept = {getItem: () => JSON.stringify({'https://elsewhere.example': {origin: 'https://elsewhere.example', room: 'R', userPublicKey: '11'.repeat(32)}})};
    assert.equal(await ownDesktopOpen(null, {memberships: none}), true, 'no home: nothing to gate');
    assert.equal(await ownDesktopOpen({...commons, admission: undefined}, {memberships: none}), true);
    assert.equal(await ownDesktopOpen(commons, {memberships: none}), false, 'no host let it in yet: the door first');
    assert.equal(await ownDesktopOpen(commons, {memberships: kept}), true, 'a member keeps theirs');
    assert.equal(await ownDesktopOpen(commons, {memberships: {getItem: () => { throw new Error('no storage'); }}}), false);
  } finally {
    globalThis.fetch = fetched;
  }
  assert.deepEqual(requests, [], 'no request of its own');
});

test('a person joins at once under a random adjective-animal name when none is given', async () => {
  const {calls, options} = harness();
  delete options.displayName;
  await joinViaHost(options);
  assert.match(calls.posted[0].body.displayName, /^[A-Z][a-z]+ [A-Z][a-z]+$/);
  assert.equal(calls.joined[0].displayName, calls.posted[0].body.displayName);
});

test('a person who joined the host before reconnects without asking it', async () => {
  const {calls, options} = harness({person: identity(0x11), membership: {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)}});
  await joinViaHost(options);
  assert.equal(calls.posted.length, 0);
  assert.equal(calls.connected[0].identity.userPublicKey, '11'.repeat(32));
  assert.equal(calls.connected[0].workspace, 'WORKSPACE');
});

test('a person who was pruned is pre-admitted again as the same person', async () => {
  const {calls, options} = harness({person: identity(0x11), membership: {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)}, connectFails: true});
  await joinViaHost(options);
  assert.equal(calls.posted[0].body.userPublicKey, '11'.repeat(32));
});

test('a kept membership the host refuses once the session starts is joined again as the same person', async () => {
  // A host that started its workspace anew answers the old certificate only when
  // the session begins, after connecting resolved.
  const membership = {origin: 'https://rooms.example', room: 'OLD', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  let disposed = false;
  options.connect = async connected => {
    calls.connected.push(connected);
    return {workspace: connected.workspace, dispose: async () => { disposed = true; },
      whenReady: async () => { throw new Error('This workspace’s host no longer has you as a member.'); }};
  };
  const peer = await joinViaHost(options);
  assert.ok(disposed);
  assert.equal(calls.posted[0].body.userPublicKey, '11'.repeat(32));
  assert.equal(peer.workspace, 'WORKSPACE');
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)});
});

test('a person the host removed is told so, asks the door once and is not let in again', async () => {
  // Removed and pruned look alike to the returning browser: the host no
  // longer has the person. The door tells them apart, turning away the
  // address a removed person joined from.
  const membership = {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership,
    door: [{code: 'removed', error: 'you were removed from this workspace within the day'}]});
  options.connect = async connected => {
    calls.connected.push(connected);
    return {workspace: connected.workspace, dispose: async () => {},
      whenReady: async () => { throw new Error('device was removed from workspace membership'); }};
  };
  await assert.rejects(joinViaHost(options), {name: 'AdmissionError', code: 'removed', message: removedFrom('Commons')});
  assert.equal(calls.posted.length, 1);
  assert.equal(calls.joined.length, 0, 'the door did not let them in');
  assert.equal(keptMembership(commons, {getItem: key => store.get(key)}), null);
  assert.match(removedFrom('Commons'), /^You were removed from Commons\. .*new invitation/);
  assert.match(removedFrom(null), /^You were removed from this workspace\./);
});

test('a door that names the person removed tells them so, whatever this browser kept', async () => {
  const {calls, options} = harness({person: identity(0x33),
    door: [{code: 'removed', error: 'you were removed from this workspace within the day'}]});
  await assert.rejects(joinViaHost(options), {name: 'AdmissionError', code: 'removed', message: removedFrom('Commons')});
  assert.equal(calls.posted.length, 1);
  assert.equal(calls.connected.length, 0);
});

test('a door taking people by invitation tells a visitor without one so, and asks once', async () => {
  const {calls, options} = harness({person: identity(0x44),
    door: [{code: 'invitation-only', error: 'this workspace is joined by invitation'}]});
  await assert.rejects(joinViaHost(options), error => error instanceof HostAdmissionError && error.code === 'invitation-only');
  assert.equal(calls.posted.length, 1);
  assert.equal(calls.connected.length, 0);
});

test('a host that has not answered a kept membership yet keeps it and is not asked to admit again', async () => {
  const membership = {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.connect = async connected => ({workspace: connected.workspace, dispose: async () => {},
    whenReady: async () => { throw new DOMException('Room admission timed out', 'TimeoutError'); }});
  await assert.rejects(joinViaHost(options), {name: 'TimeoutError'});
  assert.equal(calls.posted.length, 0);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
});

test('a host that does not answer a kept membership at all (restarting) keeps it and is not asked to admit again', async () => {
  const membership = {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.connect = async () => { throw new Error('fetch failed'); };
  await assert.rejects(joinViaHost(options), {message: 'fetch failed'});
  assert.equal(calls.posted.length, 0);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
});

test('a second tab is a new device of the person, admitted through the door once', async () => {
  // Its device key is one no host has seen: the workspace admits each device
  // once, so it asks the door for the same person rather than fail to connect.
  const membership = {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership, created: true});
  const peer = await joinViaHost(options);
  assert.equal(calls.connected.length, 0, 'no connection the room would refuse');
  assert.deepEqual(calls.posted.map(post => post.body.userPublicKey), ['11'.repeat(32)]);
  assert.equal(calls.joined[0].custody, calls.custodies[0], 'joined with the new tab\u2019s own key');
  assert.equal(calls.custodies[0].profile, `tilo-identity:${'11'.repeat(32)}@rooms.example`);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), {...membership, room: 'WORKSPACE'});
  // The tab's key is let go with its peer.
  assert.equal(calls.custodies[0].closed, false);
  await peer.dispose();
  assert.equal(calls.custodies[0].closed, true);
});

test('a second tab of a paired device pairs as a new device, approved on one of the person\u2019s devices', async () => {
  const person = '33'.repeat(32);
  const membership = {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: person, paired: true};
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
  const membership = {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
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
  assert.equal(peer.workspace, 'WORKSPACE');
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}),
    {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)});
});

test('a membership kept for another person is not reused for this one', async () => {
  const {calls, options} = harness({person: identity(0x33), membership: {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '11'.repeat(32)}});
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

test('a door not reached, and one that refuses for a reason of its own, are told', async () => {
  const down = harness({door: [{code: 'unanswered', error: 'The workspace\'s door was not reached'}]});
  await assert.rejects(joinViaHost(down.options), error => error instanceof HostAdmissionError && error.code === 'unavailable');
  const other = harness({door: [{code: 'refused', error: 'the door answered 500'}]});
  await assert.rejects(joinViaHost(other.options), error => error instanceof HostAdmissionError && error.code === 'refused');
});

test('a door with no seat is waited out as long as it asked, then the same person joins', async () => {
  const busy = {code: 'full', error: 'the workspace is full', retryAfter: 30};
  const {calls, options} = harness({door: [busy, busy, null]});
  const peer = await joinViaHost(options);
  assert.equal(peer.workspace, 'WORKSPACE');
  assert.equal(calls.posted.length, 3, 'asked again after each wait');
  assert.deepEqual(calls.posted.map(post => post.body.userPublicKey), Array(3).fill('22'.repeat(32)), 'the same person every time');
  assert.deepEqual(calls.waited.map(wait => [wait.attempt, wait.error.code, wait.retryAfter]), [[0, 'full', 30], [1, 'full', 30]]);
  assert.equal(calls.renewed.length, 0, 'the seat was the only thing missing, not the person');
  assert.equal(calls.joined.length, 1, 'one join, once there was a seat');
  assert.equal(calls.joined[0].origin, 'https://rooms.example', 'a door that names no origin is its workspace’s host');
});

test('the door is asked for the browser’s own person, never for a visitor’s device key', async () => {
  const {calls, options} = harness();
  await joinViaHost(options);
  assert.deepEqual(calls.posted[0].body, {userPublicKey: options.identity.userPublicKey, displayName: 'Brave Otter'});
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

// Two keys whose first bytes draw the same name, as two visitors' did.
const drawsAlike = rest => `0d000d${rest}`.padEnd(64, '0');
const early = drawsAlike('00' + '01'.repeat(29));
const late = drawsAlike('00' + 'f3'.repeat(29));

test('a name someone in the workspace already goes by is drawn again from the key', () => {
  const name = defaultDisplayName(early);
  assert.equal(defaultDisplayName(late), name, 'the two keys draw alike');
  const other = defaultDisplayName(late, Math.random, [name]);
  assert.notEqual(other, name);
  assert.match(other, /^[A-Z][a-z]+ [A-Z][a-z]+$/);
  assert.equal(defaultDisplayName(late, Math.random, [name]), other, 'the same room draws the same name again');
  assert.equal(defaultDisplayName(late, Math.random, [name.toUpperCase()]), other, 'names collide whatever their case');
});

test('of two people who drew one name, the one with the greater key takes another', () => {
  const name = defaultDisplayName(early);
  const lines = [{user: early, label: name}, {user: late, label: name}];
  assert.equal(redrawnDisplayName(early, name, lines), null, 'the lesser key keeps it');
  const redrawn = redrawnDisplayName(late, name, lines);
  assert.ok(redrawn && redrawn !== name);
  // Once they differ, neither draws again; nor does a person alone, nor the
  // same person's other device.
  assert.equal(redrawnDisplayName(late, redrawn, [{user: early, label: name}, {user: late, label: redrawn}]), null);
  assert.equal(redrawnDisplayName(late, name, [{user: late, label: name}, {user: late, label: name}]), null);
});

test('when every name a key draws is taken, the first is numbered', () => {
  const key = '0a'.repeat(32);
  const first = defaultDisplayName(key);
  assert.equal(defaultDisplayName(key, Math.random, [first]), `${first} 2`);
  assert.equal(defaultDisplayName(key, Math.random, [first, `${first} 2`]), `${first} 3`);
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

test('a paired device is kept as its person and reconnects as them, never as a person of its own', async () => {
  const {calls, options, store, knock} = harness();
  const person = '33'.repeat(32);
  const codes = [];
  await pairViaHost({origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: person, invite: '0f'.repeat(16), workspace: commons,
    displayName: 'Ada', memberships: options.memberships, onPairingCode: code => codes.push(code),
    pair: async pairing => { calls.connected.push(pairing); knock(pairing); return {workspace: 'WORKSPACE'}; }});
  const paired = calls.connected[0];
  assert.equal(paired.userPublicKey, person);
  assert.equal(paired.invite, '0f'.repeat(16), 'the request answers the link\u2019s invite');
  assert.equal(paired.identity, undefined, 'the root stays on the device that approves');
  assert.equal(paired.profile, `tilo-identity:${person}@rooms.example`);
  assert.equal(typeof paired.onPairingCode, 'function');
  assert.deepEqual(calls.posted, [{url: 'https://rooms.example/join', body: {userPublicKey: person, displayName: 'Ada'}}],
    'the new device joins the person\u2019s workspace through the host\u2019s door');
  calls.posted.length = 0;
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}),
    {origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: person, paired: true});

  await joinViaHost({...options, identity: undefined});
  assert.equal(calls.posted.length, 0, 'the host is not asked to admit another person');
  assert.equal(calls.connected[1].userPublicKey, person);
  assert.equal(calls.connected[1].profile, paired.profile);
  assert.equal(calls.connected[1].workspace, 'WORKSPACE');
});

test('a pairing link that names no person is refused before anything is asked', async () => {
  const {calls, options} = harness();
  await assert.rejects(pairViaHost({origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: 'nobody', pair: options.connect,
    memberships: options.memberships}), {code: 'invalid'});
  await assert.rejects(pairViaHost({origin: 'https://rooms.example', room: 'WORKSPACE', userPublicKey: '33'.repeat(32), invite: 'short',
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

/** The host that held the workspaces these tests remember. */
const HOST = 'ab'.repeat(32);

const memoryStore = () => {
  const map = new Map();
  return {getItem: key => map.get(key) ?? null, setItem: (key, value) => map.set(key, String(value)), removeItem: key => map.delete(key)};
};

/** The list as storage holds it. */
const seed = (store, list) => store.setItem('tilo-entered-workspaces', JSON.stringify(list));
const kept = (origin, room, extra = {}) => ({origin, room, hostDevice: HOST, ...extra});

/** The runtime's rules of the list (tilo-session-host's `entered`, tested
 * there), answering what each was asked with `answer`. */
const rules = answer => {
  const asked = [];
  const runtime = {};
  for (const name of ['rememberEntered', 'nameEntered', 'markEnteredUnavailable', 'forgetGoneWorkspaces', 'settleFailedWorkspace']) {
    runtime[name] = (...args) => { asked.push([name, ...args]); return JSON.stringify(answer(name, ...args)); };
  }
  return {runtime, asked};
};

test('the remembered workspaces are read as kept, and damaged or roomless entries are never offered', () => {
  const store = memoryStore();
  seed(store, [kept('https://globex.example', 'H', {name: 'Hank'}), kept('https://acme.example/', 'A', {unavailable: true})]);
  assert.deepEqual(enteredWorkspaces(store), [
    {origin: 'https://globex.example', room: 'H', hostDevice: HOST, name: 'Hank', unavailable: false},
    {origin: 'https://acme.example', room: 'A', hostDevice: HOST, name: null, unavailable: true},
  ]);
  // One that names no host that held it could never be told gone: not offered.
  store.setItem('tilo-entered-workspaces', JSON.stringify([{origin: 'not a url', room: 'X', hostDevice: HOST},
    {origin: 'https://ok.example', room: 'OK', hostDevice: HOST}, {origin: 'https://old.example', room: 'O'}, 7]));
  assert.deepEqual(enteredWorkspaces(store), [{origin: 'https://ok.example', room: 'OK', hostDevice: HOST, name: null, unavailable: false}]);
  store.setItem('tilo-entered-workspaces', '{broken');
  assert.deepEqual(enteredWorkspaces(store), []);
});

test('each change is the runtime\'s rules\', written back with no bare entry reopening what it forgot', () => {
  const store = memoryStore();
  seed(store, [kept('https://acme.example', 'A')]);
  chooseWorkspace({origin: 'https://acme.example', room: 'A'}, store);
  const after = [kept('https://acme.example', 'B', {name: 'Bee'})];
  const {runtime, asked} = rules(() => ({entered: after, unchoose: [{origin: 'https://acme.example', room: 'A'}]}));
  rememberEntered(runtime, {hostDevice: HOST, origin: 'https://acme.example/', room: 'B', name: 'Bee'}, store);
  assert.deepEqual(asked[0], ['rememberEntered', JSON.stringify([kept('https://acme.example', 'A')]),
    JSON.stringify({origin: 'https://acme.example', room: 'B', hostDevice: HOST, name: 'Bee'})], 'asked with the origin as a link names it');
  assert.deepEqual(JSON.parse(store.getItem('tilo-entered-workspaces')), after);
  assert.equal(chosenWorkspace(store), null, 'the one it forgot is not reopened');
  // What is no entry is not asked about.
  rememberEntered(runtime, {hostDevice: HOST, origin: 'not a url', room: 'X'}, store);
  rememberEntered(runtime, {hostDevice: undefined, origin: 'https://acme.example', room: 'X'}, store);
  assert.equal(asked.length, 1);
});

test('a bare entry reopens the chosen workspace until it does not open, and choosing the home forgets it', () => {
  const store = memoryStore();
  seed(store, [kept('https://acme.example', 'A', {name: 'Acme'})]);
  // A choice this browser never entered is not reopened.
  chooseWorkspace({origin: 'https://other.example', room: 'O'}, store);
  assert.equal(chosenWorkspace(store), null);
  chooseWorkspace({origin: 'https://acme.example/', room: 'A'}, store);
  assert.deepEqual(chosenWorkspace(store), {origin: 'https://acme.example', room: 'A'});
  chooseWorkspace(null, store);
  assert.equal(chosenWorkspace(store), null);
  chooseWorkspace({origin: 'https://acme.example', room: 'A'}, store);
  const {runtime} = rules(() => ({entered: [kept('https://acme.example', 'A', {name: 'Acme', unavailable: true})],
    unchoose: [{origin: 'https://acme.example', room: 'A'}]}));
  markUnavailable(runtime, {origin: 'https://acme.example', room: 'A'}, store);
  assert.equal(chosenWorkspace(store), null);
  assert.equal(enteredWorkspaces(store)[0].unavailable, true);
});

test('a workspace that did not open is settled by what its origin answers now, asked once', async () => {
  const store = memoryStore();
  seed(store, [kept('https://lounge.example', 'OLD'), kept('https://away.example', 'AWAY')]);
  const asked = [];
  const hostInfo = async origin => {
    asked.push(origin);
    if (origin === 'https://lounge.example') return {device_public_key: Array(32).fill(0xcd), user_public_key: Array(32).fill(1)};
    throw new TypeError('Failed to fetch');
  };
  const settled = {'https://lounge.example': 'gone', 'https://away.example': 'unavailable'};
  const {runtime, asked: rulesAsked} = rules((name, list, place) => ({entered: [], unchoose: [], settled: settled[JSON.parse(place).origin]}));
  assert.equal(await settleFailedWorkspace(runtime, {origin: 'https://lounge.example/', room: 'OLD'}, {hostInfo, store}), 'gone');
  assert.equal(rulesAsked[0][3], 'cd'.repeat(32), 'the host answering there now');
  seed(store, [kept('https://away.example', 'AWAY')]);
  assert.equal(await settleFailedWorkspace(runtime, {origin: 'https://away.example', room: 'AWAY'}, {hostInfo, store}), 'unavailable');
  assert.equal(rulesAsked[1][3], undefined, 'no answer is no host');
  assert.deepEqual(asked, ['https://lounge.example', 'https://away.example']);
  assert.equal(await settleFailedWorkspace(runtime, {origin: 'https://nowhere.example', room: 'X'}, {hostInfo, store}), null);
  assert.equal(rulesAsked.length, 2, 'a workspace this browser does not remember is not asked about');
});

test('a host\'s certificate names its device key as the list keeps it', () => {
  assert.equal(hostDeviceOf({device_public_key: Array(32).fill(0xcd)}), 'cd'.repeat(32));
  assert.equal(hostDeviceOf({device_public_key: 'not hex'}), null);
  assert.equal(hostDeviceOf(undefined), null);
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
  // Two on one host: their workspaces too.
  const one = [choice('https://a.example', '5e8d4510-aaaa', 'Lounge'), choice('https://a.example', '84c928ed-bbbb', 'Lounge')];
  assert.deepEqual(labels(one, null), ['Lounge · a.example · 5e8d4510', 'Lounge · a.example · 84c928ed']);
  assert.equal(workspaceActions(one)[0].title, 'Open Lounge · a.example · 5e8d4510');
});

test('the person switches between the home and the workspaces they entered, never to the one in front', () => {
  const home = {name: 'Lounge', host: {origin: 'https://rooms.example'}, admission: 'https://rooms.example/'};
  const entered = [
    {origin: 'https://acme.example', room: 'A', name: 'Acme', unavailable: false},
    {origin: 'https://globex.example', room: 'G', name: null, unavailable: true},
    // The workspace the home's door admitted this browser to is the home itself.
    {origin: 'https://rooms.example', room: 'LOUNGE', name: 'Lounge', unavailable: false},
    // Another workspace on the home's host is a workspace of its own.
    {origin: 'https://rooms.example', room: 'TEAM', name: 'Team', unavailable: false},
  ];
  const fromAcme = workspaceChoices({home, homeWorkspace: 'LOUNGE', entered, current: {kind: 'room', host: 'https://acme.example', room: 'A'}});
  assert.deepEqual(fromAcme.map(choice => [choice.id, choice.name, choice.room, choice.home, choice.unavailable]), [
    ['open-workspace:home', 'Lounge', null, true, false],
    ['open-workspace:https://globex.example/G', 'globex.example', 'G', false, true],
    ['open-workspace:https://rooms.example/TEAM', 'Team', 'TEAM', false, false],
  ]);
  // In the home (before or after its workspace is known), the home is not offered.
  for (const room of [null, 'LOUNGE']) {
    const fromHome = workspaceChoices({home, homeWorkspace: 'LOUNGE', entered, current: {kind: 'room', host: 'https://rooms.example', room}});
    assert.deepEqual(fromHome.map(choice => choice.name), ['Acme', 'globex.example', 'Team']);
  }
  // Without a home, the person's own desktop offers every workspace entered.
  assert.equal(workspaceChoices({entered, current: {kind: 'own'}}).length, 4);
  assert.deepEqual(workspaceActions(fromAcme), [
    {id: 'open-workspace:home', title: 'Open Lounge', label: 'Lounge', source: 'workspaces', subtitle: 'Workspace · rooms.example'},
    {id: 'open-workspace:https://globex.example/G', title: 'Open globex.example', label: 'globex.example', source: 'workspaces',
      subtitle: 'Workspace · globex.example · did not open last time'},
    {id: 'open-workspace:https://rooms.example/TEAM', title: 'Open Team', label: 'Team', source: 'workspaces', subtitle: 'Workspace · rooms.example'},
  ]);
});

test('the directory lists the workspaces this page holds by the host actions that open them, the one in front included', () => {
  const held = heldWorkspaces({
    entered: [
      {origin: 'https://rooms.example', room: 'WORKSPACE', name: 'Commons'},
      // The same workspace remembered twice is offered once.
      {origin: 'https://rooms.example/', room: 'WORKSPACE', name: 'Commons'},
      {origin: 'https://garden.example', room: 'GARDEN', name: null},
    ],
    current: {kind: 'room', host: 'https://rooms.example/join', room: 'HERE', name: 'Here'},
  });
  assert.deepEqual(held, [
    {workspace: 'HERE', key: 'open-workspace:https://rooms.example/HERE', name: 'Here'},
    {workspace: 'WORKSPACE', key: 'open-workspace:https://rooms.example/WORKSPACE', name: 'Commons'},
    {workspace: 'GARDEN', key: 'open-workspace:https://garden.example/GARDEN', name: 'garden.example'},
  ]);
  // A page in no workspace holds only what it remembered.
  assert.deepEqual(heldWorkspaces({entered: [], current: {kind: 'own'}}), []);
});

test('a join the directory was granted is made with its invitation and token, then opened under its name', async () => {
  const joined = [];
  const grant = await joinGranted(
    {origin: 'https://rooms.example/join/garden', invitation: 'INV', admissionToken: 'TOKEN', workspace: 'WORKSPACE', name: 'Gardeners'},
    {join: async options => { joined.push(options); return {workspace: 'WORKSPACE'}; }, displayName: 'Brave Otter'},
  );
  assert.deepEqual(joined, [{origin: 'https://rooms.example/join/garden', invitation: 'INV', admissionToken: 'TOKEN', displayName: 'Brave Otter'}]);
  assert.deepEqual(grant, {peer: {workspace: 'WORKSPACE'}, origin: 'https://rooms.example', room: 'WORKSPACE', name: 'Gardeners'});
});

test('a grant that carries no invitation joins nothing', async () => {
  await assert.rejects(() => joinGranted({origin: 'https://rooms.example', invitation: ''}, {join: async () => {}}), TypeError);
  await assert.rejects(() => joinGranted({origin: '', invitation: 'INV'}, {join: async () => {}}), TypeError);
});

test('an invitation to another workspace on the home\'s host leaves the home its own name and choice', () => {
  const store = memoryStore();
  const home = {name: 'Lounge', host: {origin: 'https://lounge.example'}};
  const person = 'cd'.repeat(32);
  // The home's door let this browser into its workspace.
  keepJoinedHost({origin: 'https://lounge.example', room: 'lounge', userPublicKey: person}, store);
  // Then the person accepts an invitation to a customer's workspace on the same host.
  keepJoinedHost({origin: 'https://lounge.example/', room: 'customer', userPublicKey: person}, store);
  assert.equal(keptMembership(home, store).room, 'lounge');
  const scope = room => ({kind: 'room', host: 'https://lounge.example', room});
  assert.equal(listedAt([home], scope('customer'), null, store), null, 'the customer\'s workspace is not named Lounge');
  assert.equal(listedAt([home], scope('lounge'), null, store), home);
  const choices = workspaceChoices({home, homeWorkspace: keptMembership(home, store).room,
    entered: [{origin: 'https://lounge.example', room: 'customer', name: 'Applicant: Jane Doe', unavailable: false}],
    current: scope('customer')});
  assert.deepEqual(choices.map(choice => [choice.name, choice.room]), [['Lounge', null]], 'the home stays one to go back to');
  // Another person on this browser joins as themselves.
  keepJoinedHost({origin: 'https://lounge.example', room: 'customer', userPublicKey: 'ef'.repeat(32)}, store);
  assert.equal(keptMembership(home, store).room, 'customer');
});

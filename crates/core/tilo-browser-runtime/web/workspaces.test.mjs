import test from 'node:test';
import assert from 'node:assert/strict';
import {chooseDisplayName, chooseWorkspace, chosenDisplayName, chosenWorkspace, confirmWaitList, entryWorkspace, enteredWorkspaces, forgetDeletedWorkspace, freeName, heldWorkspaces, HostAdmissionError, hostDeviceOf, joinGranted, joinViaHost, keepJoinedHost, keptMembership, knownWorkspaces, leaveOnWaitList, leaveWaitList, listedAt, listedWorkspaces, markUnavailable, nameEntered, ownDesktopOpen, pairViaHost, rememberEntered, removedFrom, settleFailedWorkspace, waitListing, waitListStatus, workspaceActions, workspaceAt, workspaceChoices} from './workspaces.mjs';
import {DeviceKeyError} from './device-key.mjs';
import {HOST_CHANGED} from './peer.mjs';
import {PAIRING_SAME_PERSON} from './pairing.mjs';
import {publicListings, unmarkOwnPairing} from './workspaces.mjs';

/** A pairing link's ticket, as a page shows it. */
const TICKET = `tilo1${'ab'.repeat(40)}`;

const identity = key => ({kind: 'webcrypto-ed25519', publicKey: new Uint8Array(32).fill(key), userPublicKey: key.toString(16).padStart(2, '0').repeat(32), sign: async () => new Uint8Array(64)});

const commons = {name: 'Commons', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
const KEY = 'tilo-host-memberships';

const reply = ({status = 200, headers = {}, body = {}}) => ({
  ok: status === 200, status, headers: new Map(Object.entries(headers)), json: async () => body,
  text: async () => (typeof body === 'string' ? body : JSON.stringify(body)),
});

/** A device key of `profile` as `openDeviceKey` gives it: `created` for
 * one no host has seen yet (a new tab's). */
const deviceKeyOf = (calls, created) => async profile => {
  const deviceKey = {profile, slot: created ? 1 : 0, created, closed: false, destroyed: false,
    close: async () => { deviceKey.closed = true; }, destroy: async () => { deviceKey.destroyed = true; deviceKey.closed = true; }};
  calls.deviceKeys.push(deviceKey);
  return deviceKey;
};

/** An entry's refusal as the shared entry reads it (`DeviceRefusal`, peer.mjs):
 * the entry's code (`EntryRefusalCode`) and the wait it asked for. */
const entryRefusal = ({code, error = code, retryAfter}) => Object.assign(new Error(error), {name: 'DeviceRefusal', code, retryAfter});

/** The entry as the shared entry goes through it (`entryRequest`, on the session): each
 * join through `entry` asks it once for the person, and `answers` says what it
 * answers, one per ask (`null` lets them in), the last one ever after. */
const knocking = (calls, answers) => options => {
  calls.posted.push({url: new URL('join', options.entry).href,
    body: {userPublicKey: options.identity?.userPublicKey ?? options.userPublicKey, displayName: options.displayName}});
  const answer = answers.length > 1 ? answers.shift() : answers[0];
  if (answer) throw entryRefusal(answer);
};

function harness({person = identity(0x22), membership = null, entry = [null], connectFails = false, created = false} = {}) {
  const calls = {posted: [], joined: [], connected: [], paired: [], deviceKeys: [], renewed: [], waited: [], progress: []};
  const store = new Map(membership ? [[KEY, JSON.stringify({'https://workspaces.example': membership})]] : []);
  const knock = knocking(calls, entry);
  return {
    calls,
    knock,
    options: {
      workspace: commons,
      identity: person,
      displayName: 'Ada Lovelace',
      memberships: {getItem: key => store.get(key) ?? null, setItem: (key, value) => store.set(key, value), removeItem: key => store.delete(key)},
      join: async options => {
        knock(options);
        calls.joined.push(options);
        options.onProgress?.({phase: 'admitted', workspace: 'WORKSPACE'});
        return {workspace: 'WORKSPACE', dispose: async () => {}};
      },
      connect: async options => { calls.connected.push(options); if (connectFails) throw new Error('refused: not a member'); return {workspace: options.workspace}; },
      pair: async options => { calls.paired.push(options); if (options.entry) knock(options); return {workspace: options.workspace}; },
      openDeviceKey: deviceKeyOf(calls, created),
      renew: async (deviceKey, profile) => {
        const renewed = {profile, slot: 0, created: true, renewed: calls.renewed.length + 1, closed: false, close: async () => { renewed.closed = true; }};
        calls.renewed.push({deviceKey, profile, renewed});
        await deviceKey.destroy();
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
  assert.equal(calls.posted[0].url, 'https://workspaces.example/join');
  assert.deepEqual(calls.posted[0].body, {userPublicKey: '22'.repeat(32), displayName: 'Ada Lovelace'});
  assert.equal(calls.joined[0].entry, commons.admission, 'through the workspace\'s entry, by the shared entry');
  assert.equal(calls.joined[0].origin, 'https://workspaces.example');
  assert.equal(calls.joined[0].identity.userPublicKey, '22'.repeat(32));
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '22'.repeat(32)});
});

/** The host turning away the device a join comes with (`peer.mjs`). */
const refusal = reason => Object.assign(new Error('refused'), {name: 'AdmissionError', code: 'refused', reason});

/** A join through the entry that the workspace then refuses with `reasons`,
 * one per try, then admits. */
const refusing = (calls, reasons, knock = knocking(calls, [null])) => async joined => {
  knock(joined);
  calls.joined.push(joined);
  if (reasons.length) throw refusal(reasons.shift());
  joined.onProgress?.({phase: 'admitted', workspace: 'WORKSPACE'});
  return {workspace: 'WORKSPACE', dispose: async () => { await joined.deviceKey?.close(); }};
};

test('a device the host knows under a certificate this browser lost joins as a new device of the same person', async () => {
  // The browser kept its device key but not the certificate the workspace
  // admitted it under (storage cleared, or a join the host admitted never
  // finished here): the workspace never admits that key under another.
  const membership = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.connect = async connected => { calls.connected.push(connected); throw new Error('certification denied: host certificate is not a host of this user'); };
  options.join = refusing(calls, ['certificate conflict']);
  const peer = await joinViaHost(options);
  assert.equal(peer.workspace, 'WORKSPACE');
  assert.deepEqual(calls.posted.map(post => post.body.userPublicKey), ['11'.repeat(32), '11'.repeat(32)], 'the same person, a fresh invitation');
  assert.equal(calls.joined[0].deviceKey, calls.deviceKeys[0], 'first with the key the host knows');
  assert.equal(calls.renewed.length, 1);
  assert.equal(calls.renewed[0].deviceKey, calls.deviceKeys[0], 'that key is replaced');
  assert.ok(calls.deviceKeys[0].destroyed);
  assert.equal(calls.renewed[0].profile, `tilo-identity:${'11'.repeat(32)}@workspaces.example`);
  assert.equal(calls.joined[1].deviceKey, calls.renewed[0].renewed, 'then as a new device');
  assert.deepEqual(calls.waited, [], 'at once');
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
  await peer.dispose();
  assert.ok(calls.renewed[0].renewed.closed, 'the new key is let go with its peer');
});

test('a host started anew that knows this device under another certificate is joined as a new device', async () => {
  // The host at the origin was set up again with the same workspace root: the
  // kept membership is the old host's, and the device key a stale one.
  const membership = {origin: 'https://workspaces.example', workspace: 'OLD', userPublicKey: '11'.repeat(32)};
  const {calls, options} = harness({person: identity(0x11), membership});
  options.connect = async connected => {
    calls.connected.push(connected);
    throw Object.assign(new Error(HOST_CHANGED), {name: 'AdmissionError', code: 'host-changed'});
  };
  options.join = refusing(calls, ['governed certificate conflict']);
  assert.equal((await joinViaHost(options)).workspace, 'WORKSPACE');
  assert.equal(calls.posted.length, 2);
  assert.equal(calls.renewed[0].deviceKey, calls.deviceKeys[0]);
});

test('a browser whose person is gone joins as the person it holds now, on a new device when refused', async () => {
  // The membership names a person whose identity this browser no longer
  // holds: the entry is asked for the one it does, and the key the host
  // turns away is replaced.
  const {calls, options, store} = harness({person: identity(0x33), membership: {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)}});
  options.join = refusing(calls, ['certificate conflict']);
  assert.equal((await joinViaHost(options)).workspace, 'WORKSPACE');
  assert.equal(calls.connected.length, 0);
  assert.equal(calls.joined[0].deviceKey, undefined, 'first with the key joining opens');
  assert.equal(calls.deviceKeys[0].profile, `tilo-identity:${'33'.repeat(32)}@workspaces.example`, 'that key, opened to be replaced');
  assert.equal(calls.renewed[0].deviceKey, calls.deviceKeys[0]);
  assert.equal(calls.joined[1].deviceKey, calls.renewed[0].renewed);
  assert.equal(keptMembership(commons, {getItem: key => store.get(key)}).userPublicKey, '33'.repeat(32));
});

test('a person the workspace bans is told they were removed, not asked to reload', async () => {
  const {calls, options} = harness({entry: [{code: 'banned', error: 'you were banned from this workspace'}]});
  await assert.rejects(joinViaHost(options), error => {
    assert.equal(error.code, 'removed');
    assert.equal(error.message, removedFrom('Commons'));
    return true;
  });
  assert.equal(calls.posted.length, 1);
});

test('a refusal a new device does not end says it is reconnecting and asks again, later each time', async () => {
  const {calls, options} = harness();
  options.join = refusing(calls, ['the workspace stopped admitting', 'the workspace stopped admitting', 'join was not answered',
    'join was not answered', 'join was not answered', 'join was not answered']);
  assert.equal((await joinViaHost(options)).workspace, 'WORKSPACE');
  assert.equal(calls.posted.length, 7);
  assert.deepEqual(calls.waited.map(next => next.attempt), [0, 1, 2, 3, 4], 'the reconnect screen\'s backoff, try by try');
  assert.deepEqual(calls.waited.map(next => next.error.reason), ['the workspace stopped admitting', 'join was not answered',
    'join was not answered', 'join was not answered', 'join was not answered'], 'each with the refusal it follows');
  assert.ok(calls.waited.every(next => next.since === calls.waited[0].since && Number.isFinite(next.since)));
  assert.equal(calls.progress.at(-1), 'admitted');
  assert.equal(calls.renewed.length, 6, 'each try after the first as a new device');
  assert.equal(calls.renewed[1].deviceKey.profile, `tilo-identity:${'22'.repeat(32)}@workspaces.example`,
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

test('the own desktop opens for a browser some host let in, and asks no entry', async () => {
  // The entry's join is the answer to who it lets in (admission.mjs): the
  // check reads only what this browser kept, and makes no request of its
  // own (it once read the entry's `presence`, a route the host does not
  // serve, which answered 405).
  const requests = [];
  const fetched = globalThis.fetch;
  globalThis.fetch = async url => { requests.push(String(url)); return new Response(null, {status: 405}); };
  try {
    const none = {getItem: () => null};
    const kept = {getItem: () => JSON.stringify({'https://elsewhere.example': {origin: 'https://elsewhere.example', workspace: 'R', userPublicKey: '11'.repeat(32)}})};
    assert.equal(await ownDesktopOpen(null, {memberships: none}), true, 'no home: nothing to check');
    assert.equal(await ownDesktopOpen({...commons, admission: undefined}, {memberships: none}), true);
    assert.equal(await ownDesktopOpen(commons, {memberships: none}), false, 'no host let it in yet: the entry first');
    assert.equal(await ownDesktopOpen(commons, {memberships: kept}), true, 'a member keeps theirs');
    assert.equal(await ownDesktopOpen(commons, {memberships: {getItem: () => { throw new Error('no storage'); }}}), false);
  } finally {
    globalThis.fetch = fetched;
  }
  assert.deepEqual(requests, [], 'no request of its own');
});

test('a person who gave no name joins at once under none, never a made-up one', async () => {
  const {calls, options} = harness();
  delete options.displayName;
  await joinViaHost(options);
  assert.equal(calls.posted[0].body.displayName, null);
  assert.equal(calls.joined[0].displayName, null);
});

test('a person who joined the host before reconnects without asking it', async () => {
  const {calls, options} = harness({person: identity(0x11), membership: {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)}});
  await joinViaHost(options);
  assert.equal(calls.posted.length, 0);
  assert.equal(calls.connected[0].identity.userPublicKey, '11'.repeat(32));
  assert.equal(calls.connected[0].workspace, 'WORKSPACE');
});

test('another workspace of the host is entered at its own entry, not the membership’s workspace', async () => {
  // The person's browser was let into the host's home before; the host also
  // keeps a workspace of their own there, which they never entered in this
  // browser. It is not the membership's workspace: it is asked at its own
  // entry, and the membership stays the one its entry first let them into.
  const membership = {origin: 'https://workspaces.example', workspace: 'LOUNGE', userPublicKey: '11'.repeat(32)};
  const mine = {...commons, name: 'My workspace', workspace: 'MINE', admission: 'https://workspaces.example/directory/MINE/'};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  const peer = await joinViaHost({...options, workspace: mine});
  assert.equal(peer.workspace, 'WORKSPACE');
  assert.deepEqual(calls.connected, [], 'the membership is for another workspace: it is not reconnected to');
  assert.equal(calls.joined[0].entry, mine.admission, 'this workspace’s own entry');
  assert.equal(calls.joined[0].origin, 'https://workspaces.example');
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
});

test('a person who was pruned is pre-admitted again as the same person', async () => {
  const {calls, options} = harness({person: identity(0x11), membership: {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)}, connectFails: true});
  await joinViaHost(options);
  assert.equal(calls.posted[0].body.userPublicKey, '11'.repeat(32));
});

test('a kept membership the host refuses once the session starts is joined again as the same person', async () => {
  // A host that started its workspace anew answers the old certificate only when
  // the session begins, after connecting resolved.
  const membership = {origin: 'https://workspaces.example', workspace: 'OLD', userPublicKey: '11'.repeat(32)};
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
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)});
});

test('a person the host removed is told so, asks the entry once and is not let in again', async () => {
  // Removed and pruned look alike to the returning browser: the host no
  // longer has the person. The entry tells them apart, turning away the
  // address a removed person joined from.
  const membership = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership,
    entry: [{code: 'removed', error: 'you were removed from this workspace within the day'}]});
  options.connect = async connected => {
    calls.connected.push(connected);
    return {workspace: connected.workspace, dispose: async () => {},
      whenReady: async () => { throw new Error('device was removed from workspace membership'); }};
  };
  await assert.rejects(joinViaHost(options), {name: 'AdmissionError', code: 'removed', message: removedFrom('Commons')});
  assert.equal(calls.posted.length, 1);
  assert.equal(calls.joined.length, 0, 'the entry did not let them in');
  assert.equal(keptMembership(commons, {getItem: key => store.get(key)}), null);
  assert.match(removedFrom('Commons'), /^You were removed from Commons\. .*new invitation/);
  assert.match(removedFrom(null), /^You were removed from this workspace\./);
});

test('an entry that names the person removed tells them so, whatever this browser kept', async () => {
  const {calls, options} = harness({person: identity(0x33),
    entry: [{code: 'removed', error: 'you were removed from this workspace within the day'}]});
  await assert.rejects(joinViaHost(options), {name: 'AdmissionError', code: 'removed', message: removedFrom('Commons')});
  assert.equal(calls.posted.length, 1);
  assert.equal(calls.connected.length, 0);
});

test('an entry taking people by invitation tells a visitor without one so, and asks once', async () => {
  const {calls, options} = harness({person: identity(0x44),
    entry: [{code: 'invitation-only', error: 'this workspace is joined by invitation'}]});
  await assert.rejects(joinViaHost(options), error => error instanceof HostAdmissionError && error.code === 'invitation-only');
  assert.equal(calls.posted.length, 1);
  assert.equal(calls.connected.length, 0);
});

test('an address left at the entry is sent there, and each refusal is named for the page to say', async () => {
  const posted = [];
  const entry = answer => async (url, init) => { posted.push({url: String(url), init}); return reply(answer); };
  assert.deepEqual(await leaveOnWaitList({workspace: commons, email: ' Ada@Example.org ', language: 'de-AT',
    fetch: entry({body: {email: 'ada@example.org', mail: true}})}), {email: 'ada@example.org', mail: true});
  assert.equal(posted[0].url, 'https://workspaces.example/wait-list');
  assert.equal(posted[0].init.method, 'POST');
  assert.deepEqual(JSON.parse(posted[0].init.body), {email: ' Ada@Example.org ', language: 'de-AT'});
  for (const [status, code, headers = {}] of [[400, 'invalid-email'], [429, 'too-many', {'retry-after': '600'}],
    [503, 'wait-list-full'], [404, 'no-wait-list'], [503, 'unavailable'], [500, 'something-else']]) {
    await assert.rejects(leaveOnWaitList({workspace: commons, email: 'x@y.org', fetch: entry({status, headers, body: {error: 'no', code}})}),
      error => error instanceof HostAdmissionError && error.code === (code === 'something-else' ? 'unavailable' : code)
        && error.retryAfter === (code === 'too-many' ? 600 : null));
  }
  await assert.rejects(leaveOnWaitList({workspace: commons, email: 'x@y.org', fetch: async () => { throw new TypeError('Failed to fetch'); }}),
    {name: 'HostAdmissionError', code: 'unavailable'});
  await assert.rejects(leaveOnWaitList({workspace: {name: 'Closed', host: {origin: 'https://h.example'}}, email: 'x@y.org', fetch: entry({})}),
    {name: 'HostAdmissionError', code: 'invitation-only'});
});

const memoryStorage = () => {
  const values = new Map();
  return {values, getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, String(value)), removeItem: key => values.delete(key)};
};

test('the browser keeps the secret that takes its address off an entry’s wait list, and only for that entry', async () => {
  const storage = memoryStorage();
  const secret = 'a1'.repeat(32);
  const posted = [];
  const entry = answer => async (url, init) => { posted.push({url: String(url), body: JSON.parse(init.body), credentials: init.credentials}); return reply(answer); };
  assert.equal(waitListing(commons, storage), null);
  assert.deepEqual(await leaveOnWaitList({workspace: commons, email: ' Ada@Example.org ', storage,
    fetch: entry({body: {email: 'Ada@Example.org', removal: secret}})}), {email: 'Ada@Example.org', mail: false});
  assert.deepEqual(waitListing(commons, storage), {email: 'Ada@Example.org', pending: false});
  assert.equal(waitListing({...commons, admission: 'https://other.example/'}, storage), null);
  assert.doesNotMatch(JSON.stringify(waitListing(commons, storage)), /a1a1/, 'the secret stays inside');

  // Taking it off sends the secret alone, and forgets it here.
  await leaveWaitList({workspace: commons, storage, fetch: entry({body: {}})});
  assert.equal(posted.at(-1).url, 'https://workspaces.example/wait-list/remove');
  assert.deepEqual(posted.at(-1).body, {secret});
  assert.equal(posted.at(-1).credentials, 'omit');
  assert.equal(waitListing(commons, storage), null);
  assert.equal(storage.values.size, 0);
  // Nothing kept: nothing is asked.
  const asked = posted.length;
  await leaveWaitList({workspace: commons, storage, fetch: entry({body: {}})});
  assert.equal(posted.length, asked);
});

test('a wait-list removal the entry could not do keeps the secret to try again', async () => {
  const storage = memoryStorage();
  await leaveOnWaitList({workspace: commons, email: 'bo@example.org', storage,
    fetch: async () => reply({body: {email: 'bo@example.org', removal: 'b2'.repeat(32)}})});
  for (const [status, code, headers = {}] of [[429, 'too-many', {'retry-after': '60'}], [503, 'unavailable']]) {
    await assert.rejects(leaveWaitList({workspace: commons, storage, fetch: async () => reply({status, headers, body: {error: 'no', code}})}),
      error => error instanceof HostAdmissionError && error.code === code);
    assert.deepEqual(waitListing(commons, storage), {email: 'bo@example.org', pending: false});
  }
  await assert.rejects(leaveWaitList({workspace: commons, storage, fetch: async () => { throw new TypeError('Failed to fetch'); }}),
    {code: 'unavailable'});
  assert.deepEqual(waitListing(commons, storage), {email: 'bo@example.org', pending: false});
  // A secret the entry calls none is nothing to keep.
  await leaveWaitList({workspace: commons, storage, fetch: async () => reply({status: 400, body: {code: 'invalid-secret'}})});
  assert.equal(waitListing(commons, storage), null);
  // An entry that handed back no secret leaves nothing to take off here.
  await leaveOnWaitList({workspace: commons, email: 'cy@example.org', storage, fetch: async () => reply({body: {email: 'cy@example.org'}})});
  assert.equal(waitListing(commons, storage), null);
});

test('a mailed address waits to be confirmed by its link, and the entry says where it stands', async () => {
  const storage = memoryStorage();
  const posted = [];
  const entry = answer => async (url, init) => { posted.push({url: String(url), body: JSON.parse(init.body)}); return reply(answer); };
  const secret = 'c3'.repeat(32);
  await leaveOnWaitList({workspace: commons, email: 'cy@example.org', storage,
    fetch: entry({body: {email: 'cy@example.org', removal: secret, mail: true}})});
  assert.deepEqual(waitListing(commons, storage), {email: 'cy@example.org', pending: true});

  assert.equal(await waitListStatus({workspace: commons, storage, fetch: entry({body: {status: 'pending'}})}), 'pending');
  assert.equal(posted.at(-1).url, 'https://workspaces.example/wait-list/status');
  assert.deepEqual(posted.at(-1).body, {secret});
  // The link confirms it: sent as it came, and this browser stops waiting.
  const token = 'd4'.repeat(32);
  assert.equal(await confirmWaitList({workspace: commons, token, storage, fetch: entry({body: {confirmed: true}})}), true);
  assert.equal(posted.at(-1).url, 'https://workspaces.example/wait-list/confirm');
  assert.deepEqual(posted.at(-1).body, {token});
  assert.deepEqual(waitListing(commons, storage), {email: 'cy@example.org', pending: false});
  // A link no longer waiting, or none at all, confirms nothing.
  assert.equal(await confirmWaitList({workspace: commons, token, storage, fetch: entry({body: {confirmed: false}})}), false);
  const asked = posted.length;
  assert.equal(await confirmWaitList({workspace: commons, token: 'nope', storage, fetch: entry({body: {confirmed: true}})}), false);
  assert.equal(posted.length, asked);
  // The entry cannot say now: nothing changes; it says gone: forgotten.
  assert.equal(await waitListStatus({workspace: commons, storage, fetch: entry({status: 503, body: {code: 'unavailable'}})}), null);
  assert.equal(await waitListStatus({workspace: commons, storage, fetch: async () => { throw new TypeError('Failed to fetch'); }}), null);
  assert.ok(waitListing(commons, storage));
  assert.equal(await waitListStatus({workspace: commons, storage, fetch: entry({body: {status: 'gone'}})}), 'gone');
  assert.equal(waitListing(commons, storage), null);
  assert.equal(await waitListStatus({workspace: commons, storage, fetch: entry({body: {status: 'listed'}})}), null);
});

test('a removal link takes its address off, and this browser forgets what it left', async () => {
  const storage = memoryStorage();
  const posted = [];
  const entry = answer => async (url, init) => { posted.push({url: String(url), body: JSON.parse(init.body)}); return reply(answer); };
  await leaveOnWaitList({workspace: commons, email: 'dee@example.org', storage,
    fetch: entry({body: {email: 'dee@example.org', removal: 'e5'.repeat(32), mail: true}})});
  const linked = 'f6'.repeat(32);
  await leaveWaitList({workspace: commons, secret: linked, storage, fetch: entry({body: {}})});
  assert.deepEqual(posted.at(-1), {url: 'https://workspaces.example/wait-list/remove', body: {secret: linked}});
  assert.equal(waitListing(commons, storage), null);
  await assert.rejects(leaveWaitList({workspace: commons, secret: 'not hex', storage, fetch: entry({body: {}})}), {code: 'invalid-secret'});
});

test('a host that has not answered a kept membership yet keeps it and is not asked to admit again', async () => {
  const membership = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.connect = async connected => ({workspace: connected.workspace, dispose: async () => {},
    whenReady: async () => { throw Object.assign(new Error('Workspace admission timed out'), {name: 'TimeoutError', code: 'timed-out'}); }});
  await assert.rejects(joinViaHost(options), {name: 'TimeoutError'});
  assert.equal(calls.posted.length, 0);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
});

test('a host that does not answer a kept membership at all (restarting) keeps it and is not asked to admit again', async () => {
  const membership = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.connect = async () => { throw new Error('fetch failed'); };
  await assert.rejects(joinViaHost(options), {message: 'fetch failed'});
  assert.equal(calls.posted.length, 0);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
});

test('a second tab is a new device of the person, admitted through the entry once', async () => {
  // Its device key is one no host has seen: the workspace admits each device
  // once, so it asks the entry for the same person rather than fail to connect.
  const membership = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership, created: true});
  const peer = await joinViaHost(options);
  assert.equal(calls.connected.length, 0, 'no connection the workspace would refuse');
  assert.deepEqual(calls.posted.map(post => post.body.userPublicKey), ['11'.repeat(32)]);
  assert.equal(calls.joined[0].deviceKey, calls.deviceKeys[0], 'joined with the new tab\u2019s own key');
  assert.equal(calls.deviceKeys[0].profile, `tilo-identity:${'11'.repeat(32)}@workspaces.example`);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), {...membership, workspace: 'WORKSPACE'});
  // The tab's key is let go with its peer.
  assert.equal(calls.deviceKeys[0].closed, false);
  await peer.dispose();
  assert.equal(calls.deviceKeys[0].closed, true);
});

test('a second tab of a paired device pairs as a new device, approved on one of the person\u2019s devices', async () => {
  const person = '33'.repeat(32);
  const membership = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: person, paired: true};
  const {calls, options} = harness({membership, created: true});
  await joinViaHost({...options, identity: undefined, onPairingCode: () => {}});
  assert.equal(calls.connected.length, 0);
  assert.equal(calls.paired[0].userPublicKey, person);
  assert.equal(calls.paired[0].deviceKey, calls.deviceKeys[0]);
  assert.equal(typeof calls.paired[0].onPairingCode, 'function');
  assert.deepEqual(calls.posted, [{url: 'https://workspaces.example/join', body: {userPublicKey: person, displayName: 'Ada Lovelace'}}],
    'the new device joins the person\u2019s workspace through the entry once paired');
});

test('every device key held by other tabs keeps the membership and asks the host nothing', async () => {
  // Not the host's refusal: this browser has as many tabs open as device
  // keys. Joining again would spend an admission and the address's join window,
  // then fail the same way.
  const membership = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.openDeviceKey = async () => { throw new DeviceKeyError('device-key-busy', 'Every device key is held by another tab'); };
  await assert.rejects(joinViaHost(options), {name: 'DeviceKeyError', code: 'device-key-busy'});
  assert.equal(calls.posted.length, 0);
  assert.equal(calls.joined.length, 0);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), membership);
});

test('a host set up again is joined again through its entry once, without asking', async () => {
  // The peer refuses what this browser kept for the previous host before
  // any session starts (peer.mjs); the person simply joins the new host.
  const membership = {origin: 'https://workspaces.example', workspace: 'OLD', userPublicKey: '11'.repeat(32)};
  const {calls, options, store} = harness({person: identity(0x11), membership});
  options.connect = async connected => {
    calls.connected.push(connected);
    throw Object.assign(new Error(HOST_CHANGED), {name: 'AdmissionError', code: 'host-changed'});
  };
  const peer = await joinViaHost(options);
  assert.equal(calls.connected.length, 1, 'the kept membership was tried once');
  assert.equal(calls.posted.length, 1, 'exactly one join at the entry');
  assert.equal(calls.posted[0].body.userPublicKey, '11'.repeat(32));
  assert.equal(calls.joined.length, 1);
  assert.equal(calls.joined[0].identity.userPublicKey, '11'.repeat(32));
  assert.equal(peer.workspace, 'WORKSPACE');
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}),
    {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)});
});

test('a person invited into another workspace of the home\'s host comes to the home through its entry', async () => {
  // The site's home names its workspace in its entry's address; this browser
  // was let in first by an invitation into another workspace on that host.
  const home = {...commons, admission: 'https://workspaces.example/peer/v1/directory/HOME/'};
  const {calls, options} = harness({person: identity(0x11),
    membership: {origin: 'https://workspaces.example', workspace: 'INVITED', userPublicKey: '11'.repeat(32)}});
  await joinViaHost({...options, workspace: home});
  assert.equal(calls.connected.length, 0, 'the workspace it was invited into is not the home');
  assert.equal(calls.joined[0].entry, home.admission, 'the home\'s entry lets them in');
  // Kept for the home itself, the membership reconnects without the entry.
  const member = harness({person: identity(0x11),
    membership: {origin: 'https://workspaces.example', workspace: 'HOME', userPublicKey: '11'.repeat(32)}});
  await joinViaHost({...member.options, workspace: home});
  assert.equal(member.calls.connected[0].workspace, 'HOME');
  assert.equal(member.calls.posted.length, 0);
});

test('a membership kept for another person is not reused for this one', async () => {
  const {calls, options} = harness({person: identity(0x33), membership: {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '11'.repeat(32)}});
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

test('an entry not reached, and one that refuses for a reason of its own, are told', async () => {
  const down = harness({entry: [{code: 'unanswered', error: 'The workspace\'s entry was not reached'}]});
  await assert.rejects(joinViaHost(down.options), error => error instanceof HostAdmissionError && error.code === 'unavailable');
  const other = harness({entry: [{code: 'refused', error: 'the entry answered 500'}]});
  await assert.rejects(joinViaHost(other.options), error => error instanceof HostAdmissionError && error.code === 'refused');
});

test('an entry with no capacity is waited out as long as it asked, then the same person joins', async () => {
  const busy = {code: 'full', error: 'the workspace is full', retryAfter: 30};
  const {calls, options} = harness({entry: [busy, busy, null]});
  const peer = await joinViaHost(options);
  assert.equal(peer.workspace, 'WORKSPACE');
  assert.equal(calls.posted.length, 3, 'asked again after each wait');
  assert.deepEqual(calls.posted.map(post => post.body.userPublicKey), Array(3).fill('22'.repeat(32)), 'the same person every time');
  assert.deepEqual(calls.waited.map(wait => [wait.attempt, wait.error.code, wait.retryAfter]), [[0, 'full', 30], [1, 'full', 30]]);
  assert.equal(calls.renewed.length, 0, 'capacity was the only thing missing, not the person');
  assert.equal(calls.joined.length, 1, 'one join, once there was capacity');
  assert.equal(calls.joined[0].origin, 'https://workspaces.example', 'an entry that names no origin is its workspace’s host');
});

test('the entry is asked for the browser’s own person, never for a visitor’s device key', async () => {
  const {calls, options} = harness();
  await joinViaHost(options);
  assert.deepEqual(calls.posted[0].body, {userPublicKey: options.identity.userPublicKey, displayName: 'Ada Lovelace'});
});

test('the home workspace is listed first, then the person\u2019s own, each host once', () => {
  const team = {name: 'Team', host: {origin: 'https://team.example'}, workspace: 'r'};
  const again = {name: 'Commons again', host: {origin: 'https://workspaces.example/'}};
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
  await pairViaHost({origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: person, ticket: TICKET, listed: commons,
    displayName: 'Ada', memberships: options.memberships, onPairingCode: code => codes.push(code),
    findProfile: async () => null, agree: async () => true,
    pair: async pairing => { calls.connected.push(pairing); knock(pairing); return {workspace: 'WORKSPACE'}; }});
  const paired = calls.connected[0];
  assert.equal(paired.userPublicKey, person);
  assert.equal(paired.ticket, TICKET, 'the device logs in with the link\u2019s ticket');
  assert.equal(paired.identity, undefined, 'the root stays on the device that approves');
  assert.equal(paired.profile, `tilo-identity:${person}@workspaces.example`);
  assert.equal(typeof paired.onPairingCode, 'function');
  assert.deepEqual(calls.posted, [{url: 'https://workspaces.example/join', body: {userPublicKey: person, displayName: 'Ada'}}],
    'the new device joins the person\u2019s workspace through the host\u2019s entry');
  calls.posted.length = 0;
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}),
    {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: person, paired: true});

  await joinViaHost({...options, identity: undefined});
  assert.equal(calls.posted.length, 0, 'the host is not asked to admit another person');
  assert.equal(calls.connected[1].userPublicKey, person);
  assert.equal(calls.connected[1].profile, paired.profile);
  assert.equal(calls.connected[1].workspace, 'WORKSPACE');
});

test('a browser that becomes another person here forgets the workspaces it entered as the other', async () => {
  const person = '22'.repeat(32);
  const {options, store} = harness({membership: {origin: 'https://workspaces.example', workspace: 'theirs', userPublicKey: '11'.repeat(32)}});
  store.set('tilo-entered-workspaces', JSON.stringify([{origin: 'https://workspaces.example', workspace: 'theirs', hostDevice: 'ab'.repeat(32)}]));
  const forgotten = [];
  await pairViaHost({origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: person, ticket: TICKET, listed: commons,
    memberships: options.memberships, findProfile: async () => null, agree: async () => true,
    runtime: {forgetEnteredAt: (stored, origin) => {
      forgotten.push(origin);
      const kept = JSON.parse(stored).filter(entry => entry.origin !== origin);
      return JSON.stringify({entered: kept, unchoose: []});
    }},
    pair: async () => ({workspace: 'WORKSPACE'})});
  assert.deepEqual(forgotten, ['https://workspaces.example'],
    'the workspaces it entered as the other person there are not this person\u2019s');
  assert.deepEqual(JSON.parse(store.get('tilo-entered-workspaces')), []);
});

test('a pairing link that names no person is refused before anything is asked', async () => {
  const {calls, options} = harness();
  await assert.rejects(pairViaHost({origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: 'nobody', pair: options.connect,
    memberships: options.memberships}), {code: 'invalid'});
  await assert.rejects(pairViaHost({origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '33'.repeat(32), ticket: 'short',
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
const kept = (origin, workspace, extra = {}) => ({origin, workspace, hostDevice: HOST, ...extra});

/** The runtime's rules of the list (tilo-session-client's `entered`, tested
 * there), answering what each was asked with `answer`. */
const rules = answer => {
  const asked = [];
  const runtime = {};
  for (const name of ['rememberEntered', 'nameEntered', 'markEnteredUnavailable', 'forgetGoneWorkspaces', 'forgetDeletedWorkspace', 'settleFailedWorkspace']) {
    runtime[name] = (...args) => { asked.push([name, ...args]); return JSON.stringify(answer(name, ...args)); };
  }
  return {runtime, asked};
};

test('the remembered workspaces are read as kept, and damaged entries and entries with no workspace are never offered', () => {
  const store = memoryStore();
  seed(store, [kept('https://globex.example', 'H', {name: 'Hank'}), kept('https://acme.example/', 'A', {unavailable: true})]);
  assert.deepEqual(enteredWorkspaces(store), [
    {origin: 'https://globex.example', workspace: 'H', hostDevice: HOST, name: 'Hank', unavailable: false},
    {origin: 'https://acme.example', workspace: 'A', hostDevice: HOST, name: null, unavailable: true},
  ]);
  // One that names no host that held it could never be told gone: not offered.
  store.setItem('tilo-entered-workspaces', JSON.stringify([{origin: 'not a url', workspace: 'X', hostDevice: HOST},
    {origin: 'https://ok.example', workspace: 'OK', hostDevice: HOST}, {origin: 'https://old.example', workspace: 'O'}, 7]));
  assert.deepEqual(enteredWorkspaces(store), [{origin: 'https://ok.example', workspace: 'OK', hostDevice: HOST, name: null, unavailable: false}]);
  store.setItem('tilo-entered-workspaces', '{broken');
  assert.deepEqual(enteredWorkspaces(store), []);
});

test('each change is the runtime\'s rules\', written back with no bare entry reopening what it forgot', () => {
  const store = memoryStore();
  seed(store, [kept('https://acme.example', 'A')]);
  chooseWorkspace({origin: 'https://acme.example', workspace: 'A'}, store);
  const after = [kept('https://acme.example', 'B', {name: 'Bee'})];
  const {runtime, asked} = rules(() => ({entered: after, unchoose: [{origin: 'https://acme.example', workspace: 'A'}]}));
  rememberEntered(runtime, {hostDevice: HOST, origin: 'https://acme.example/', workspace: 'B', name: 'Bee'}, store);
  assert.deepEqual(asked[0], ['rememberEntered', JSON.stringify([kept('https://acme.example', 'A')]),
    JSON.stringify({origin: 'https://acme.example', workspace: 'B', hostDevice: HOST, name: 'Bee'})], 'asked with the origin as a link names it');
  assert.deepEqual(JSON.parse(store.getItem('tilo-entered-workspaces')), after);
  assert.equal(chosenWorkspace(store), null, 'the one it forgot is not reopened');
  // What is no entry is not asked about.
  rememberEntered(runtime, {hostDevice: HOST, origin: 'not a url', workspace: 'X'}, store);
  rememberEntered(runtime, {hostDevice: undefined, origin: 'https://acme.example', workspace: 'X'}, store);
  assert.equal(asked.length, 1);
});

test('a bare entry reopens the chosen workspace until it does not open, and choosing the home forgets it', () => {
  const store = memoryStore();
  seed(store, [kept('https://acme.example', 'A', {name: 'Acme'})]);
  // A choice this browser never entered is not reopened.
  chooseWorkspace({origin: 'https://other.example', workspace: 'O'}, store);
  assert.equal(chosenWorkspace(store), null);
  chooseWorkspace({origin: 'https://acme.example/', workspace: 'A'}, store);
  assert.deepEqual(chosenWorkspace(store), {origin: 'https://acme.example', workspace: 'A'});
  chooseWorkspace(null, store);
  assert.equal(chosenWorkspace(store), null);
  chooseWorkspace({origin: 'https://acme.example', workspace: 'A'}, store);
  const {runtime} = rules(() => ({entered: [kept('https://acme.example', 'A', {name: 'Acme', unavailable: true})],
    unchoose: [{origin: 'https://acme.example', workspace: 'A'}]}));
  markUnavailable(runtime, {origin: 'https://acme.example', workspace: 'A'}, store);
  assert.equal(chosenWorkspace(store), null);
  assert.equal(enteredWorkspaces(store)[0].unavailable, true);
});

test('a workspace its owner deleted is forgotten and no longer reopened', () => {
  const store = memoryStore();
  seed(store, [kept('https://acme.example', 'A', {name: 'Acme'}), kept('https://acme.example', 'B')]);
  chooseWorkspace({origin: 'https://acme.example', workspace: 'A'}, store);
  const {runtime, asked} = rules(() => ({entered: [kept('https://acme.example', 'B')], unchoose: [{origin: 'https://acme.example', workspace: 'A'}]}));
  forgetDeletedWorkspace(runtime, {origin: 'https://acme.example/', workspace: 'A'}, store);
  assert.deepEqual(asked[0].slice(0, 1), ['forgetDeletedWorkspace']);
  assert.deepEqual(JSON.parse(asked[0][2]), {origin: 'https://acme.example', workspace: 'A'}, 'asked with the origin as a link names it');
  assert.deepEqual(enteredWorkspaces(store).map(entry => entry.workspace), ['B']);
  assert.equal(chosenWorkspace(store), null, 'the deleted one is not reopened');
  forgetDeletedWorkspace(runtime, {origin: 'not a url', workspace: 'A'}, store);
  assert.equal(asked.length, 1, 'what is no place is not asked about');
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
  assert.equal(await settleFailedWorkspace(runtime, {origin: 'https://lounge.example/', workspace: 'OLD'}, {hostInfo, store}), 'gone');
  assert.equal(rulesAsked[0][3], 'cd'.repeat(32), 'the host answering there now');
  seed(store, [kept('https://away.example', 'AWAY')]);
  assert.equal(await settleFailedWorkspace(runtime, {origin: 'https://away.example', workspace: 'AWAY'}, {hostInfo, store}), 'unavailable');
  assert.equal(rulesAsked[1][3], undefined, 'no answer is no host');
  assert.deepEqual(asked, ['https://lounge.example', 'https://away.example']);
  assert.equal(await settleFailedWorkspace(runtime, {origin: 'https://nowhere.example', workspace: 'X'}, {hostInfo, store}), null);
  assert.equal(rulesAsked.length, 2, 'a workspace this browser does not remember is not asked about');
});

test('a workspace whose origin says its owner deleted it is settled as deleted, and only that answer says so', async () => {
  const store = memoryStore();
  seed(store, [kept('https://acme.example', 'GONE', {name: 'Acme'})]);
  const hostInfo = async () => ({device_public_key: Array(32).fill(0xab)});
  const named = [];
  const deleted = async (origin, workspace) => { named.push([origin, workspace]); return workspace === 'GONE'; };
  const {runtime, asked} = rules((name, list, place, answering, gone) => ({entered: [], unchoose: [JSON.parse(place)],
    settled: gone ? 'deleted' : 'unavailable'}));
  assert.equal(await settleFailedWorkspace(runtime, {origin: 'https://acme.example/', workspace: 'GONE'}, {hostInfo, deleted, store}), 'deleted');
  assert.deepEqual(named, [['https://acme.example', 'GONE']], 'the origin is asked about the workspace by its id');
  assert.deepEqual(asked[0].slice(3), [HOST, true], 'the rules hear the host answering and that it said deleted');
  assert.deepEqual(enteredWorkspaces(store), [], 'forgotten');
  // A host that cannot be reached, or answers anything but yes, deletes nothing.
  for (const answer of [async () => { throw new TypeError('Failed to fetch'); }, async () => 'yes', async () => false]) {
    seed(store, [kept('https://acme.example', 'GONE')]);
    assert.equal(await settleFailedWorkspace(runtime, {origin: 'https://acme.example', workspace: 'GONE'}, {hostInfo, deleted: answer, store}), 'unavailable');
    assert.equal(asked.at(-1)[4], false);
  }
});

test('a host\'s certificate names its device key as the list keeps it', () => {
  assert.equal(hostDeviceOf({device_public_key: Array(32).fill(0xcd)}), 'cd'.repeat(32));
  assert.equal(hostDeviceOf({device_public_key: 'cd'.repeat(32)}), 'cd'.repeat(32));
  assert.equal(hostDeviceOf({device_public_key: ['15', ...Array(31).fill(0)]}), `0f${'00'.repeat(31)}`);
  assert.equal(hostDeviceOf({device_public_key: [undefined, ...Array(31).fill(0)]}), null);
  assert.equal(hostDeviceOf({device_public_key: 'not hex'}), null);
  assert.equal(hostDeviceOf(undefined), null);
});

test('workspaces that share a name are told apart by a place a person knows, never the address dialled', () => {
  const choice = (origin, workspace, name, extra = {}) => ({id: `open-workspace:${origin}/${workspace}`, name, origin, workspace, home: false, unavailable: false, ...extra});
  const labels = (choices, current) => workspaceActions(choices, {current}).map(action => action.label);
  // The common case: distinct names, nothing added.
  assert.deepEqual(labels([choice('https://a.example', 'A', 'Acme'), choice('https://b.example', 'B', 'Lounge')], 'Team'), ['Acme', 'Lounge']);
  // Two Lounges on two sites, beside a third in front: each says its site.
  assert.deepEqual(labels([choice('https://a.example', 'A', 'Lounge'), choice('https://b.example', 'B', 'Lounge')], 'Lounge'),
    ['Lounge · a.example', 'Lounge · b.example']);
  assert.deepEqual(labels([choice('https://a.example', 'A', 'Lounge')], 'Lounge'), ['Lounge · a.example']);
  // Two on one site: the site tells nobody apart, so each says whose it is.
  const one = [choice('https://a.example', '5e8d4510-aaaa', 'Lounge', {role: 'owner'}),
    choice('https://a.example', '84c928ed-bbbb', 'Lounge', {role: 'editor'})];
  assert.deepEqual(labels(one, null), ['Lounge · yours', 'Lounge · editor']);
  assert.equal(workspaceActions(one)[0].title, 'Open Lounge · yours');
  // The host's own name, when it gave one, is the place people read: not the
  // address this browser dialled, and not its port.
  const hosted = [choice('http://127.0.0.1:37400', 'A', 'Studio', {place: 'tilo.localhost:32520'}),
    choice('http://127.0.0.1:37400', 'B', 'Studio', {place: 'Annas Mac'})];
  assert.deepEqual(labels(hosted, null), ['Studio · tilo.localhost', 'Studio · Annas Mac']);
  // An address names nothing at all: with nothing else to go by, the name is
  // all the menu says, and no row ever reads as a host and port.
  const addressed = [choice('http://127.0.0.1:37400', 'A', 'Studio')];
  assert.deepEqual(labels(addressed, 'Studio'), ['Studio']);
  for (const action of workspaceActions([...hosted, ...addressed])) {
    assert.ok(!action.label.includes('127.0.0.1'), action.label);
    assert.ok(!action.subtitle.includes('127.0.0.1'), action.subtitle);
  }
});

test('two workspaces of one name and one owner are told apart by when they were made', () => {
  const now = new Date(2026, 10, 5, 12, 0).getTime();
  const at = (day, hour, minute = 0) => new Date(2026, 10, day, hour, minute).getTime();
  const studio = (workspace, madeAt) => ({id: `open-workspace:https://a.example/${workspace}`, name: 'Studio', origin: 'https://a.example',
    workspace, home: false, own: false, unavailable: false, role: 'owner', madeAt});
  const labels = choices => workspaceActions(choices, {now}).map(action => action.label);
  // Both are the person's, so "yours" tells nothing: the day each was made does.
  assert.deepEqual(labels([studio('A', at(3, 9)), studio('B', at(4, 9))]),
    ['Studio · made 3 Nov', 'Studio · made 4 Nov']);
  // Made the same day, the time of day says which, and a day that is today
  // reads as today.
  const yesterday = [studio('A', at(4, 9, 20)), studio('B', at(4, 16, 45))];
  assert.deepEqual(labels(yesterday), ['Studio · made 4 Nov 09:20', 'Studio · made 4 Nov 16:45']);
  const today = [studio('A', at(5, 9, 20)), studio('B', at(5, 11, 5))];
  assert.deepEqual(labels(today), ['Studio · made today 09:20', 'Studio · made today 11:05']);
  // A year gone by says its year, and the subtitle says when it was made too.
  const old = [studio('A', new Date(2024, 10, 3, 9).getTime())];
  assert.deepEqual(labels(old), ['Studio']);
  assert.equal(workspaceActions(old, {now})[0].subtitle, 'Workspace · a.example · made 3 Nov 2024');
  // Two made in the same minute read alike: nothing is added that would not tell them apart.
  assert.deepEqual(labels([studio('A', at(4, 9)), studio('B', at(4, 9))]), ['Studio', 'Studio']);
});

test('the person’s own desktop is its own kind of place, not another workspace', () => {
  const own = {id: 'open-workspace:own', name: 'Your desktop', origin: null, workspace: null, home: false, own: true, unavailable: false, place: null, role: null, madeAt: null};
  assert.deepEqual(workspaceActions([own]), [{
    id: 'open-workspace:own',
    title: 'Open your desktop',
    label: 'Your desktop',
    source: 'desktop',
    icon: 'user',
    subtitle: 'Your own desktop, in this browser',
  }]);
  // Offered first by the choices, so the menu can stand it above the workspaces.
  const choices = workspaceChoices({own: true});
  assert.deepEqual(choices.map(choice => [choice.own, choice.name, choice.madeAt]), [[true, 'Your desktop', null]]);
});

test('a workspace asked for under a name the switcher holds is numbered past it', () => {
  // The shell's own rule, for the workspaces the page makes: two of one name
  // never stand side by side where a person picks one by its name.
  assert.equal(freeName('Studio', []), 'Studio');
  assert.equal(freeName(' Studio ', ['Home']), 'Studio', 'the name as it would be stored decides it');
  assert.equal(freeName('Studio', ['Home', 'Studio']), 'Studio 2');
  assert.equal(freeName('Studio', ['Studio', 'Studio 3']), 'Studio 2', 'the first free number, not one past the highest');
  assert.equal(freeName('Studio', ['Studio', 'Studio 2', 'Studio 3']), 'Studio 4');
});

test('the person switches between the home and the workspaces they entered, never to the one in front', () => {
  const home = {name: 'Lounge', host: {origin: 'https://workspaces.example'}, admission: 'https://workspaces.example/'};
  const entered = [
    {origin: 'https://acme.example', workspace: 'A', name: 'Acme', unavailable: false},
    {origin: 'https://globex.example', workspace: 'G', name: null, unavailable: true},
    // The workspace the home's entry admitted this browser to is the home itself.
    {origin: 'https://workspaces.example', workspace: 'LOUNGE', name: 'Lounge', unavailable: false},
    // Another workspace on the home's host is a workspace of its own.
    {origin: 'https://workspaces.example', workspace: 'TEAM', name: 'Team', unavailable: false},
  ];
  const fromAcme = workspaceChoices({home, homeWorkspace: 'LOUNGE', entered, current: {kind: 'workspace', host: 'https://acme.example', workspace: 'A'}});
  assert.deepEqual(fromAcme.map(choice => [choice.id, choice.name, choice.workspace, choice.home, choice.unavailable]), [
    ['open-workspace:home', 'Lounge', null, true, false],
    ['open-workspace:https://globex.example/G', 'globex.example', 'G', false, true],
    ['open-workspace:https://workspaces.example/TEAM', 'Team', 'TEAM', false, false],
  ]);
  // In the home (before or after its workspace is known), the home is not offered.
  for (const workspace of [null, 'LOUNGE']) {
    const fromHome = workspaceChoices({home, homeWorkspace: 'LOUNGE', entered, current: {kind: 'workspace', host: 'https://workspaces.example', workspace}});
    assert.deepEqual(fromHome.map(choice => choice.name), ['Acme', 'globex.example', 'Team']);
  }
  // Without a home, the person's own desktop offers every workspace entered.
  assert.equal(workspaceChoices({entered, current: {kind: 'own'}}).length, 4);
  assert.deepEqual(workspaceActions(fromAcme), [
    {id: 'open-workspace:home', title: 'Open Lounge', label: 'Lounge', source: 'workspaces', subtitle: 'Workspace · this site'},
    {id: 'open-workspace:https://globex.example/G', title: 'Open globex.example', label: 'globex.example', source: 'workspaces',
      subtitle: 'Workspace · globex.example · did not open last time'},
    {id: 'open-workspace:https://workspaces.example/TEAM', title: 'Open Team', label: 'Team', source: 'workspaces', subtitle: 'Workspace · workspaces.example'},
  ]);
});

test('the host’s list names the person’s workspaces, and memory says where those on other hosts are', () => {
  const home = {name: 'Lounge', host: {origin: 'https://workspaces.example'}, workspace: 'LOUNGE'};
  const entered = [
    {origin: 'https://workspaces.example', workspace: 'OWN', name: 'Old name', hostDevice: '11'.repeat(32), unavailable: true},
    {origin: 'https://elsewhere.example', workspace: 'AWAY', name: null, hostDevice: '22'.repeat(32), unavailable: false},
  ];
  const listed = [
    // The answering host keeps these itself (it names no address for them):
    // reached here, and named as it names them, no longer marked as not opening.
    {workspace: 'OWN', name: 'My workspace', role: 'owner', host: {name: 'workspaces.example'}, made_at_ms: 1_700_000_000_000},
    {workspace: 'LOUNGE', name: 'Lounge', host: {name: 'workspaces.example'}},
    // Another host's: its name is no page address, so only a workspace this
    // browser reached before is offered.
    {workspace: 'AWAY', name: 'Away', host: {name: 'eu1.example', hint: 'cd'.repeat(8)}},
    {workspace: 'ELSEWHERE', name: 'Unreached', host: {name: 'eu2.example', hint: 'ef'.repeat(8)}},
    // A workspace no device has said the host of yet: nothing opens it.
    {workspace: 'UNKNOWN', name: 'Unknown'},
  ];
  const answering = 'https://workspaces.example';
  assert.deepEqual(knownWorkspaces({entered, listed, answering}), [
    {origin: answering, workspace: 'OWN', name: 'My workspace', hostDevice: '11'.repeat(32), unavailable: false, place: 'workspaces.example', role: 'owner', madeAt: 1_700_000_000_000},
    {origin: 'https://elsewhere.example', workspace: 'AWAY', name: 'Away', hostDevice: '22'.repeat(32), unavailable: false, place: 'eu1.example', role: null, madeAt: null},
    {origin: answering, workspace: 'LOUNGE', name: 'Lounge', unavailable: false, place: 'workspaces.example', role: null, madeAt: null},
  ]);
  // In the Lounge, neither the home nor the workspace in front is offered back.
  assert.deepEqual(workspaceChoices({home, homeWorkspace: 'LOUNGE', entered, listed, answering,
    current: {kind: 'workspace', host: answering, workspace: 'LOUNGE'}}).map(choice => [choice.name, choice.place, choice.role]),
  [['My workspace', 'workspaces.example', 'owner'], ['Away', 'eu1.example', null]]);
  // Before the host answers, the switcher is what the browser remembered.
  assert.deepEqual(knownWorkspaces({entered}), entered);
});

test('the directory lists the workspaces this page holds by the host actions that open them, the one in front included', () => {
  const held = heldWorkspaces({
    entered: [
      {origin: 'https://workspaces.example', workspace: 'WORKSPACE', name: 'Commons'},
      // The same workspace remembered twice is offered once.
      {origin: 'https://workspaces.example/', workspace: 'WORKSPACE', name: 'Commons'},
      {origin: 'https://garden.example', workspace: 'GARDEN', name: null},
    ],
    current: {kind: 'workspace', host: 'https://workspaces.example/join', workspace: 'HERE', name: 'Here'},
  });
  assert.deepEqual(held, [
    {workspace: 'HERE', key: 'open-workspace:https://workspaces.example/HERE', name: 'Here'},
    {workspace: 'WORKSPACE', key: 'open-workspace:https://workspaces.example/WORKSPACE', name: 'Commons'},
    {workspace: 'GARDEN', key: 'open-workspace:https://garden.example/GARDEN', name: 'garden.example'},
  ]);
  // A page in no workspace holds only what it remembered.
  assert.deepEqual(heldWorkspaces({entered: [], current: {kind: 'own'}}), []);
});

test('a join chosen in the directory enters through its entry on the session, then opens under its name', async () => {
  const joined = [];
  const entry = 'https://workspaces.example/peer/v1/directory/WORKSPACE/';
  const grant = await joinGranted(
    {origin: 'https://workspaces.example', entry, workspace: 'WORKSPACE', name: 'Gardeners'},
    {join: async options => { joined.push(options); return {workspace: 'WORKSPACE'}; }, displayName: 'Ada Lovelace'},
  );
  assert.deepEqual(joined, [{origin: 'https://workspaces.example', entry, displayName: 'Ada Lovelace'}],
    'the page enters through the entry itself, holding no invitation or token');
  assert.deepEqual(grant, {peer: {workspace: 'WORKSPACE'}, origin: 'https://workspaces.example', workspace: 'WORKSPACE', name: 'Gardeners'});
});

test('a pasted link\'s invitation is redeemed with the same join', async () => {
  const joined = [];
  await joinGranted(
    {origin: 'https://workspaces.example', invitation: 'INV', workspace: 'WORKSPACE', name: 'Gardeners'},
    {join: async options => { joined.push(options); return {workspace: 'WORKSPACE'}; }},
  );
  assert.deepEqual(joined, [{origin: 'https://workspaces.example', invitation: 'INV'}]);
});

test('a grant that names neither an entry nor an invitation joins nothing', async () => {
  await assert.rejects(() => joinGranted({origin: 'https://workspaces.example', invitation: ''}, {join: async () => {}}), TypeError);
  await assert.rejects(() => joinGranted({origin: '', entry: 'https://workspaces.example/directory/W/'}, {join: async () => {}}), TypeError);
});

test('an invitation to another workspace on the home\'s host leaves the home its own name and choice', () => {
  const store = memoryStore();
  const home = {name: 'Lounge', host: {origin: 'https://lounge.example'}};
  const person = 'cd'.repeat(32);
  // The home's entry let this browser into its workspace.
  keepJoinedHost({origin: 'https://lounge.example', workspace: 'lounge', userPublicKey: person}, store);
  // Then the person accepts an invitation to a customer's workspace on the same host.
  keepJoinedHost({origin: 'https://lounge.example/', workspace: 'customer', userPublicKey: person}, store);
  assert.equal(keptMembership(home, store).workspace, 'lounge');
  const scope = workspace => ({kind: 'workspace', host: 'https://lounge.example', workspace});
  assert.equal(listedAt([home], scope('customer'), null, store), null, 'the customer\'s workspace is not named Lounge');
  assert.equal(listedAt([home], scope('lounge'), null, store), home);
  const choices = workspaceChoices({home, homeWorkspace: keptMembership(home, store).workspace,
    entered: [{origin: 'https://lounge.example', workspace: 'customer', name: 'Applicant: Jane Doe', unavailable: false}],
    current: scope('customer')});
  assert.deepEqual(choices.map(choice => [choice.name, choice.workspace]), [['Lounge', null]], 'the home stays one to go back to');
  // Another person on this browser joins as themselves.
  keepJoinedHost({origin: 'https://lounge.example', workspace: 'customer', userPublicKey: 'ef'.repeat(32)}, store);
  assert.equal(keptMembership(home, store).workspace, 'customer');
});

test('a kept membership or entered workspace that names no workspace is left out, not opened half-read', () => {
  const store = new Map([[KEY, JSON.stringify({
    'https://workspaces.example': {origin: 'https://workspaces.example', workspaceName: 'WORKSPACE', userPublicKey: '11'.repeat(32)},
    'https://other.example': {origin: 'https://other.example', workspace: null, userPublicKey: '22'.repeat(32)},
  })]]);
  const memberships = {getItem: key => store.get(key)};
  assert.equal(keptMembership(commons, memberships), null);
  assert.deepEqual(keptMembership({host: {origin: 'https://other.example'}}, memberships),
    {origin: 'https://other.example', workspace: null, userPublicKey: '22'.repeat(32)});
  // Naming or marking a workspace the caller could not name asks the runtime nothing.
  const runtime = new Proxy({}, {get: (_, rule) => () => { throw new Error(`asked ${String(rule)}`); }});
  const entered = {getItem: () => '[]', setItem: () => {}};
  nameEntered(runtime, {origin: 'https://workspaces.example', workspace: undefined, name: 'Tilo'}, entered);
  markUnavailable(runtime, {origin: 'https://workspaces.example', workspace: undefined}, entered);
});

test('the workspace an entry admits into: the one its listing names, else the one its address does', () => {
  assert.equal(entryWorkspace({workspace: 'w1', admission: 'https://h.example/peer/v1/directory/w2/'}), 'w1');
  assert.equal(entryWorkspace({host: {origin: 'https://h.example'}, admission: 'https://h.example/peer/v1/directory/w2/'}), 'w2');
  assert.equal(entryWorkspace({admission: 'https://h.example/peer/v1/admission'}), null);
  assert.equal(entryWorkspace(null), null);
});

test('a browser is not paired to the person it already is: the link is refused in plain words and nothing changes', async () => {
  // A pairing link opened in the browser that made it. Its new tab asked the
  // host, the first tab approved, and the browser was kept as a device paired
  // to its own person: from then on it left the root it holds unused, and
  // offered to pair no further device.
  const person = identity(0x22);
  const joined = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: person.userPublicKey};
  const {calls, options, store} = harness({person, membership: joined});
  const before = store.get(KEY);
  const link = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: person.userPublicKey, ticket: TICKET, listed: commons,
    memberships: options.memberships, onPairingCode: () => assert.fail('no code is shown: no host was asked'),
    findProfile: async () => null,
    pair: async pairing => { calls.paired.push(pairing); return {workspace: 'WORKSPACE'}; }};
  const refused = await pairViaHost({...link, holds: person.userPublicKey, agree: () => assert.fail('nobody is asked whose device this becomes: it is its own person’s already')})
    .then(() => null, error => error);
  assert.equal(store.get(KEY), before, 'what this browser keeps of the host is as it was: it is not marked paired');
  assert.deepEqual(calls.paired, [], 'the host is asked nothing, so the link is not spent and nobody is asked to approve');
  assert.equal(refused?.name, 'HostAdmissionError');
  assert.equal(refused?.code, 'same-person');
  assert.equal(refused?.message, PAIRING_SAME_PERSON);
  assert.deepEqual(keptMembership(commons, {getItem: key => store.get(key)}), joined);

  // The same link in a browser that holds another person, or none, pairs it.
  for (const holds of [identity(0x44).userPublicKey, null]) {
    store.delete(KEY);
    calls.paired.length = 0;
    await pairViaHost({...link, holds, onPairingCode: () => {}, agree: async () => true});
    assert.equal(calls.paired.length, 1, String(holds));
    assert.equal(keptMembership(commons, {getItem: key => store.get(key)}).paired, true, String(holds));
  }
});

test('a browser that was paired to its own person is its own person again; a device paired to someone else stays paired', async () => {
  const person = identity(0x22).userPublicKey;
  const other = identity(0x33).userPublicKey;
  const hosts = {
    'https://workspaces.example': {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: person, paired: true},
    'https://elsewhere.example': {origin: 'https://elsewhere.example', workspace: 'W2', userPublicKey: other, paired: true},
    'https://own.example': {origin: 'https://own.example', workspace: 'W3', userPublicKey: person, owner: true},
  };
  const store = new Map([[KEY, JSON.stringify(hosts)]]);
  const memberships = {getItem: key => store.get(key) ?? null, setItem: (key, value) => store.set(key, value)};
  let asked = 0;
  const keeps = root => async () => { asked += 1; return root ? {rootPublicKey: root} : null; };

  // A device that keeps no person, or another one, stays paired.
  for (const root of [null, identity(0x44).userPublicKey]) {
    assert.deepEqual(await unmarkOwnPairing(keeps(root), memberships), []);
    assert.deepEqual(JSON.parse(store.get(KEY)), hosts, String(root));
  }
  // The browser that keeps that person's root is them at that host again,
  // and nothing else it keeps changes.
  assert.deepEqual(await unmarkOwnPairing(keeps(person), memberships), ['https://workspaces.example']);
  const {paired: _, ...joined} = hosts['https://workspaces.example'];
  assert.deepEqual(JSON.parse(store.get(KEY)), {...hosts, 'https://workspaces.example': joined});
  assert.deepEqual(keptMembership(commons, memberships), joined);

  // A browser with no paired host is not asked whom it keeps, and writes nothing.
  const plain = JSON.stringify({'https://own.example': hosts['https://own.example']});
  asked = 0;
  assert.deepEqual(await unmarkOwnPairing(keeps(person),
    {getItem: () => plain, setItem: () => assert.fail('nothing to change')}), []);
  assert.equal(asked, 0);
});

/** A host that answers what it keeps of a person's profile, and keeps what it was asked. */
const profileHost = (asked, kept) => async (url, init) => {
  asked.push({url: String(url), body: init?.body});
  return reply({body: {profile: kept(init?.body)}});
};

test('a device says whose it becomes before any host is asked to let it in, and stopping there pairs nothing', async () => {
  const {PAIRING_STOPPED} = await import('./pairing.mjs');
  const {calls, options, store} = harness();
  const person = '33'.repeat(32);
  const asked = [];
  const said = [];
  const link = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: person, ticket: TICKET, listed: commons,
    memberships: options.memberships, onPairingCode: () => {}, ofSite: true,
    fetch: profileHost(asked, key => ({version: 1, user_public_key: key, display_name: 'Ada Lovelace'})),
    pair: async pairing => { calls.paired.push(pairing); return {workspace: 'WORKSPACE'}; }};
  const stopped = await pairViaHost({...link, agree: async who => { said.push({...who, paired: calls.paired.length}); return false; }})
    .then(() => null, error => error);
  assert.deepEqual(asked, [{url: 'https://workspaces.example/peer/v1/profile/fetch', body: person}],
    'the host that keeps the person is asked who they are, by the key the link names');
  assert.deepEqual(said, [{userPublicKey: person, name: 'Ada Lovelace', holds: null, origin: 'https://workspaces.example', ofSite: true, paired: 0}],
    'the person is told before the host is asked to let the device in, and which server that is');
  assert.equal(stopped?.name, 'HostAdmissionError');
  assert.equal(stopped?.code, 'stopped');
  assert.equal(stopped?.message, PAIRING_STOPPED);
  assert.deepEqual(calls.paired, [], 'the host is asked nothing: the link is not spent and nobody is asked to approve');
  assert.equal(store.get(KEY), undefined, 'and this browser keeps nothing of it');

  await pairViaHost({...link, agree: async () => true});
  assert.equal(calls.paired.length, 1, 'having agreed, it asks');
  assert.equal(keptMembership(commons, {getItem: key => store.get(key)}).paired, true);
});

test('the name a device shows is its host’s word for the person the link names, and never another’s', async () => {
  const {calls, options} = harness();
  const person = '33'.repeat(32);
  const names = [];
  const link = {origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: person, listed: commons, memberships: options.memberships,
    onPairingCode: () => {}, ofSite: true, agree: async who => { names.push(who.name); return false; },
    pair: async pairing => { calls.paired.push(pairing); return {workspace: 'WORKSPACE'}; }};
  const hosts = [
    // The host keeps no profile of them.
    async () => reply({body: {profile: null}}),
    // It answers with someone else's.
    async () => reply({body: {profile: {version: 1, user_public_key: '44'.repeat(32), display_name: 'Bo Smith'}}}),
    // It does not answer, or answers that it cannot.
    async () => { throw new TypeError('Failed to fetch'); },
    async () => reply({status: 500}),
    // It answers with a name that would turn the sentence round.
    async () => reply({body: {profile: {version: 1, user_public_key: person, display_name: 'Ada‮\nLovelace'}}}),
  ];
  // A link names the server it is followed to, and whoever keeps that server
  // answers any name for the key, its reader's own too. One the site does
  // not name as its own is asked nothing before the person goes on, and its
  // word names nobody: so it is for a caller that did not say whose it is.
  const asked = [];
  const said = [];
  const anyName = async (url, init) => {
    asked.push(String(url));
    return reply({body: {profile: {version: 1, user_public_key: init?.body, display_name: 'Bo Smith'}}});
  };
  const others = [false, undefined, 'yes', 1].map(ofSite => ({ofSite, fetch: anyName, agree: async who => { said.push(who); return false; }}));
  for (const asking of [...hosts.map(fetch => ({fetch})), ...others]) await assert.rejects(pairViaHost({...link, ...asking}), {code: 'stopped'});
  assert.deepEqual(names, ['', '', '', '', 'Ada Lovelace']);
  assert.deepEqual(asked, [], 'no server but the site’s own is asked who the person is');
  assert.deepEqual(said, Array(4).fill({userPublicKey: person, name: '', holds: null, origin: 'https://workspaces.example', ofSite: false}));
  assert.deepEqual(calls.paired, []);
});

test('a device that stopped while it waited keeps nothing, though its request was approved meanwhile', async () => {
  const {PAIRING_STOPPED} = await import('./pairing.mjs');
  const {options, store} = harness();
  let stopped = false;
  let letGo = 0;
  const refused = await pairViaHost({origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '33'.repeat(32), ticket: TICKET,
    listed: commons, memberships: options.memberships, findProfile: async () => null, agree: async () => true, stopped: () => stopped,
    onPairingCode: () => {},
    // The person stops while the other device decides, and it approves all the same.
    pair: async () => { stopped = true; return {workspace: 'WORKSPACE', dispose: async () => { letGo += 1; }}; }}).then(() => null, error => error);
  assert.equal(refused?.code, 'stopped');
  assert.equal(refused?.message, PAIRING_STOPPED);
  assert.equal(letGo, 1, 'the device it would have been is let go');
  assert.equal(store.get(KEY), undefined, 'and this browser is kept as nobody’s device');
});

test('nothing pairs a device that was not first told whose it becomes', async () => {
  const {calls, options} = harness();
  await assert.rejects(pairViaHost({origin: 'https://workspaces.example', workspace: 'WORKSPACE', userPublicKey: '33'.repeat(32), listed: commons,
    memberships: options.memberships, findProfile: async () => null,
    pair: async pairing => { calls.paired.push(pairing); return {workspace: 'WORKSPACE'}; }}), TypeError);
  assert.deepEqual(calls.paired, []);
});

test('a server’s public directory is read as each workspace’s own entry, whether it lets anyone in, and its wait list', async () => {
  const asked = [];
  const listings = await publicListings('https://ada.tilo.cloud/some/page', {fetch: async (url, init) => {
    asked.push([url, init.method, init.credentials]);
    return {ok: true, json: async () => ({workspaces: [
      {workspace: 'open', name: 'Reading group', entry: 'directory/open/', invitationOnly: false, waitList: null},
      {workspace: 'lounge', name: '', entry: 'directory/lounge/', invitationOnly: true, waitList: {contact: 'hi@ada.example', mail: true}},
      {workspace: 'broken'},
    ]})};
  }});
  assert.deepEqual(asked, [['https://ada.tilo.cloud/peer/v1/directory', 'POST', 'omit']]);
  assert.deepEqual(listings, [
    {workspace: 'open', name: 'Reading group', entry: 'https://ada.tilo.cloud/peer/v1/directory/open/', invitationOnly: false, waitList: null},
    {workspace: 'lounge', name: null, entry: 'https://ada.tilo.cloud/peer/v1/directory/lounge/', invitationOnly: true,
      waitList: {contact: 'hi@ada.example', mail: true}},
  ]);
  await assert.rejects(publicListings('https://ada.tilo.cloud', {fetch: async () => ({ok: false, status: 404})}), /404/);
});

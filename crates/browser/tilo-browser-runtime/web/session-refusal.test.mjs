import assert from 'node:assert/strict';
import {test} from 'node:test';

import {followRefusal, hostUnreachable, reachedHost, REJOIN_WINDOW_MS, REJOINED, refusalText, rejoinedName, rejoinedRecently, retryRejoin, retryUnreachable, UNREACHED} from './session-refusal.mjs';

/** A peer whose snapshots the test sends; records recover requests. */
function fakePeer({recover = async () => {}} = {}) {
  const listeners = new Set();
  const recovered = [];
  return {
    recovered,
    send: snapshot => { for (const listener of [...listeners]) listener(snapshot); },
    listening: () => listeners.size,
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    getSnapshot: () => ({phase: 'ready'}),
    async recover(request) { recovered.push(request.kind); return recover(request); },
  };
}

function memoryStorage() {
  const map = new Map();
  return {getItem: key => map.get(key) ?? null, setItem: (key, value) => { map.set(key, String(value)); }, removeItem: key => { map.delete(key); }, map};
}

const failed = errorCode => ({phase: 'failed', errorCode});
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

function follow({peer, canRejoin = () => true, storage = memoryStorage(), now = () => 1_000_000}) {
  const seen = {rejoined: [], refused: []};
  const stop = followRefusal({peer, canRejoin, storage, now, displayName: () => 'Brave Otter',
    rejoin: async code => { seen.rejoined.push(code); },
    refuse: async code => { seen.refused.push(code); }});
  return {seen, stop, storage};
}

test('a host that forgot the person, or was set up again, is joined again through its door, once', async () => {
  for (const code of ['not-admitted', 'host-changed']) {
    const peer = fakePeer();
    const {seen, storage} = follow({peer});
    peer.send(failed(code));
    await settle();
    assert.deepEqual(seen, {rejoined: [code], refused: []});
    assert.deepEqual(JSON.parse(storage.map.get(REJOINED)), {at: 1_000_000, name: 'Brave Otter'});
    assert.equal(rejoinedName({storage, now: 1_000_001}), 'Brave Otter', 'the reloaded page joins under the same name');
    assert.equal(peer.listening(), 0, 'stops following once it acts');
  }
});

test('a page refused again soon after joining again by itself is told, not looped', async () => {
  const storage = memoryStorage();
  storage.setItem(REJOINED, JSON.stringify({at: 1_000_000 - REJOIN_WINDOW_MS + 1}));
  const peer = fakePeer();
  const {seen} = follow({peer, storage});
  peer.send(failed('not-admitted'));
  await settle();
  assert.deepEqual(seen, {rejoined: [], refused: ['not-admitted']});
  assert.ok(rejoinedRecently({storage, now: 1_000_000}));
  assert.ok(!rejoinedRecently({storage, now: 1_000_000 + REJOIN_WINDOW_MS}));
  assert.equal(rejoinedName({storage, now: 1_000_000}), null);
});

test('a page with no door to come back through, and a revoked device, are told what happened', async () => {
  const peer = fakePeer();
  const {seen} = follow({peer, canRejoin: () => false});
  peer.send(failed('not-admitted'));
  await settle();
  assert.deepEqual(seen, {rejoined: [], refused: ['not-admitted']});
  const revoked = fakePeer();
  const told = follow({peer: revoked});
  revoked.send(failed('device-revoked'));
  await settle();
  assert.deepEqual(told.seen, {rejoined: [], refused: ['device-revoked']});
});

test('a certificate the host did not take is issued again once, then said', async () => {
  const peer = fakePeer();
  const {seen} = follow({peer});
  peer.send(failed('certificate-rejected'));
  await settle();
  assert.deepEqual(peer.recovered, ['reauthenticate']);
  assert.deepEqual(seen, {rejoined: [], refused: []});
  peer.send(failed('certificate-rejected'));
  await settle();
  assert.deepEqual(peer.recovered, ['reauthenticate']);
  assert.deepEqual(seen.refused, ['certificate-rejected']);
  const failing = fakePeer({recover: async () => { throw new Error('locked'); }});
  const again = follow({peer: failing});
  failing.send(failed('certificate-rejected'));
  await settle();
  assert.deepEqual(again.seen.refused, ['certificate-rejected']);
});

test('reachability and other failures are left to the session', async () => {
  const peer = fakePeer();
  const {seen} = follow({peer});
  for (const snapshot of [failed('transport-unavailable'), {phase: 'incomplete', errorCode: 'not-admitted'}, {phase: 'ready'}]) {
    peer.send(snapshot);
  }
  await settle();
  assert.deepEqual(seen, {rejoined: [], refused: []});
  assert.equal(peer.listening(), 1);
});

test('what the person is told names the workspace and never asks them to certify anything', () => {
  for (const code of ['not-admitted', 'host-changed', 'device-revoked', 'certificate-rejected']) {
    const text = refusalText(code, 'Lounge');
    assert.match(text, /Lounge/);
    assert.doesNotMatch(text, /certif/i);
  }
  assert.match(refusalText('device-revoked'), /removed from your devices/);
});

test('a door that does not answer yet after joining again is tried again a bounded number of times', () => {
  const storage = memoryStorage();
  assert.equal(retryRejoin({storage, now: 5}), null, 'only after joining again by itself');
  storage.setItem(REJOINED, JSON.stringify({at: 1, name: 'Brave Otter'}));
  const waits = [];
  for (let wait = retryRejoin({storage, now: 2}); wait !== null; wait = retryRejoin({storage, now: 2})) waits.push(wait);
  assert.deepEqual(waits, [1000, 2000, 4000, 8000, 15_000, 15_000, 15_000, 15_000]);
  assert.equal(rejoinedName({storage, now: 2}), 'Brave Otter');
});

test('a host that does not answer before the page got in is asked again, later each time, a bounded number of times', () => {
  const storage = memoryStorage();
  const waits = [];
  for (let wait = retryUnreachable({storage, now: 10}); wait !== null; wait = retryUnreachable({storage, now: 20})) waits.push(wait);
  assert.deepEqual(waits, [1000, 2000, 4000, 8000, 15_000, 15_000, 15_000, 15_000]);
  assert.equal(retryUnreachable({storage, now: 10 + 10 * 60 * 1000}), 1000, 'tries long ago count no more');
  reachedHost({storage});
  assert.equal(storage.getItem(UNREACHED), null, 'getting in starts them over');
});

test('only a host not answering at all is unreachable, never its refusal', () => {
  assert.ok(hostUnreachable(new TypeError('Failed to fetch')));
  assert.ok(hostUnreachable(new Error('fetch failed')));
  assert.ok(hostUnreachable(new TypeError('NetworkError when attempting to fetch resource.')));
  assert.ok(hostUnreachable(Object.assign(new Error('join: HTTP 503'), {name: 'HostAdmissionError', code: 'unavailable'})));
  assert.ok(!hostUnreachable(Object.assign(new Error('needs an invitation'), {name: 'HostAdmissionError', code: 'invitation-only'})));
  assert.ok(!hostUnreachable(Object.assign(new Error('refused'), {name: 'AdmissionError', code: 'refused'})));
  assert.ok(!hostUnreachable(null));
});

// Tests of the end-to-end authentication check's own reading
// (dev/auth-e2e.mjs); the run itself needs the built stack and Chrome, which
// the `Browser / authentication` workflow has.
import assert from 'node:assert/strict';
import test from 'node:test';
import {joinsTo, keptApart, keptNamespaces, membershipFor, parseArgs, parseBanner, PRUNE_AFTER_MINS, PRUNE_WAIT_MS, REMOVE_MEMBER, SETTLED}
  from './auth-e2e.mjs';

test('the arguments name every input, absolute', () => {
  const options = parseArgs(['--bundle', 'b', '--relay-bin', 'r', '--room-service-bin', 's', '--out', 'o',
    '--expect-revision', 'abc', '--timeout', '60']);
  assert.ok(options.bundle.endsWith('/b') && options.relayBin.endsWith('/r') && options.out.endsWith('/o'));
  assert.equal(options.expectRevision, 'abc');
  assert.equal(options.timeout, 60);
  assert.equal(options.allowDev, false);
  assert.throws(() => parseArgs(['--bundle', 'b']), /usage/);
  assert.throws(() => parseArgs(['--bundle', 'b', '--relay-bin', 'r', '--room-service-bin', 's', '--out', 'o', '--x']), /unknown/);
  assert.throws(() => parseArgs(['--bundle', 'b', '--relay-bin', 'r', '--room-service-bin', 's', '--out', 'o', '--timeout', '0']),
    /positive/);
  assert.throws(() => parseArgs(['--out']), /needs a value/);
});

test("run.py's banner gives the room", () => {
  const banner = "\nTilo is up in 3.2s, room 'auth' (0b5e7a62-4d0f-4c55-9d2a-58d3a3f0c001), bundle abc\n";
  assert.deepEqual(parseBanner(`run.py: $ something\n${banner}`), {room: '0b5e7a62-4d0f-4c55-9d2a-58d3a3f0c001'});
  assert.equal(parseBanner('run.py: building'), null);
});

test('the membership is read by normalized origin, and nothing else counts', () => {
  const kept = JSON.stringify({'http://localhost:9764': {origin: 'http://localhost:9764', room: 'r', userPublicKey: 'k'}});
  assert.deepEqual(membershipFor(kept, 'http://localhost:9764/'), {origin: 'http://localhost:9764', room: 'r', userPublicKey: 'k'});
  assert.equal(membershipFor(kept, 'http://localhost:9765'), null);
  assert.equal(membershipFor(null, 'http://localhost:9764'), null);
  assert.equal(membershipFor('not json', 'http://localhost:9764'), null);
});

test('only POSTs to the door are joins', () => {
  const requests = [
    {url: 'http://localhost:8790/join', method: 'POST'},
    {url: 'http://localhost:8790/join', method: 'OPTIONS'},
    {url: 'http://localhost:8790/presence', method: 'GET'},
    {url: 'http://localhost:8791/join', method: 'POST'},
  ];
  assert.deepEqual(joinsTo(requests, 'http://localhost:8790'), [requests[0]]);
});

test('the settled entry states are the ones the entry page sets', () => {
  assert.deepEqual(SETTLED, ['open', 'refused', 'failed', 'unsupported']);
});

test('what a browser kept is read per namespace, with the host its certificate is from', () => {
  const certificate = device => JSON.stringify({cert: {}, hostCert: {device_public_key: Array(32).fill(device)}});
  const rows = [
    {device: 'tilo-browser-device', key: 'b', digest: '02', certificate: null},
    {device: 'tilo-browser-device', key: 'a', digest: '01', certificate: certificate(1)},
    {device: `tilo-browser-device:${'02'.repeat(32)}`, key: 'a', digest: '03', certificate: certificate(2)},
  ];
  const kept = keptNamespaces(rows);
  assert.equal(kept['tilo-browser-device'].rows, 2);
  assert.equal(kept['tilo-browser-device'].hostDevice, '01'.repeat(32));
  assert.equal(kept[`tilo-browser-device:${'02'.repeat(32)}`].hostDevice, '02'.repeat(32));
  assert.equal(keptNamespaces([...rows].reverse())['tilo-browser-device'].digest, kept['tilo-browser-device'].digest, 'in any order');
  assert.notEqual(keptNamespaces([{...rows[0], digest: 'ff'}, rows[1]])['tilo-browser-device'].digest, kept['tilo-browser-device'].digest,
    'any changed row changes the digest');
});

test('state kept apart means the old namespace untouched and a new one certified by the new host', () => {
  const fresh = `tilo-browser-device:${'02'.repeat(32)}`;
  const old = {rows: 3, digest: 'd1', hostDevice: '01'.repeat(32)};
  const before = {current: 'tilo-browser-device', namespaces: {'tilo-browser-device': old}};
  const after = {current: fresh, namespaces: {'tilo-browser-device': old, [fresh]: {rows: 2, digest: 'd2', hostDevice: '02'.repeat(32)}}};
  assert.equal(keptApart(before, after), true);
  // The old namespace written to (reused), never moved off, a new one of the
  // old host, or nothing read: each fails.
  assert.equal(keptApart(before, {...after, namespaces: {...after.namespaces, 'tilo-browser-device': {...old, digest: 'd3'}}}), false);
  assert.equal(keptApart(before, {current: 'tilo-browser-device', namespaces: before.namespaces}), false);
  assert.equal(keptApart(before, {...after, namespaces: {...after.namespaces, [fresh]: {rows: 2, digest: 'd2', hostDevice: old.hostDevice}}}), false);
  assert.equal(keptApart({current: null, namespaces: {}, error: 'unreadable'}, after), false);
  assert.equal(keptApart(before, {current: null, namespaces: {}, error: 'unreadable'}), false);
});

test('the pruned person is waited for past the horizon and two prune passes', () => {
  // The relay prunes once a minute at most, and a guest it has no record of
  // on a pass is recorded then and pruned on a later one.
  const pass = Math.max(60, PRUNE_AFTER_MINS * 15) * 1000;
  assert.ok(PRUNE_WAIT_MS > PRUNE_AFTER_MINS * 60_000 + pass);
  assert.ok(PRUNE_WAIT_MS <= 5 * 60_000, 'the check stays a few minutes long');
});

test("a moderator removes someone through the desktop's door, confirmed, and only a completed removal counts", async () => {
  const person = 'bb'.repeat(32);
  const asked = [];
  const answer = result => async (...args) => { asked.push(args); return result; };
  try {
    globalThis.tiloMount = {invokeAction: answer({state: 'completed', output: {value: {members: [{userPublicKey: person, removed: true}]}}})};
    assert.deepEqual(await eval(REMOVE_MEMBER('room-1', person)), {ok: true, state: 'completed', removed: true});
    assert.deepEqual(asked[0], ['workspace.permissions:room-1', 'remove-member', {person}, {confirm: true}]);
    globalThis.tiloMount = {invokeAction: answer({state: 'completed', output: {value: {members: [{userPublicKey: person, removed: false}]}}})};
    assert.equal((await eval(REMOVE_MEMBER('room-1', person))).ok, false, 'still a member');
    globalThis.tiloMount = {invokeAction: async () => { throw Error('action_failed: not a moderator'); }};
    assert.deepEqual(await eval(REMOVE_MEMBER('room-1', person)), {ok: false, error: 'action_failed: not a moderator'});
    delete globalThis.tiloMount;
    assert.equal((await eval(REMOVE_MEMBER('room-1', person))).ok, false, 'no desktop');
  } finally {
    delete globalThis.tiloMount;
  }
});

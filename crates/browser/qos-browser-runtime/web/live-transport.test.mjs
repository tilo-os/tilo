import test from 'node:test';
import assert from 'node:assert/strict';
import {createLiveTransport, validateLiveEndpoint, MAX_OPEN_RECORDS} from './live-transport.mjs';
import {createFakeRelay} from './live-fake-relay.test.mjs';
import * as moq from './media-moq-library.mjs';

const ROOM = 'a'.repeat(64);
const A = 'b'.repeat(64);
const B = 'c'.repeat(64);
const ENDPOINT = 'https://host.test:19343/moq/v21/live';
const track = n => `ops/${n.toString(16).padStart(64, '0')}`;
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
async function until(predicate, label, limit = 200) {
  for (let i = 0; i < limit; i++) { if (predicate()) return; await tick(); }
  assert.fail(`timed out waiting for ${label}`);
}

function relay() {
  // The grant bytes in these tests are the publisher hex, standing in for a signed grant.
  return createFakeRelay({authorize: bytes => ({namespace: `tilo-live/${ROOM}/${new TextDecoder().decode(bytes)}`})});
}
async function open(fake, publisher, directions = {publish: true, subscribe: true}) {
  const transport = createLiveTransport({library: fake.library});
  return transport.connect({
    endpoint: ENDPOINT,
    authorization: new TextEncoder().encode(publisher),
    namespace: ['tilo-live', ROOM, publisher],
    directions,
  });
}

test('endpoint and namespace validation', async () => {
  assert.throws(() => validateLiveEndpoint('https://host.test/moq/v21/voice'));
  assert.throws(() => validateLiveEndpoint('http://host.test/moq/v21/live'));
  assert.throws(() => validateLiveEndpoint('https://u:p@host.test/moq/v21/live'));
  assert.equal(validateLiveEndpoint(ENDPOINT).pathname, '/moq/v21/live');
  const fake = relay();
  const transport = createLiveTransport({library: fake.library});
  await assert.rejects(transport.connect({endpoint: ENDPOINT, authorization: new Uint8Array([1]), namespace: ['tilo-voice', ROOM, A], directions: {publish: true, subscribe: true}}));
  await assert.rejects(transport.connect({endpoint: ENDPOINT, authorization: new Uint8Array(), namespace: ['tilo-live', ROOM, A], directions: {publish: true, subscribe: true}}));
});

test('members discover each other and exchange presence datagrams', async () => {
  const fake = relay();
  const a = await open(fake, A);
  const b = await open(fake, B);
  const seenByB = [];
  const members = [];
  b.watchMembers({onMember: (id, active) => members.push([id, active])});
  await until(() => members.length === 1, 'announcement');
  assert.deepEqual(members, [[A, true]], 'own namespace is excluded');
  b.subscribePresence(A, (bytes, at) => seenByB.push([...bytes]));
  await tick();
  assert.equal(a.publishPresence(Uint8Array.of(1, 2, 3)), true);
  assert.equal(a.publishPresence(Uint8Array.of(4)), true);
  await until(() => seenByB.length === 2, 'presence');
  assert.deepEqual(seenByB, [[1, 2, 3], [4]]);
  assert.equal(a.stats().mode, 'datagrams');
  assert.throws(() => a.publishPresence(new Uint8Array(1101)), /datagram bound/);
  a.close();
  await until(() => members.length === 2, 'departure');
  assert.deepEqual(members[1], [A, false]);
  b.close();
});

test('ops frames arrive in order, including for a subscription made before the publisher opened the record', async () => {
  const fake = relay();
  const a = await open(fake, A);
  const b = await open(fake, B);
  const early = [];
  const late = [];
  b.subscribeOps(A, track(1), bytes => early.push(bytes[0]));
  await tick();
  for (let i = 0; i < 50; i++) a.publishOps(track(1), Uint8Array.of(i));
  await until(() => early.length === 50, 'early ops');
  assert.deepEqual(early, [...Array(50).keys()]);
  for (let i = 0; i < 3; i++) a.publishOps(track(2), Uint8Array.of(100 + i));
  b.subscribeOps(A, track(2), bytes => late.push(bytes[0]));
  a.publishOps(track(2), Uint8Array.of(103));
  await until(() => late.length >= 1, 'late ops');
  assert.equal(late.at(-1), 103);
  assert.ok(late.every((v, i, list) => i === 0 || v > list[i - 1]), 'ordered');
  assert.throws(() => a.publishOps('ops/zz', Uint8Array.of(1)));
  a.close();
  b.close();
});

test('publisher keeps at most 16 open record tracks', async () => {
  const fake = relay();
  const a = await open(fake, A);
  for (let i = 0; i < MAX_OPEN_RECORDS + 4; i++) a.publishOps(track(i + 1), Uint8Array.of(i));
  assert.equal(a.stats().openRecords, MAX_OPEN_RECORDS);
  a.closeOps(track(MAX_OPEN_RECORDS + 4));
  assert.equal(a.stats().openRecords, MAX_OPEN_RECORDS - 1);
  a.close();
});

test('subscribe-only sessions publish nothing', async () => {
  const fake = relay();
  const viewer = await open(fake, A, {publish: false, subscribe: true});
  assert.equal(viewer.publishPresence(Uint8Array.of(1)), false);
  assert.equal(viewer.publishOps(track(1), Uint8Array.of(1)), false);
  assert.equal(fake.counters.published, 0);
  const publisherOnly = await open(fake, B, {publish: true, subscribe: false});
  assert.throws(() => publisherOnly.watchMembers({onMember() {}}), /not authorized/);
  viewer.close();
  publisherOnly.close();
});

test('relay loss closes the session', async () => {
  const fake = relay();
  const a = await open(fake, A);
  let ended = false;
  void a.closed.then(() => { ended = true; });
  fake.dropAll();
  await until(() => ended, 'closed');
  assert.equal(a.publishPresence(Uint8Array.of(1)), false);
});

test('a reset publisher stream (revocation / egress withdrawn) notifies the subscriber', async () => {
  const fake = relay();
  const a = await open(fake, A);
  const b = await open(fake, B);
  let presenceEnded = 0;
  let opsEnded = 0;
  b.subscribePresence(A, () => {}, () => presenceEnded++);
  b.subscribeOps(A, track(7), () => {}, () => opsEnded++);
  a.publishOps(track(7), Uint8Array.of(1));
  await tick();
  // The relay aborts only this publisher's egress tracks.
  a.close();
  await until(() => presenceEnded === 1 && opsEnded === 1, 'end notifications');
  b.close();
});

test('tracks requested on demand do not count against the open-record limit until written', async () => {
  const fake = relay();
  const a = await open(fake, A);
  const b = await open(fake, B);
  for (let i = 0; i < 20; i++) b.subscribeOps(A, track(100 + i), () => {});
  await until(() => a.stats().servedRecords === 20, 'served on request');
  assert.equal(a.stats().openRecords, 0);
  const got = [];
  b.subscribeOps(A, track(200), bytes => got.push(bytes[0]));
  await until(() => a.stats().servedRecords === 21, 'served');
  a.publishOps(track(200), Uint8Array.of(42));
  await until(() => got.length === 1, 'delivered on the served track');
  assert.deepEqual([a.stats().openRecords, a.stats().servedRecords], [1, 20]);
  a.close();
  b.close();
});

test('ops subscriptions tolerate empty groups and keep every one-frame group of a burst in order (latency_max >= 2 s)', async () => {
  const fake = relay();
  const b = await open(fake, B);
  // A host-fed author namespace written directly: one-frame groups plus the
  // relay's empty expiry groups.
  const url = new URL(ENDPOINT);
  const host = await fake.library.Connection.connect(url, {authorization: new TextEncoder().encode(A), webtransport: {protocols: ['moqt-21']}});
  const broadcast = new moq.Broadcast.Producer();
  const ops = broadcast.createTrack(track(9), {timescale: moq.Time.Timescale.MICRO, ordered: true}, {maxGroups: 512, maxFrames: 1024});
  host.publish(`tilo-live/${ROOM}/${A}`, broadcast);
  const options = [];
  fake.onOpsSubscribe(`tilo-live/${ROOM}/${A}`, (name, value) => options.push(value));
  const got = [];
  let ended = 0;
  b.subscribeOps(A, track(9), bytes => got.push(bytes[0]), () => ended++);
  await tick();
  assert.ok(options[0].latencyMax >= 2000, `latencyMax ${options[0].latencyMax}`);
  assert.equal(options[0].ordered, true);
  const ts = () => new moq.Time.Timestamp(1, moq.Time.Timescale.MICRO);
  const empty = () => { const group = ops.appendGroup(); group.close(); };
  empty();
  for (let i = 0; i < 100; i++) {
    const group = ops.appendGroup();
    group.writeFrame({payload: Uint8Array.of(i), timestamp: ts()});
    group.close();
    if (i % 10 === 9) empty();
  }
  empty();
  await until(() => got.length === 100, 'burst delivered');
  assert.deepEqual(got, [...Array(100).keys()]);
  assert.equal(ended, 0, 'empty groups do not end the subscription');
  host.close();
  b.close();
});

test('a withdrawn member broadcast ends datagram-only subscriptions and a fresh announcement is re-emitted', async () => {
  const fake = relay();
  const a = await open(fake, A);
  const b = await open(fake, B);
  const members = [];
  b.watchMembers({onMember: (id, active) => members.push(active)});
  await until(() => members.length === 1, 'announced');
  let presenceEnded = 0;
  let opsEnded = 0;
  b.subscribePresence(A, () => {}, () => presenceEnded++);
  b.subscribeOps(A, track(3), () => {}, () => opsEnded++);
  await tick();
  // Relay withdraws the namespace without closing the publisher's tracks.
  fake.withdraw(`tilo-live/${ROOM}/${A}`);
  await until(() => presenceEnded === 1 && opsEnded === 1, 'ended on broadcast close');
  // Fresh announcement of the same member.
  a.close();
  const again = await open(fake, A);
  await until(() => members.length >= 3, 're-announce');
  assert.deepEqual(members.slice(-2), [false, true]);
  again.close();
  b.close();
});

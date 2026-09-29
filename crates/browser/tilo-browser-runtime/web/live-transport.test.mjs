import test from 'node:test';
import assert from 'node:assert/strict';
import {createLiveTransport, validateLiveEndpoint, MAX_OPEN_RECORDS} from './live-transport.mjs';
import {createFakeRelay} from './live-fake-relay.test.mjs';

const ROOM = 'a'.repeat(64);
const A = 'b'.repeat(64);
const B = 'c'.repeat(64);
const ENDPOINT = 'https://host.test:19343/moq/live';
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
  const transport = createLiveTransport({library: fake.library, carrier: fake.carrier});
  return transport.connect({
    endpoint: ENDPOINT,
    authorization: new TextEncoder().encode(publisher),
    namespace: ['tilo-live', ROOM, publisher],
    directions,
  });
}

test('endpoint and namespace validation', async () => {
  assert.throws(() => validateLiveEndpoint('https://host.test/moq/voice'));
  assert.throws(() => validateLiveEndpoint('http://host.test/moq/live'));
  assert.equal(validateLiveEndpoint('http://localhost:9764/moq/live').port, '9764');
  assert.throws(() => validateLiveEndpoint('https://u:p@host.test/moq/live'));
  assert.equal(validateLiveEndpoint(ENDPOINT).pathname, '/moq/live');
  const fake = relay();
  const transport = createLiveTransport({library: fake.library, carrier: fake.carrier});
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

test('ops frames arrive in order, and a record is an announced broadcast of its own', async () => {
  const fake = relay();
  const a = await open(fake, A);
  const b = await open(fake, B);
  const early = [];
  const late = [];
  a.publishOps(track(1), Uint8Array.of(0));
  await until(() => fake.records.has(`tilo-live/${ROOM}/${A}/${track(1)}`), 'record announced');
  await until(() => fake.members.get(`tilo-live/${ROOM}/${A}`)?.tracks.has(track(1)), 'record relayed');
  b.subscribeOps(A, track(1), bytes => early.push(bytes[0]));
  await tick();
  for (let i = 1; i < 50; i++) a.publishOps(track(1), Uint8Array.of(i));
  await until(() => early.length >= 49, 'early ops');
  assert.ok(early.every((v, i, list) => i === 0 || v > list[i - 1]), 'ordered');
  assert.equal(early.at(-1), 49);
  for (let i = 0; i < 3; i++) a.publishOps(track(2), Uint8Array.of(100 + i));
  await until(() => fake.members.get(`tilo-live/${ROOM}/${A}`)?.tracks.has(track(2)), 'second record relayed');
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
  await until(() => fake.records.size === MAX_OPEN_RECORDS - 1, 'closed records retracted');
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
  a.publishOps(track(7), Uint8Array.of(1));
  await until(() => fake.members.get(`tilo-live/${ROOM}/${A}`)?.tracks.has(track(7)), 'record relayed');
  b.subscribePresence(A, () => {}, () => presenceEnded++);
  b.subscribeOps(A, track(7), () => {}, () => opsEnded++);
  await tick();
  // The relay aborts only this publisher's egress tracks.
  a.close();
  await until(() => presenceEnded === 1 && opsEnded === 1, 'end notifications');
  b.close();
});

test('ops subscriptions ask for 2.5 s of reordering, skip seal frames and keep every one-frame group of a burst in order', async () => {
  const fake = relay();
  const b = await open(fake, B);
  // A host-fed author namespace written directly: one-frame groups plus the
  // relay's seal groups (one empty frame each).
  const ops = fake.hostTrack(`tilo-live/${ROOM}/${A}`, track(9));
  const got = [];
  let ended = 0;
  b.subscribeOps(A, track(9), bytes => got.push(bytes[0]), () => ended++);
  await until(() => ops.subscription.peek() !== undefined, 'subscribed');
  assert.ok(ops.subscription.peek().maxAge >= 2000, `maxAge ${ops.subscription.peek().maxAge}`);
  const ts = () => new fake.library.Time.Timestamp(1, fake.library.Time.Timescale.MICRO);
  const seal = () => { const group = ops.appendGroup(); group.writeFrame({payload: new Uint8Array(), timestamp: ts()}); group.close(); };
  seal();
  for (let i = 0; i < 100; i++) {
    const group = ops.appendGroup();
    group.writeFrame({payload: Uint8Array.of(i), timestamp: ts()});
    group.close();
    if (i % 10 === 9) seal();
  }
  seal();
  await until(() => got.length === 100, 'burst delivered');
  assert.deepEqual(got, [...Array(100).keys()]);
  assert.equal(ended, 0, 'seal groups do not end the subscription');
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
  a.publishOps(track(3), Uint8Array.of(1));
  await until(() => fake.members.get(`tilo-live/${ROOM}/${A}`)?.tracks.has(track(3)), 'record relayed');
  b.subscribePresence(A, () => {}, () => presenceEnded++);
  b.subscribeOps(A, track(3), () => {}, () => opsEnded++);
  await tick();
  // Relay withdraws the namespace.
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

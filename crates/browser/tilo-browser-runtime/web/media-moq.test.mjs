import test from 'node:test';
import assert from 'node:assert/strict';

import * as pinnedLibrary from './media-moq-library.mjs';
import {createMoqTransport} from './media-moq.mjs';
import {MOQ_PROTOCOL, requestBroadcast} from './moq-carrier.mjs';
import {decodeVoiceBundle, encodeVoiceBundle, encodeVoiceObject} from './media-voice-profile.mjs';

const CALL = 'a'.repeat(64);
const PUBLISHER = 'b'.repeat(64);
const SESSION_ID = 7;
const ENDPOINT = new URL('https://media.example.test/moq/voice');
const MICRO = pinnedLibrary.Time.Timescale.MICRO;

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return {promise, resolve, reject};
}

function manualClock({monotonic = 0, wall = 1000} = {}) {
  let monotonicMs = monotonic;
  let wallMs = wall;
  let nextTimer = 0;
  const timers = new Map();
  const runDue = () => {
    for (;;) {
      const due = [...timers.entries()]
        .filter(([, timer]) => timer.at <= monotonicMs)
        .sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      timers.delete(due[0]);
      due[1].callback();
    }
  };
  return {
    clock: {
      monotonicNow: () => monotonicMs,
      wallNow: () => wallMs,
      setTimeout(callback, delay) {
        const id = ++nextTimer;
        timers.set(id, {callback, at: monotonicMs + delay});
        return id;
      },
      clearTimeout(id) { timers.delete(id); },
    },
    advance(ms) { monotonicMs += ms; runDue(); },
    setWall(value) { wallMs = value; },
    get timers() { return timers; },
  };
}

async function waitFor(predicate) {
  for (let index = 0; index < 200; index++) {
    if (predicate()) return;
    await new Promise(resolve => setImmediate(resolve));
  }
  assert.fail('condition was not reached');
}

function descriptor(overrides = {}) {
  return {
    authorization: Uint8Array.of(1, 2, 3),
    carrierHandle: SESSION_ID,
    publishNamespace: `${CALL}/${PUBLISHER}`,
    subscribePrefix: CALL,
    subscribeNamespaces: ['1', 'd', 'e', 'f'].map(id => `${CALL}/${id.repeat(64)}`),
    directions: {publish: true, subscribe: true},
    ...overrides,
  };
}

function hashes() {
  return [{algorithm: 'sha-256', value: new Uint8Array(32).fill(7)}];
}

/**
 * A fake admitted carrier plus a `Connection.connect` that records what the
 * transport hands `@moq/net`: the origin it publishes (`publish`) and the
 * origin remote broadcasts land in (`consume`, where tests announce remote
 * publishers).
 */
function harness({connection: connectionOverrides = {}, connect, library = {}} = {}) {
  const state = {carrierCalls: [], connectCalls: [], carrierClosed: 0};
  const closed = deferred();
  const revoked = deferred();
  const connection = {
    version: MOQ_PROTOCOL,
    transport: 'webtransport',
    closed: closed.promise,
    close() { closed.resolve(null); },
    ...connectionOverrides,
  };
  const carrier = async props => {
    state.carrierCalls.push(props);
    return {transport: {admitted: true}, revoked: revoked.promise, close() { state.carrierClosed++; }};
  };
  const Connection = {
    async connect(props) {
      state.connectCalls.push(props);
      state.publish = props.publish;
      state.consume = props.consume;
      return connect ? connect(props, connection) : connection;
    },
  };
  return {
    state,
    connection,
    carrier,
    library: {...pinnedLibrary, ...library, Connection},
    revoke: () => revoked.resolve(),
    closeConnection: () => closed.resolve(null),
  };
}

function transportFor(h, options = {}) {
  return createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 60_000}),
    library: h.library,
    carrier: h.carrier,
    clock: {now: () => 0},
    ...options,
  });
}

/** A remote publisher the relay offers: `<call>/<id>` with an `audio` track. */
function announceRemote(h, id, info = {timescale: MICRO, maxAge: 1000}) {
  const broadcast = h.state.consume.createBroadcast(`${CALL}/${id}`);
  const audio = broadcast.createTrack('audio', info);
  broadcast.announce();
  return {broadcast, audio};
}

/** Subscribe to what this session publishes. */
async function published(h) {
  const {broadcast} = await requestBroadcast(h.state.publish, `${CALL}/${PUBLISHER}`);
  return broadcast;
}

test('connect presents the grant on the carrier and hands moq only the admitted transport and origins', async () => {
  const abort = new AbortController();
  const grant = {opaque: 'kept outside the transport'};
  const h = harness();
  const certificateHashes = hashes();
  const transport = transportFor(h, {
    serverCertificateHashes: certificateHashes,
    authorize: async request => {
      assert.deepEqual(request, {grant, signal: abort.signal});
      return descriptor();
    },
  });
  const session = await transport.connect({grant, signal: abort.signal});
  assert.equal(h.state.carrierCalls.length, 1);
  const [carrierCall] = h.state.carrierCalls;
  assert.equal(carrierCall.url.href, ENDPOINT.href);
  assert.deepEqual(carrierCall.grant, Uint8Array.of(1, 2, 3));
  assert.deepEqual(carrierCall.serverCertificateHashes, certificateHashes);
  assert.equal(carrierCall.signal, abort.signal);
  assert.equal(h.state.connectCalls.length, 1);
  const [call] = h.state.connectCalls;
  assert.equal(call.url.href, ENDPOINT.href);
  assert.deepEqual(call.transport, {admitted: true});
  assert.equal(call.signal, abort.signal);
  assert.equal(call.publish instanceof pinnedLibrary.Origin.Consumer, true);
  assert.equal(call.consume instanceof pinnedLibrary.Origin.Producer, true);
  assert.equal('authorization' in call, false, 'the grant never reaches moq');
  for (const name of ['publish', 'subscribe', 'refreshAuthorization', 'close']) assert.equal(typeof session[name], 'function');
  let ended = false;
  void session.closed.then(() => { ended = true; });
  session.close();
  await waitFor(() => ended);
  assert.equal(h.state.carrierClosed >= 1, true);
});

test('factory rejects endpoints with credentials or mutable URL fields and invalid pins', () => {
  const base = {
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 1}),
    library: pinnedLibrary,
  };
  for (const url of [
    'http://media.example.test/moq/voice',
    'https://user@media.example.test/moq/voice',
    'https://media.example.test/moq/voice?authorization=secret',
    'https://media.example.test/moq/voice#different',
    'https://media.example.test/moq/v21/voice',
  ]) {
    assert.throws(() => createMoqTransport({...base, url, serverCertificateHashes: hashes()}), /endpoint|https|credential/i);
  }
  for (const serverCertificateHashes of [
    [],
    [{algorithm: 'sha-512', value: new Uint8Array(32)}],
    [{algorithm: 'sha-256', value: new Uint8Array(31)}],
    [{algorithm: 'sha-256', value: '00'}],
  ]) {
    assert.throws(() => createMoqTransport({...base, url: ENDPOINT, serverCertificateHashes}), /certificate|sha-256|hash/i);
  }
});

test('WebPKI endpoints connect without local certificate hashes', async () => {
  const h = harness();
  const session = await transportFor(h, {serverCertificateHashes: undefined}).connect({grant: {}, signal: new AbortController().signal});
  assert.equal(h.state.carrierCalls[0].serverCertificateHashes, undefined);
  session.close();
});

test('connect rejects malformed host authorization before opening a transport', async () => {
  const h = harness();
  const invalid = [
    descriptor({authorization: new Uint8Array()}),
    descriptor({authorization: new Uint8Array(8193)}),
    descriptor({carrierHandle: 'A'.repeat(64)}),
    descriptor({publishNamespace: `${CALL}/short`}),
    descriptor({publishNamespace: `${CALL}/${PUBLISHER}/extra`}),
    descriptor({subscribePrefix: 'd'.repeat(64)}),
    descriptor({subscribePrefix: `${CALL}/`}),
    descriptor({directions: {publish: false, subscribe: false}}),
    descriptor({directions: {publish: true, subscribe: 'yes'}}),
  ];
  for (const authorization of invalid) {
    const transport = transportFor(h, {authorize: async () => authorization});
    await assert.rejects(transport.connect({grant: {}, signal: new AbortController().signal}), /authorization|carrier|session|namespace|prefix|direction/i);
  }
  assert.equal(h.state.carrierCalls.length, 0);
  assert.equal(h.state.connectCalls.length, 0);
});

test('connect propagates expired authorization without opening a transport', async () => {
  const h = harness();
  const expired = Object.assign(new Error('grant expired'), {code: 'expired'});
  const transport = transportFor(h, {authorize: async () => { throw expired; }});
  await assert.rejects(transport.connect({grant: {}, signal: new AbortController().signal}), error => error === expired);
  assert.equal(h.state.carrierCalls.length, 0);
});

test('connect closes sessions with a mismatched protocol version or transport', async () => {
  for (const mismatch of [
    {version: 'moq-lite-05', transport: 'webtransport'},
    {version: MOQ_PROTOCOL, transport: 'websocket'},
  ]) {
    let closed = false;
    const h = harness({connection: {...mismatch, close() { closed = true; }}});
    await assert.rejects(transportFor(h).connect({grant: {}, signal: new AbortController().signal}), /moq-lite-06|WebTransport/);
    assert.equal(closed, true);
    assert.equal(h.state.carrierClosed >= 1, true);
  }
});

test('an aborted in-flight connect closes a library result that settles late', async () => {
  const pending = deferred();
  let closed = false;
  const h = harness({connection: {close() { closed = true; }}, connect: () => pending.promise});
  const abort = new AbortController();
  const connecting = transportFor(h).connect({grant: {}, signal: abort.signal});
  await waitFor(() => h.state.connectCalls.length === 1);
  abort.abort(new Error('left call'));
  pending.resolve(h.connection);
  await assert.rejects(connecting, /left call/);
  assert.equal(closed, true);
});

test('a host revocation on the admission stream closes the session', async () => {
  const h = harness();
  const session = await transportFor(h).connect({grant: {}, signal: new AbortController().signal});
  let ended = false;
  void session.closed.then(() => { ended = true; });
  h.revoke();
  await waitFor(() => ended);
  assert.throws(() => session.publish({timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}), /closed/i);
});

test('publication writes five voice objects per reliable group on one announced audio track', async () => {
  const h = harness();
  let time = 1000;
  const session = await transportFor(h, {
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: time + 60_000}),
    clock: {monotonicNow: () => time},
    // Stream fallback (no datagram transport).
    datagrams: false,
  }).connect({grant: {}, signal: new AbortController().signal});
  const broadcast = await published(h);
  const audio = broadcast.track('audio').subscribe({groups: {start: {included: 0}}});
  assert.equal((await audio.info()).maxAge, 100);
  const received = [];
  for (let expectedGroup = 0; expectedGroup < 2; expectedGroup++) {
    for (let offset = 0; offset < 5; offset++) {
      const index = expectedGroup * 5 + offset;
      assert.equal(session.publish({timestamp: BigInt(index * 960), duration: 960, payload: Uint8Array.of(0xf8, index)}), true);
      time += 1;
    }
    const group = await audio.recvGroup();
    assert.equal(group.sequence, expectedGroup);
    let count = 0;
    for (;;) {
      const frame = await group.readFrame();
      if (!frame) break;
      count++;
      assert.equal(frame.timestamp.as(MICRO), received.length * 20_000);
      received.push(decodeFixture(frame.payload));
    }
    assert.equal(count, 5);
  }
  assert.deepEqual(received.map(frame => frame.sequence), [0n, 1n, 2n, 3n, 4n, 5n, 6n, 7n, 8n, 9n]);
  assert.deepEqual(received.map(frame => frame.timestamp), [0n, 960n, 1920n, 2880n, 3840n, 4800n, 5760n, 6720n, 7680n, 8640n]);
  audio.close();
  session.close();
});

function decodeFixture(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return {
    sequence: view.getBigUint64(4, true),
    timestamp: view.getBigUint64(12, true),
    duration: view.getUint16(20, true),
    payload: bytes.slice(32),
  };
}

test('subscription follows exact call-scoped publishers at the live edge and labels every frame', async () => {
  const remoteId = 'd'.repeat(64);
  const h = harness();
  const reports = [];
  const session = await transportFor(h, {onReport: report => reports.push(report)}).connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const ended = [];
  const unsubscribe = await session.subscribe({
    signal: new AbortController().signal,
    onFrame: frame => frames.push(frame),
    onPublisherEnded: publisherId => ended.push(publisherId),
  });
  // Not publishers of this call: other shapes and this session's own namespace.
  for (const path of [`${CALL}/${'D'.repeat(64)}`, `${CALL}/${remoteId}/extra`, `${'e'.repeat(64)}/${remoteId}`]) {
    h.state.consume.createBroadcast(path).announce();
  }
  const remote = announceRemote(h, remoteId, {timescale: 48_000, maxAge: 1000});
  await waitFor(() => remote.audio.used.peek());
  // The live edge plus 200 ms of reordering, never more.
  assert.equal(remote.audio.subscription.peek().maxAge, 200);
  const group = remote.audio.appendGroup();
  group.writeFrame({
    payload: encodeVoiceObject({sequence: 7n, timestamp: 1920n, duration: 960, payload: Uint8Array.of(9, 8)}),
    timestamp: new pinnedLibrary.Time.Timestamp(1920, pinnedLibrary.Time.Timescale(48_000)),
  });
  group.close();
  await waitFor(() => frames.length === 1);
  assert.deepEqual(frames[0], {publisherId: remoteId, sequence: 7n, timestamp: 1920n, duration: 960, payload: Uint8Array.of(9, 8)});
  remote.broadcast.close();
  await waitFor(() => ended.length === 1);
  assert.deepEqual(ended, [remoteId]);
  assert.deepEqual(reports, []);
  unsubscribe();
  session.close();
});

test('subscription admits at most seven other same-call publisher namespaces, including ones announced after admission', async () => {
  const h = harness();
  const reports = [];
  // The admitted inventory is empty: the call-scoped relay offers joiners in place.
  const session = await transportFor(h, {
    authorize: async () => descriptor({subscribeNamespaces: []}),
    onReport: report => reports.push(report),
  }).connect({grant: {}, signal: new AbortController().signal});
  const ended = [];
  const unsubscribe = await session.subscribe({onFrame() {}, onPublisherEnded: id => ended.push(id), signal: new AbortController().signal});
  const ids = Array.from({length: 8}, (_, index) => (index + 1).toString(16).padStart(64, '0'));
  const remotes = ids.map(id => announceRemote(h, id));
  await waitFor(() => remotes.filter(remote => remote.audio.used.peek()).length === 7);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(remotes.map(remote => remote.audio.used.peek()), [true, true, true, true, true, true, true, false]);
  assert.equal(reports.some(report => report.type === 'media-publisher-limit'), true);
  unsubscribe();
  assert.deepEqual(new Set(ended), new Set(ids.slice(0, 7)));
  session.close();
});

test('refresh binds the new grant to the exact live session and a host-verified short expiry', async () => {
  let time = 1000;
  const calls = [];
  const h = harness();
  const clock = {monotonicNow: () => time, wallNow: () => time};
  const session = await transportFor(h, {
    refreshAuthorization: async request => { calls.push(request); return {carrierHandle: SESSION_ID, expiresAtMs: time + 60_000}; },
    clock,
  }).connect({grant: {}, signal: new AbortController().signal});
  const grant = {renewed: true};
  const abort = new AbortController();
  assert.deepEqual(await session.refreshAuthorization({grant, signal: abort.signal}), {expiresAtMs: 61_000});
  assert.deepEqual(calls, [{grant, carrierHandle: SESSION_ID, signal: abort.signal}]);
  for (const result of [
    {carrierHandle: 'd'.repeat(64), expiresAtMs: time + 1000},
    {carrierHandle: SESSION_ID, expiresAtMs: time},
    {carrierHandle: SESSION_ID, expiresAtMs: time + 60_001},
    {carrierHandle: SESSION_ID, expiresAtMs: Number.NaN},
  ]) {
    const invalidSession = await transportFor(harness(), {refreshAuthorization: async () => result, clock}).connect({grant: {}, signal: new AbortController().signal});
    await assert.rejects(invalidSession.refreshAuthorization({grant, signal: abort.signal}), /session|expiry|expires|binding/i);
    invalidSession.close();
  }
  session.close();
});

test('refresh rejects results that settle after abort or session teardown', async () => {
  for (const stop of ['abort', 'close']) {
    const pending = deferred();
    const session = await transportFor(harness(), {
      refreshAuthorization: async () => pending.promise,
      clock: {monotonicNow: () => 1000, wallNow: () => 1000},
    }).connect({grant: {}, signal: new AbortController().signal});
    const abort = new AbortController();
    const refreshing = session.refreshAuthorization({grant: {}, signal: abort.signal});
    await Promise.resolve();
    if (stop === 'abort') abort.abort(new Error('refresh aborted'));
    else session.close();
    pending.resolve({carrierHandle: SESSION_ID, expiresAtMs: 2000});
    await assert.rejects(refreshing, /abort|closed|stale/i);
    session.close();
  }
});

test('receiver drops malformed and duplicate voice objects without poisoning the live publisher', async () => {
  const remoteId = 'e'.repeat(64);
  const h = harness();
  const reports = [];
  const session = await transportFor(h, {onReport: report => reports.push(report)}).connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const unsubscribe = await session.subscribe({onFrame: frame => frames.push(frame), onPublisherEnded() {}, signal: new AbortController().signal});
  const remote = announceRemote(h, remoteId);
  await waitFor(() => remote.audio.used.peek());
  const valid = encodeVoiceObject({sequence: 4n, timestamp: 960n, duration: 960, payload: Uint8Array.of(1)});
  const group = remote.audio.appendGroup();
  for (const payload of [Uint8Array.of(1, 2), valid, valid]) {
    group.writeFrame({payload, timestamp: new pinnedLibrary.Time.Timestamp(20_000, MICRO)});
  }
  group.close();
  await waitFor(() => reports.filter(report => report.type === 'media-receive-drop').length === 2);
  assert.equal(frames.length, 1);
  assert.equal(frames[0].publisherId, remoteId);
  assert.equal(frames[0].sequence, 4n);
  assert.deepEqual(reports.filter(report => report.type === 'media-receive-drop').map(report => report.reason), ['invalid-object', 'duplicate']);
  unsubscribe();
  session.close();
});

test('remote connection closure tears down publisher and subscriber resources', async () => {
  const remoteId = 'f'.repeat(64);
  const h = harness({connection: {close() {}}});
  const session = await transportFor(h).connect({grant: {}, signal: new AbortController().signal});
  const ended = [];
  await session.subscribe({onFrame() {}, onPublisherEnded: publisherId => ended.push(publisherId), signal: new AbortController().signal});
  const remote = announceRemote(h, remoteId);
  await waitFor(() => remote.audio.used.peek());
  h.closeConnection();
  await waitFor(() => ended.length === 1);
  assert.deepEqual(ended, [remoteId]);
  assert.throws(() => session.publish({timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}), /closed/i);
});

test('post-connect acquisition failures close every resource already acquired', async () => {
  const closed = [];
  class Producer {
    createBroadcast() {
      return {createTrack() { throw new Error('track allocation failed'); }, close() { closed.push('broadcast'); }};
    }
    consume() { return {}; }
    close() { closed.push('origin'); }
  }
  const h = harness({connection: {close() { closed.push('connection'); }}, library: {Origin: {...pinnedLibrary.Origin, Producer}}});
  await assert.rejects(transportFor(h).connect({grant: {}, signal: new AbortController().signal}), /track allocation failed/);
  assert.deepEqual(closed.filter(value => value !== 'origin'), ['broadcast', 'connection']);
  assert.equal(h.state.carrierClosed >= 1, true);
});

test('close continues through throwing media resources and always closes the connection', async () => {
  const closed = [];
  const group = {writeFrame() {}, close() { closed.push('group'); throw new Error('group close failed'); }};
  const track = {appendGroup() { return group; }, close() { closed.push('track'); throw new Error('track close failed'); }};
  class Producer {
    createBroadcast() {
      return {createTrack() { return track; }, announce() {}, close() { closed.push('broadcast'); throw new Error('broadcast close failed'); }};
    }
    consume() { return {}; }
    close() {}
  }
  const h = harness({connection: {close() { closed.push('connection'); }}, library: {Origin: {...pinnedLibrary.Origin, Producer}}});
  const session = await transportFor(h, {datagrams: false}).connect({grant: {}, signal: new AbortController().signal});
  session.publish({timestamp: 0n, duration: 960, payload: Uint8Array.of(1)});
  assert.doesNotThrow(() => session.close());
  assert.deepEqual(closed, ['group', 'track', 'broadcast', 'connection']);
});

test('publisher expires a partial group at its 100 ms deadline', async () => {
  const time = manualClock();
  const h = harness();
  const reports = [];
  const session = await transportFor(h, {
    authorize: async () => descriptor({directions: {publish: true, subscribe: false}}),
    clock: time.clock,
    datagrams: false,
    onReport: report => reports.push(report),
  }).connect({grant: {}, signal: new AbortController().signal});
  const broadcast = await published(h);
  const slow = broadcast.track('audio').subscribe({groups: {start: {included: 0}}});
  session.publish({timestamp: 0n, duration: 960, payload: Uint8Array.of(1)});
  time.advance(100);
  const expiredPartial = await slow.recvGroup();
  assert.equal(expiredPartial.frameCount <= 1, true);
  assert.equal(reports.some(report => report.type === 'media-group-expired' && report.frames === 1), true);
  expiredPartial.close();
  slow.close();
  session.close();
});

test('queue residency uses monotonic time even when wall time regresses', async () => {
  const time = manualClock({monotonic: 100, wall: 10_000});
  let monotonicReads = 0;
  const monotonicNow = time.clock.monotonicNow;
  time.clock.monotonicNow = () => { monotonicReads++; return monotonicNow(); };
  const session = await transportFor(harness(), {
    authorize: async () => descriptor({directions: {publish: true, subscribe: false}}),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 10_500}),
    clock: time.clock,
    datagrams: false,
  }).connect({grant: {}, signal: new AbortController().signal});
  assert.equal(session.publish({timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}), true);
  time.setWall(1);
  time.advance(20);
  assert.equal(session.publish({timestamp: 960n, duration: 960, payload: Uint8Array.of(2)}), true);
  assert.equal(monotonicReads > 0, true);
  session.close();
});

test('receiver skips retained backlog and abandons a stalled group at its deadline', async () => {
  const remoteId = '1'.repeat(64);
  const time = manualClock();
  const h = harness();
  const reports = [];
  const session = await transportFor(h, {
    authorize: async () => descriptor({directions: {publish: false, subscribe: true}}),
    clock: time.clock,
    onReport: report => reports.push(report),
  }).connect({grant: {}, signal: new AbortController().signal});
  const remote = announceRemote(h, remoteId);
  const old = remote.audio.appendGroup();
  old.writeFrame({payload: encodeVoiceObject({sequence: 1n, timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}), timestamp: new pinnedLibrary.Time.Timestamp(0, MICRO)});
  old.close();
  const live = remote.audio.appendGroup();
  live.writeFrame({payload: encodeVoiceObject({sequence: 2n, timestamp: 960n, duration: 960, payload: Uint8Array.of(2)}), timestamp: new pinnedLibrary.Time.Timestamp(20_000, MICRO)});
  live.close();
  const frames = [];
  const unsubscribe = await session.subscribe({onFrame: frame => frames.push(frame), onPublisherEnded() {}, signal: new AbortController().signal});
  await waitFor(() => frames.length >= 1);
  assert.equal(frames.at(-1).sequence, 2n);
  const stalled = remote.audio.appendGroup();
  await waitFor(() => time.timers.size > 0);
  const newer = remote.audio.appendGroup();
  newer.writeFrame({payload: encodeVoiceObject({sequence: 3n, timestamp: 1920n, duration: 960, payload: Uint8Array.of(3)}), timestamp: new pinnedLibrary.Time.Timestamp(40_000, MICRO)});
  newer.close();
  time.advance(200);
  await waitFor(() => frames.some(frame => frame.sequence === 3n));
  assert.deepEqual(frames.map(frame => frame.sequence).slice(-2), [2n, 3n]);
  stalled.close();
  unsubscribe();
  session.close();
});

test('exact remote inventory is mandatory sorted bounded and cannot authorize another call or self', async () => {
  const h = harness();
  for (const subscribeNamespaces of [
    undefined,
    Array(8).fill(`${CALL}/${'d'.repeat(64)}`),
    [`${CALL}/${PUBLISHER}`],
    [`${'e'.repeat(64)}/${'d'.repeat(64)}`],
    [`${CALL}/${'e'.repeat(64)}`, `${CALL}/${'d'.repeat(64)}`],
  ]) {
    await assert.rejects(transportFor(h, {authorize: async () => descriptor({subscribeNamespaces})}).connect({grant: {}}));
  }
  assert.equal(h.state.carrierCalls.length, 0);
});

test('datagram publication sends each object once as primary and again as redundancy', async () => {
  const h = harness();
  let time = 0;
  const session = await transportFor(h, {
    authorize: async () => descriptor({directions: {publish: true, subscribe: false}}),
    clock: {monotonicNow: () => time},
  }).connect({grant: {}, signal: new AbortController().signal});
  const audio = (await published(h)).track('audio').subscribe({groups: {start: {included: 0}}});
  await audio.info();
  const bundles = [];
  for (let index = 0; index < 3; index++) {
    assert.equal(session.publish({timestamp: BigInt(index * 960), duration: 960, payload: Uint8Array.of(0xf8, index)}), true);
    time += 20;
    const datagram = await audio.recvDatagram();
    bundles.push(decodeVoiceBundle(datagram.payload).map(object => object.sequence));
  }
  assert.deepEqual(bundles, [[0n], [1n, 0n], [2n, 1n]]);
  assert.equal(session.stats().mode, 'datagrams');
  assert.equal(session.stats().sentDatagrams, 3);
  audio.close();
  session.close();
});

async function datagramReceiver(options = {}) {
  const remoteId = '1'.repeat(64);
  const h = harness();
  const session = await transportFor(h, {authorize: async () => descriptor({directions: {publish: false, subscribe: true}}), ...options}).connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const unsubscribe = await session.subscribe({onFrame: frame => frames.push(frame), onPublisherEnded() {}, signal: new AbortController().signal});
  const remote = announceRemote(h, remoteId);
  const object = sequence => encodeVoiceObject({sequence, timestamp: sequence * 960n, duration: 960, payload: Uint8Array.of(Number(sequence % 200n) + 1)});
  const send = (...sequences) => remote.audio.appendDatagram(new pinnedLibrary.Time.Timestamp(0, MICRO), encodeVoiceBundle(sequences.map(object)));
  // Wait for the subscription to attach before sending best-effort datagrams.
  for (let attempt = 0; attempt < 50 && frames.length === 0; attempt++) {
    send(0n);
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  return {h, session, frames, unsubscribe, send};
}

test('datagram receiver repairs a lost object from redundancy and ignores repeats', async () => {
  const {session, frames, unsubscribe, send} = await datagramReceiver();
  assert.equal(frames[0].sequence, 0n);
  // Datagram carrying 1 is lost; 2 arrives with 1 as redundancy, then 3 with 2.
  send(2n, 1n);
  send(3n, 2n);
  await waitFor(() => frames.some(frame => frame.sequence === 3n));
  assert.deepEqual(frames.map(frame => frame.sequence).filter((value, index, all) => all.indexOf(value) === index), [0n, 1n, 2n, 3n]);
  const stats = session.stats();
  assert.equal(stats.recoveredObjects >= 1, true);
  assert.equal(stats.duplicateObjects >= 1, true);
  assert.equal(stats.missingObjects, 0);
  unsubscribe();
  session.close();
});

test('sustained receive-path loss raises the datagram redundancy depth', async () => {
  let time = 0;
  const remoteId = '1'.repeat(64);
  const h = harness();
  const session = await transportFor(h, {clock: {monotonicNow: () => time}}).connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const unsubscribe = await session.subscribe({onFrame: frame => frames.push(frame), onPublisherEnded() {}, signal: new AbortController().signal});
  const remote = announceRemote(h, remoteId);
  const object = sequence => encodeVoiceObject({sequence, timestamp: sequence * 960n, duration: 960, payload: Uint8Array.of(1)});
  const send = (...sequences) => remote.audio.appendDatagram(new pinnedLibrary.Time.Timestamp(0, MICRO), encodeVoiceBundle(sequences.map(object)));
  for (let attempt = 0; attempt < 50 && frames.length === 0; attempt++) {
    send(0n);
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  // 10% of primaries lost (every tenth datagram missing).
  for (let sequence = 1n; sequence <= 60n; sequence++) {
    if (sequence % 10n !== 0n) send(sequence, sequence - 1n);
    time += 20;
  }
  await waitFor(() => frames.some(frame => frame.sequence === 59n));
  time += 2_100;
  const audio = (await published(h)).track('audio').subscribe({groups: {start: {included: 0}}});
  await audio.info();
  session.publish({timestamp: 0n, duration: 960, payload: Uint8Array.of(0xf8)});
  await audio.recvDatagram();
  time += 20;
  session.publish({timestamp: 960n, duration: 960, payload: Uint8Array.of(0xf8)});
  await audio.recvDatagram();
  time += 20;
  session.publish({timestamp: 1920n, duration: 960, payload: Uint8Array.of(0xf8)});
  const third = decodeVoiceBundle((await audio.recvDatagram()).payload).map(value => value.sequence);
  assert.deepEqual(third, [2n, 1n, 0n], 'depth 2 after sustained loss');
  assert.equal(session.stats().redundancy, 2);
  assert.ok(session.stats().receiveLoss >= 0.05, `receive loss ${session.stats().receiveLoss}`);
  audio.close();
  unsubscribe();
  session.close();
});

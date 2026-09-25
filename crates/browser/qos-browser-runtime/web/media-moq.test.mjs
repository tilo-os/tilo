import test from 'node:test';
import assert from 'node:assert/strict';

import * as pinnedLibrary from './media-moq-library.mjs';
import {createMoqTransport} from './media-moq.mjs';
import {decodeVoiceBundle, encodeVoiceBundle, encodeVoiceObject} from './media-voice-profile.mjs';

const CALL = 'a'.repeat(64);
const PUBLISHER = 'b'.repeat(64);
const SESSION_ID = 7;
const ENDPOINT = new URL('https://media.example.test/moq/v21/voice');

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
  for (let index = 0; index < 100; index++) {
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
    subscribeNamespaces: ['1','d','e','f'].map(id=>`${CALL}/${id.repeat(64)}`),
    directions: {publish: true, subscribe: true},
    ...overrides,
  };
}

function hashes() {
  return [{algorithm: 'sha-256', value: new Uint8Array(32).fill(7)}];
}

function established(overrides = {}) {
  const closed = deferred();
  return {
    version: 'moq-transport-21',
    transport: 'webtransport',
    closed: closed.promise,
    publish() {},
    announced() { return new pinnedLibrary.Announce.Producer(CALL).consume(); },
    consume() { return new pinnedLibrary.Broadcast.Producer().consume(); },
    close() { closed.resolve(null); },
    ...overrides,
  };
}

test('connect uses only the authorized Draft 21 WebTransport endpoint and certificate pins', async () => {
  const abort = new AbortController();
  const grant = {opaque: 'kept outside the transport'};
  const certificateHashes = hashes();
  const calls = [];
  const connection = established();
  const library = {
    ...pinnedLibrary,
    Connection: {
      async connect(url, options) {
        calls.push({url, options});
        return connection;
      },
    },
  };
  const authorize = async request => {
    assert.deepEqual(request, {grant, signal: abort.signal});
    return descriptor();
  };
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: certificateHashes,
    authorize,
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 1}),
    library,
    clock: {now: () => 0},
  });

  const session = await transport.connect({grant, signal: abort.signal});

  assert.equal(calls.length, 1);
  assert.equal(calls[0].url.href, ENDPOINT.href);
  assert.deepEqual(calls[0].options, {
    authorization: Uint8Array.of(1, 2, 3),
    websocket: {enabled: false},
    webtransport: {
      protocols: ['moqt-21'],
      serverCertificateHashes: certificateHashes,
    },
    signal: abort.signal,
  });
  assert.equal(typeof session.publish, 'function');
  assert.equal(typeof session.subscribe, 'function');
  assert.equal(typeof session.refreshAuthorization, 'function');
  assert.equal(typeof session.close, 'function');
  assert.equal(session.closed, connection.closed);
  session.close();
});

test('factory rejects endpoints with credentials or mutable URL fields and invalid pins', () => {
  const base = {
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 1}),
    library: pinnedLibrary,
  };
  for (const url of [
    'http://media.example.test/moq',
    'https://user@media.example.test/moq',
    'https://media.example.test/moq?authorization=secret',
    'https://media.example.test/moq#different',
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
  let options;
  const transport = createMoqTransport({
    url: ENDPOINT,
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: Date.now() + 1000}),
    library: {
      ...pinnedLibrary,
      Connection: {async connect(_url, value) { options = value; return established(); }},
    },
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  assert.deepEqual(options.webtransport, {protocols: ['moqt-21']});
  session.close();
});

test('connect rejects malformed host authorization before opening a transport', async () => {
  let connectCalls = 0;
  const library = {
    ...pinnedLibrary,
    Connection: {async connect() { connectCalls++; return established(); }},
  };
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
    const transport = createMoqTransport({
      url: ENDPOINT,
      serverCertificateHashes: hashes(),
      authorize: async () => authorization,
      refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 1}),
      library,
    });
    await assert.rejects(transport.connect({grant: {}, signal: new AbortController().signal}), /authorization|carrier|session|namespace|prefix|direction/i);
  }
  assert.equal(connectCalls, 0);
});

test('connect propagates expired authorization without opening a transport', async () => {
  let connectCalls = 0;
  const expired = Object.assign(new Error('grant expired'), {code: 'expired'});
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => { throw expired; },
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 1}),
    library: {...pinnedLibrary, Connection: {async connect() { connectCalls++; }}},
  });
  await assert.rejects(transport.connect({grant: {}, signal: new AbortController().signal}), error => error === expired);
  assert.equal(connectCalls, 0);
});

test('connect closes sessions with a mismatched protocol version or transport', async () => {
  for (const mismatch of [
    {version: 'moq-transport-20', transport: 'webtransport'},
    {version: 'moq-transport-21', transport: 'websocket'},
  ]) {
    let closed = false;
    const connection = established({...mismatch, close() { closed = true; }});
    const transport = createMoqTransport({
      url: ENDPOINT,
      serverCertificateHashes: hashes(),
      authorize: async () => descriptor(),
      refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 1}),
      library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    });
    await assert.rejects(transport.connect({grant: {}, signal: new AbortController().signal}), /moq-transport-21|WebTransport/);
    assert.equal(closed, true);
  }
});

test('an aborted in-flight connect closes a library result that settles late', async () => {
  const pending = deferred();
  let closed = false;
  const connection = established({close() { closed = true; }});
  const abort = new AbortController();
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 1}),
    library: {...pinnedLibrary, Connection: {connect() { return pending.promise; }}},
  });
  const connecting = transport.connect({grant: {}, signal: abort.signal});
  await Promise.resolve();
  abort.abort(new Error('left call'));
  pending.resolve(connection);
  await assert.rejects(connecting, /left call/);
  assert.equal(closed, true);
});

test('publication writes five voice objects per reliable group on one audio track', async () => {
  let published;
  const connection = established({
    publish(path, broadcast) { published = {path, broadcast}; },
  });
  let time = 1000;
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: time + 60_000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: {monotonicNow: () => time},
    // Stream fallback (no datagram transport).
    datagrams: false,
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  assert.equal(published.path, `${CALL}/${PUBLISHER}`);

  const broadcast = published.broadcast.consume();
  const audio = broadcast.subscribe('audio', {startGroup: 0});
  const received = [];
  for (let expectedGroup = 0; expectedGroup < 2; expectedGroup++) {
    for (let offset = 0; offset < 5; offset++) {
      const index = expectedGroup * 5 + offset;
      assert.equal(session.publish({
        timestamp: BigInt(index * 960),
        duration: 960,
        payload: Uint8Array.of(0xf8, index),
      }), true);
      time += 1;
    }
    const group = await audio.recvGroup();
    assert.equal(group.sequence, expectedGroup);
    let count = 0;
    for (;;) {
      const frame = await group.readFrame();
      if (!frame) break;
      count++;
      assert.equal(frame.timestamp.scale, 1_000_000);
      assert.equal(frame.timestamp.value, received.length * 20_000);
      received.push(decodeFixture(frame.payload));
    }
    assert.equal(count, 5);
  }
  assert.deepEqual(received.map(frame => frame.sequence), [0n, 1n, 2n, 3n, 4n, 5n, 6n, 7n, 8n, 9n]);
  assert.deepEqual(received.map(frame => frame.timestamp), [0n, 960n, 1920n, 2880n, 3840n, 4800n, 5760n, 6720n, 7680n, 8640n]);
  audio.close();
  broadcast.close();
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
  const announcements = new pinnedLibrary.Announce.Producer(CALL);
  const remote = new pinnedLibrary.Broadcast.Producer();
  const remoteAudio = remote.createTrack('audio', {timescale: 48_000, latencyMax: 100, ordered: true});
  const consumed = [];
  const connection = established({
    announced(prefix) {
      assert.equal(prefix, CALL);
      return announcements.consume();
    },
    consume(path) {
      consumed.push(path);
      assert.equal(path, `${CALL}/${remoteId}`);
      return remote.consume();
    },
  });
  const reports = [];
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 60_000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: {now: () => 0},
    onReport: report => reports.push(report),
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const ended = [];
  const subscriptionAbort = new AbortController();
  const unsubscribe = await session.subscribe({
    signal: subscriptionAbort.signal,
    onFrame: frame => frames.push(frame),
    onPublisherEnded: publisherId => ended.push(publisherId),
  });

  for (const path of ['', 'D'.repeat(64), `${remoteId}/extra`, PUBLISHER]) {
    announcements.append({path, active: true});
  }
  announcements.append({path: remoteId, active: true});
  await waitFor(() => remoteAudio.used.peek());
  assert.deepEqual(consumed, [`${CALL}/${remoteId}`]);
  const options = remoteAudio.subscription.peek();
  assert.equal(options.startGroup, undefined);
  assert.equal(options.ordered, false);
  assert.equal(options.latencyMax, 200);

  const group = remoteAudio.appendGroup();
  group.writeFrame({
    payload: encodeVoiceObject({sequence: 7n, timestamp: 1920n, duration: 960, payload: Uint8Array.of(9, 8)}),
    timestamp: new pinnedLibrary.Time.Timestamp(1920, pinnedLibrary.Time.Timescale(48_000)),
  });
  group.close();
  await waitFor(() => frames.length === 1);
  assert.deepEqual(frames[0], {
    publisherId: remoteId,
    sequence: 7n,
    timestamp: 1920n,
    duration: 960,
    payload: Uint8Array.of(9, 8),
  });

  announcements.append({path: remoteId, active: false});
  await waitFor(() => ended.length === 1);
  assert.deepEqual(ended, [remoteId]);
  assert.deepEqual(reports, []);
  unsubscribe();
  session.close();
});

test('subscription admits at most seven other same-call publisher namespaces, including ones announced after admission', async () => {
  const announcements = new pinnedLibrary.Announce.Producer(CALL);
  const consumed = [];
  const remotes = new Map();
  const connection = established({
    announced() { return announcements.consume(); },
    consume(path) {
      consumed.push(path);
      const remote = new pinnedLibrary.Broadcast.Producer();
      remote.createTrack('audio', {timescale: 48_000});
      remotes.set(path, remote);
      return remote.consume();
    },
  });
  const reports = [];
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    // The admitted inventory is empty: the call-scoped relay offers joiners in place.
    authorize: async () => descriptor({subscribeNamespaces:[]}),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 60_000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: {now: () => 0},
    onReport: report => reports.push(report),
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const ended = [];
  const unsubscribe = await session.subscribe({
    onFrame() {},
    onPublisherEnded: id => ended.push(id),
    signal: new AbortController().signal,
  });
  const ids = Array.from({length: 8}, (_, index) => (index + 1).toString(16).padStart(64, '0'));
  for (const path of ids) announcements.append({path, active: true});
  await waitFor(() => consumed.length === 7);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(consumed, ids.slice(0, 7).map(id => `${CALL}/${id}`));
  assert.equal(reports.some(report => report.type === 'media-publisher-limit'), true);
  unsubscribe();
  assert.deepEqual(new Set(ended), new Set(ids.slice(0, 7)));
  session.close();
});

test('refresh binds the new grant to the exact live session and a host-verified short expiry', async () => {
  let time = 1000;
  const calls = [];
  const refresh = async request => {
    calls.push(request);
    return {carrierHandle: SESSION_ID, expiresAtMs: time + 60_000};
  };
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: refresh,
    library: {...pinnedLibrary, Connection: {async connect() { return established(); }}},
    clock: {monotonicNow: () => time, wallNow: () => time},
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
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
    calls.length = 0;
    const invalidTransport = createMoqTransport({
      url: ENDPOINT,
      serverCertificateHashes: hashes(),
      authorize: async () => descriptor(),
      refreshAuthorization: async request => { calls.push(request); return result; },
      library: {...pinnedLibrary, Connection: {async connect() { return established(); }}},
      clock: {monotonicNow: () => time, wallNow: () => time},
    });
    const invalidSession = await invalidTransport.connect({grant: {}, signal: new AbortController().signal});
    await assert.rejects(
      invalidSession.refreshAuthorization({grant, signal: abort.signal}),
      /session|expiry|expires|binding/i,
    );
    invalidSession.close();
  }
  session.close();
});

test('refresh rejects results that settle after abort or session teardown', async () => {
  for (const stop of ['abort', 'close']) {
    const pending = deferred();
    const connection = established();
    const transport = createMoqTransport({
      url: ENDPOINT,
      serverCertificateHashes: hashes(),
      authorize: async () => descriptor(),
      refreshAuthorization: async () => pending.promise,
      library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
      clock: {monotonicNow: () => 1000, wallNow: () => 1000},
    });
    const session = await transport.connect({grant: {}, signal: new AbortController().signal});
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
  const announcements = new pinnedLibrary.Announce.Producer(CALL);
  const remote = new pinnedLibrary.Broadcast.Producer();
  const remoteAudio = remote.createTrack('audio', {timescale: pinnedLibrary.Time.Timescale.MICRO});
  const connection = established({
    announced() { return announcements.consume(); },
    consume() { return remote.consume(); },
  });
  const reports = [];
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 60_000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: {now: () => 0},
    onReport: report => reports.push(report),
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const unsubscribe = await session.subscribe({
    onFrame: frame => frames.push(frame),
    onPublisherEnded() {},
    signal: new AbortController().signal,
  });
  announcements.append({path: remoteId, active: true});
  await waitFor(() => remoteAudio.used.peek());

  const valid = encodeVoiceObject({sequence: 4n, timestamp: 960n, duration: 960, payload: Uint8Array.of(1)});
  const group = remoteAudio.appendGroup();
  for (const payload of [Uint8Array.of(1, 2), valid, valid]) {
    group.writeFrame({payload, timestamp: new pinnedLibrary.Time.Timestamp(20_000, pinnedLibrary.Time.Timescale.MICRO)});
  }
  group.close();
  await waitFor(() => reports.filter(report => report.type === 'media-receive-drop').length === 2);
  assert.equal(frames.length, 1);
  assert.equal(frames[0].publisherId, remoteId);
  assert.equal(frames[0].sequence, 4n);
  assert.deepEqual(
    reports.filter(report => report.type === 'media-receive-drop').map(report => report.reason),
    ['invalid-object', 'duplicate'],
  );
  unsubscribe();
  session.close();
});

test('remote connection closure tears down publisher and subscriber resources', async () => {
  const remoteId = 'f'.repeat(64);
  const connectionClosed = deferred();
  const announcements = new pinnedLibrary.Announce.Producer(CALL);
  const remote = new pinnedLibrary.Broadcast.Producer();
  const remoteAudio = remote.createTrack('audio', {timescale: pinnedLibrary.Time.Timescale.MICRO});
  const connection = established({
    closed: connectionClosed.promise,
    announced() { return announcements.consume(); },
    consume() { return remote.consume(); },
    close() {},
  });
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 60_000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: {now: () => 0},
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const ended = [];
  await session.subscribe({
    onFrame() {},
    onPublisherEnded: publisherId => ended.push(publisherId),
    signal: new AbortController().signal,
  });
  announcements.append({path: remoteId, active: true});
  await waitFor(() => remoteAudio.used.peek());

  connectionClosed.resolve(null);
  await waitFor(() => ended.length === 1);
  assert.deepEqual(ended, [remoteId]);
  assert.throws(
    () => session.publish({timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}),
    /closed/i,
  );
});

test('post-connect acquisition failures close every resource already acquired', async () => {
  const closed = [];
  class Producer {
    createTrack() { throw new Error('track allocation failed'); }
    close() { closed.push('broadcast'); }
  }
  const connection = established({close() { closed.push('connection'); }});
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: Date.now() + 1000}),
    library: {
      ...pinnedLibrary,
      Broadcast: {Producer},
      Connection: {async connect() { return connection; }},
    },
  });
  await assert.rejects(
    transport.connect({grant: {}, signal: new AbortController().signal}),
    /track allocation failed/,
  );
  assert.deepEqual(closed, ['broadcast', 'connection']);
});

test('close continues through throwing media resources and always closes the connection', async () => {
  const closed = [];
  const group = {
    writeFrame() {},
    close() { closed.push('group'); throw new Error('group close failed'); },
  };
  const track = {
    appendGroup() { return group; },
    close() { closed.push('track'); throw new Error('track close failed'); },
  };
  class Producer {
    createTrack() { return track; }
    close() { closed.push('broadcast'); throw new Error('broadcast close failed'); }
  }
  const connection = established({
    publish() {},
    close() { closed.push('connection'); },
  });
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: Date.now() + 1000}),
    library: {
      ...pinnedLibrary,
      Broadcast: {Producer},
      Connection: {async connect() { return connection; }},
    },
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  session.publish({timestamp: 0n, duration: 960, payload: Uint8Array.of(1)});
  assert.doesNotThrow(() => session.close());
  assert.deepEqual(closed, ['group', 'track', 'broadcast', 'connection']);
});

test('publisher expires a partial group and real Track retention bounds a stalled consumer', async () => {
  const time = manualClock();
  let published;
  const connection = established({publish(_path, broadcast) { published = broadcast; }});
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor({directions: {publish: true, subscribe: false}}),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 2000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: time.clock,
    datagrams: false,
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const broadcast = published.consume();
  const slow = broadcast.track('audio').subscribe({startGroup: 0});
  const info = await slow.info();
  assert.equal(info.latencyMax, 100);

  session.publish({timestamp: 0n, duration: 960, payload: Uint8Array.of(1)});
  time.advance(100);
  const expiredPartial = await slow.recvGroup();
  assert.equal(expiredPartial.isClosed, true);
  expiredPartial.close();

  const originalDateNow = Date.now;
  Date.now = time.clock.monotonicNow;
  try {
    for (let index = 1; index <= 15; index++) {
      session.publish({timestamp: BigInt(index * 960), duration: 960, payload: Uint8Array.of(index)});
      time.advance(20);
    }
    let retained = 0;
    while (slow.tryRecvGroup()) retained++;
    assert.equal(retained <= 1, true, `slow consumer retained ${retained} groups`);

    for (let index = 16; index <= 65; index++) {
      session.publish({timestamp: BigInt(index * 960), duration: 960, payload: Uint8Array.of(index)});
    }
    retained = 0;
    while (slow.tryRecvGroup()) retained++;
    assert.equal(retained <= 1, true, `rapid producer retained ${retained} groups`);
  } finally {
    Date.now = originalDateNow;
  }
  slow.close();
  broadcast.close();
  session.close();
});

test('completed producer groups expire while held by an idle stalled consumer', async () => {
  let published;
  const connection = established({publish(_path, broadcast) { published = broadcast; }});
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor({directions: {publish: true, subscribe: false}}),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: Date.now() + 1000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    datagrams: false,
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const broadcast = published.consume();
  const stalled = broadcast.track('audio').subscribe({startGroup: 0});

  for (let index = 0; index < 5; index++) {
    session.publish({
      timestamp: BigInt(index * 960),
      duration: 960,
      payload: Uint8Array.of(index),
    });
  }
  const held = await stalled.recvGroup();
  assert.equal(held.bufferedFrames, 5);
  await new Promise(resolve => setTimeout(resolve, 160));
  assert.equal(held.bufferedFrames, 0);
  assert.equal(await held.readFrame(), undefined);

  held.close();
  stalled.close();
  broadcast.close();
  session.close();
});

test('queue residency uses monotonic time even when wall time regresses', async () => {
  const time = manualClock({monotonic: 100, wall: 10_000});
  let monotonicReads = 0;
  const monotonicNow = time.clock.monotonicNow;
  time.clock.monotonicNow = () => { monotonicReads++; return monotonicNow(); };
  const connection = established();
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor({directions: {publish: true, subscribe: false}}),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 10_500}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: time.clock,
    datagrams: false,
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
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
  const announcements = new pinnedLibrary.Announce.Producer(CALL);
  const remote = new pinnedLibrary.Broadcast.Producer();
  const remoteAudio = remote.createTrack('audio', {timescale: pinnedLibrary.Time.Timescale.MICRO, latencyMax: 1000});
  const old = remoteAudio.appendGroup();
  old.writeFrame({
    payload: encodeVoiceObject({sequence: 1n, timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}),
    timestamp: new pinnedLibrary.Time.Timestamp(0, pinnedLibrary.Time.Timescale.MICRO),
  });
  old.close();
  const live = remoteAudio.appendGroup();
  live.writeFrame({
    payload: encodeVoiceObject({sequence: 2n, timestamp: 960n, duration: 960, payload: Uint8Array.of(2)}),
    timestamp: new pinnedLibrary.Time.Timestamp(20_000, pinnedLibrary.Time.Timescale.MICRO),
  });
  live.close();
  const connection = established({
    announced() { return announcements.consume(); },
    consume() { return remote.consume(); },
  });
  const reports = [];
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor({directions: {publish: false, subscribe: true}}),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 2000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: time.clock,
    onReport: report => reports.push(report),
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const unsubscribe = await session.subscribe({
    onFrame: frame => frames.push(frame),
    onPublisherEnded() {},
    signal: new AbortController().signal,
  });
  announcements.append({path: remoteId, active: true});
  await waitFor(() => frames.length === 1);
  assert.deepEqual(frames.map(frame => frame.sequence), [2n]);

  const stalled = remoteAudio.appendGroup();
  await waitFor(() => time.timers.size > 0);
  const newer = remoteAudio.appendGroup();
  newer.writeFrame({
    payload: encodeVoiceObject({sequence: 3n, timestamp: 1920n, duration: 960, payload: Uint8Array.of(3)}),
    timestamp: new pinnedLibrary.Time.Timestamp(40_000, pinnedLibrary.Time.Timescale.MICRO),
  });
  newer.close();
  time.advance(200);
  await waitFor(() => frames.length === 2);
  assert.deepEqual(frames.map(frame => frame.sequence), [2n, 3n]);
  assert.equal(reports.some(report => report.reason === 'group-deadline'), true);
  stalled.close();
  unsubscribe();
  session.close();
});

test('subscriber mirrors do not refresh an object age when old backing data is dequeued', async () => {
  const remoteId = '1'.repeat(64);
  const announcements = new pinnedLibrary.Announce.Producer(CALL);
  const remote = new pinnedLibrary.Broadcast.Producer();
  const remoteAudio = remote.createTrack('audio', {
    timescale: pinnedLibrary.Time.Timescale.MICRO,
    latencyMax: 1000,
  });
  const old = remoteAudio.appendGroup();
  old.writeFrame({
    payload: encodeVoiceObject({sequence: 1n, timestamp: 0n, duration: 960, payload: Uint8Array.of(1)}),
    timestamp: new pinnedLibrary.Time.Timestamp(0, pinnedLibrary.Time.Timescale.MICRO),
  });
  old.close();
  await new Promise(resolve => setTimeout(resolve, 240));

  const connection = established({
    announced() { return announcements.consume(); },
    consume() { return remote.consume(); },
  });
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor({directions: {publish: false, subscribe: true}}),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: Date.now() + 1000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const ended = [];
  const unsubscribe = await session.subscribe({
    onFrame: frame => frames.push(frame),
    onPublisherEnded: publisherId => ended.push(publisherId),
    signal: new AbortController().signal,
  });
  announcements.append({path: remoteId, active: true});
  await waitFor(() => remoteAudio.used.peek());
  remoteAudio.close();
  await waitFor(() => ended.length === 1);
  assert.deepEqual(frames, []);

  unsubscribe();
  session.close();
});


test('exact remote inventory is mandatory sorted bounded and cannot authorize another call or self',async()=>{
 let opens=0;
 for(const subscribeNamespaces of [undefined,Array(8).fill(`${CALL}/${'d'.repeat(64)}`),[`${CALL}/${PUBLISHER}`],[`${'e'.repeat(64)}/${'d'.repeat(64)}`],[`${CALL}/${'e'.repeat(64)}`,`${CALL}/${'d'.repeat(64)}`]]){
  const transport=createMoqTransport({url:ENDPOINT,authorize:async()=>descriptor({subscribeNamespaces}),refreshAuthorization:async()=>({carrierHandle:SESSION_ID,expiresAtMs:1}),library:{...pinnedLibrary,Connection:{async connect(){opens++;return established();}}}});
  await assert.rejects(transport.connect({grant:{}}));
 }
 assert.equal(opens,0);
});

test('datagram publication sends each object once as primary and again as redundancy', async () => {
  let published;
  const connection = established({publish(_path, broadcast) { published = broadcast; }});
  let time = 0;
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor({directions: {publish: true, subscribe: false}}),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 2000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: {monotonicNow: () => time},
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const audio = published.consume().track('audio').subscribe({startGroup: 0});
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

test('datagram receiver repairs a lost object from redundancy and ignores repeats', async () => {
  const remoteId = '1'.repeat(64);
  const announcements = new pinnedLibrary.Announce.Producer(CALL);
  const remote = new pinnedLibrary.Broadcast.Producer();
  const remoteAudio = remote.createTrack('audio', {timescale: pinnedLibrary.Time.Timescale.MICRO, latencyMax: 1000});
  const connection = established({
    announced() { return announcements.consume(); },
    consume() { return remote.consume(); },
  });
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor({directions: {publish: false, subscribe: true}}),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 2000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const unsubscribe = await session.subscribe({
    onFrame: frame => frames.push(frame),
    onPublisherEnded() {},
    signal: new AbortController().signal,
  });
  announcements.append({path: remoteId, active: true});
  const object = sequence => encodeVoiceObject({sequence, timestamp: sequence * 960n, duration: 960, payload: Uint8Array.of(Number(sequence) + 1)});
  const send = (...sequences) => remoteAudio.appendDatagram(
    new pinnedLibrary.Time.Timestamp(0, pinnedLibrary.Time.Timescale.MICRO),
    encodeVoiceBundle(sequences.map(object)),
  );
  // Wait for the subscription to attach before sending best-effort datagrams.
  for (let attempt = 0; attempt < 50 && frames.length === 0; attempt++) {
    send(0n);
    await new Promise(resolve => setTimeout(resolve, 5));
  }
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
  const remoteId = '1'.repeat(64);
  const announcements = new pinnedLibrary.Announce.Producer(CALL);
  const remote = new pinnedLibrary.Broadcast.Producer();
  const remoteAudio = remote.createTrack('audio', {timescale: pinnedLibrary.Time.Timescale.MICRO, latencyMax: 1000});
  let published;
  const connection = established({
    announced() { return announcements.consume(); },
    consume() { return remote.consume(); },
    publish(_path, broadcast) { published = broadcast; },
  });
  let time = 0;
  const transport = createMoqTransport({
    url: ENDPOINT,
    serverCertificateHashes: hashes(),
    authorize: async () => descriptor(),
    refreshAuthorization: async () => ({carrierHandle: SESSION_ID, expiresAtMs: 2000}),
    library: {...pinnedLibrary, Connection: {async connect() { return connection; }}},
    clock: {monotonicNow: () => time},
  });
  const session = await transport.connect({grant: {}, signal: new AbortController().signal});
  const frames = [];
  const unsubscribe = await session.subscribe({onFrame: frame => frames.push(frame), onPublisherEnded() {}, signal: new AbortController().signal});
  announcements.append({path: remoteId, active: true});
  const object = sequence => encodeVoiceObject({sequence, timestamp: sequence * 960n, duration: 960, payload: Uint8Array.of(1)});
  const send = (...sequences) => remoteAudio.appendDatagram(new pinnedLibrary.Time.Timestamp(0, pinnedLibrary.Time.Timescale.MICRO), encodeVoiceBundle(sequences.map(object)));
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
  const audio = published.consume().track('audio').subscribe({startGroup: 0});
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

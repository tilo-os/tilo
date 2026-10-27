import test from 'node:test';
import assert from 'node:assert/strict';
import {createVoiceTransport} from './voice-transport.mjs';

const CALL = 'c'.repeat(64), SELF = 'a'.repeat(64), REMOTE = 'b'.repeat(64);
const drain = async () => {for (let i = 0; i < 10; i++) await Promise.resolve();};
test('leaving closes a publisher request that has not resolved a broadcast', async () => {
  let announce;
  let requested;
  const origins = [];
  class Producer {
    constructor() {origins.push(this);}
    announced() {return {next: () => new Promise(resolve => {announce = resolve;}), close() {announce?.();}};}
    request() {requested = {active: {peek: () => undefined, changed: () => new Promise(() => {})}, closed: false, close() {this.closed = true;}}; return requested;}
    close() {}
  }
  const library = {Origin: {Producer}, Path: {from: (...parts) => parts.join('/'), Pattern: {subtree: path => path}, stripPrefix: (prefix, path) => path.slice(prefix.length + 1)},
    Connection: {connect: async () => ({version: 'moq-lite-06', transport: 'webtransport', closed: new Promise(() => {}), close() {}})}};
  const transport = createVoiceTransport({library, carrier: async () => ({transport: {}, revoked: new Promise(() => {}), close() {}})});
  const session = await transport.connect({grant: {call: CALL, participation: SELF, token: '01', endpoint: 'https://localhost/moq/voice', directions: {publish: false, subscribe: true}}});
  session.subscribe({onFrame() {}});
  announce({kind: 'announced', prefix: `${CALL}/${REMOTE}`});
  await drain();
  assert.ok(requested, 'the actual carrier requested the announced remote');
  session.close();
  await drain();
  assert.equal(requested.closed, true, 'leave releases a pending request without waiting for the broadcaster');
});

test('a renewed publisher announcement replaces its old subscription and sequence window', async () => {
  const {encodeVoiceObject, encodeVoiceBundle} = await import('./voice-profile.mjs');
  let announce;
  const requests = [];
  class Producer {
    announced() {return {next: () => new Promise(resolve => {announce = resolve;}), close() {announce?.();}};}
    request() {
      let datagram, group;
      const track = {
        recvDatagram: () => new Promise(resolve => {datagram = resolve;}),
        recvGroup: () => new Promise(resolve => {group = resolve;}),
        close() {datagram?.(); group?.();},
      };
      const request = {active: {peek: () => ({track: () => ({subscribe: () => track})})},
        closed: false, close() {this.closed = true;},
        deliver() {datagram({payload: encodeVoiceBundle([encodeVoiceObject({sequence: 0n, timestamp: 0n, duration: 960, payload: new Uint8Array([0xf8, 0xff, 0xfe])})])});}};
      requests.push(request); return request;
    }
    close() {}
  }
  const library = {Origin: {Producer}, Path: {from: (...parts) => parts.join('/'), Pattern: {subtree: path => path}, stripPrefix: (prefix, path) => path.slice(prefix.length + 1)},
    Connection: {connect: async () => ({version: 'moq-lite-06', transport: 'webtransport', closed: new Promise(() => {}), close() {}})}};
  const transport = createVoiceTransport({library, carrier: async () => ({transport: {}, revoked: new Promise(() => {}), close() {}})});
  const session = await transport.connect({grant: {call: CALL, participation: SELF, token: '01', endpoint: 'https://localhost/moq/voice', directions: {publish: false, subscribe: true}}});
  const frames = [];
  session.subscribe({onFrame: frame => frames.push(frame)});
  announce({kind: 'announced', prefix: `${CALL}/${REMOTE}`});
  await drain();
  requests[0].deliver();
  await drain();
  assert.equal(frames.length, 1);
  // The replacement can be announced before either old receive loop ends.
  announce({kind: 'announced', prefix: `${CALL}/${REMOTE}`});
  await drain();
  assert.equal(requests.length, 2, 'renewal must request the new broadcast');
  assert.equal(requests[0].closed, true, 'the replaced request is released');
  requests[1].deliver();
  await drain();
  assert.equal(frames.length, 2, 'sequence zero belongs to the new carrier and is audible');
  session.close();
  await drain();
  assert.equal(requests[1].closed, true, 'leave releases the renewed request');
});

test('a republish that takes the path without a new announcement is heard from its own sequence window', async () => {
  const {encodeVoiceObject, encodeVoiceBundle} = await import('./voice-profile.mjs');
  let announce;
  let request;
  // One broadcast per carrier of the remote publisher; ending it ends its
  // track's receive loops as a closed carrier does.
  const carrier = () => {
    let datagram, group;
    const track = {
      recvDatagram: () => new Promise(resolve => {datagram = resolve;}),
      recvGroup: () => new Promise(resolve => {group = resolve;}),
      close() {datagram?.(); group?.();},
    };
    return {
      track: () => ({subscribe: () => track}),
      end() {track.close();},
      deliver(sequence) {datagram({payload: encodeVoiceBundle([encodeVoiceObject({sequence, timestamp: sequence * 960n, duration: 960, payload: new Uint8Array([0xf8, 0xff, 0xfe])})])});},
    };
  };
  class Producer {
    announced() {return {next: () => new Promise(resolve => {announce = resolve;}), close() {announce?.();}};}
    request() {
      let current = carrier(), changed, waiters = 0;
      request = {
        active: {peek: () => current, changed: () => {waiters++; return new Promise(resolve => {changed = resolve;});}},
        get waiters() {return waiters;},
        current: () => current,
        swap(next) {current = next; changed?.();},
        closed: false, close() {this.closed = true;},
      };
      return request;
    }
    close() {}
  }
  const library = {Origin: {Producer}, Path: {from: (...parts) => parts.join('/'), Pattern: {subtree: path => path}, stripPrefix: (prefix, path) => path.slice(prefix.length + 1)},
    Connection: {connect: async () => ({version: 'moq-lite-06', transport: 'webtransport', closed: new Promise(() => {}), close() {}})}};
  const transport = createVoiceTransport({library, carrier: async () => ({transport: {}, revoked: new Promise(() => {}), close() {}})});
  const session = await transport.connect({grant: {call: CALL, participation: SELF, token: '01', endpoint: 'https://localhost/moq/voice', directions: {publish: false, subscribe: true}}});
  const frames = [];
  const publishers = [];
  session.subscribe({onFrame: frame => frames.push(frame), onPublisher: (publisher, active) => publishers.push([publisher, active])});
  announce({kind: 'announced', prefix: `${CALL}/${REMOTE}`});
  await drain();
  const first = request.current();
  first.deliver(0n);
  await drain();
  first.deliver(1n);
  await drain();
  assert.equal(frames.length, 2);
  // The remote's renewed carrier republishes the path and its old carrier
  // closes; the route never lapses, so no announcement says so.
  const second = carrier();
  first.end();
  await drain();
  assert.equal(request.waiters, 1, 'an ended broadcast waits for the path to resolve again, not spinning on it');
  request.swap(second);
  await drain();
  second.deliver(0n);
  await drain();
  assert.equal(frames.length, 3, 'the broadcast that took the path is heard, sequence zero included');
  assert.deepEqual(publishers, [[REMOTE, true]], 'the publisher never left the call');
  assert.equal(request.closed, false);
  session.close();
  await drain();
  assert.equal(request.closed, true, 'leave releases the request');
});

/** A publishing carrier whose datagrams and QUIC loss counters the test sees. */
async function publisher({getStats} = {}) {
  const {decodeVoiceBundle} = await import('./voice-profile.mjs');
  const datagrams = [];
  let now = 0;
  class Producer {
    createBroadcast() {
      return {announce() {}, close() {}, createTrack: () => ({appendDatagram: (_at, payload) => datagrams.push(decodeVoiceBundle(payload).map(object => object.sequence)), close() {}})};
    }
    consume() { return {}; }
    close() {}
  }
  const library = {Origin: {Producer}, Path: {from: (...parts) => parts.join('/')},
    Time: {Timescale: {MICRO: 'us'}, Timestamp: class {}},
    Connection: {connect: async () => ({version: 'moq-lite-06', transport: 'webtransport', closed: new Promise(() => {}), close() {}})}};
  const transport = createVoiceTransport({library, clock: {now: () => now}, carrier: async () => ({transport: {getStats}, revoked: new Promise(() => {}), close() {}})});
  const session = await transport.connect({grant: {call: CALL, participation: SELF, publication: `${CALL}/${SELF}`, token: '01', endpoint: 'https://localhost/moq/voice', directions: {publish: true, subscribe: false}}});
  let frame = 0n;
  // One 20 ms frame per call; a DTX pause skips media time, not sequence.
  const send = async (pause = 0n) => {
    frame += pause;
    session.publish({timestamp: frame * 960n, duration: 960, payload: Uint8Array.of(0x78, 1, 2)});
    frame++;
    now += 20;
    await drain();
  };
  return {session, datagrams, send};
}

test('a reset group loses its frames, not the publisher: the next group is heard', async () => {
  const {encodeVoiceObject, encodeVoiceBundle} = await import('./voice-profile.mjs');
  let announce;
  const groups = [];
  let waiting;
  const track = {
    recvDatagram: () => new Promise(() => {}),
    recvGroup: () => groups.length ? Promise.resolve(groups.shift()) : new Promise(resolve => {waiting = resolve;}),
    tryRecvGroup: () => undefined,
    close() {},
  };
  const send = group => {if (waiting) {const resolve = waiting; waiting = undefined; resolve(group);} else groups.push(group);};
  // A native publisher's group of objects; `reset` fails its next read as
  // the relay's reset of that group's stream does.
  const group = (sequence, {reset} = {}) => {
    let read = 0;
    return {
      readFrame: async () => {
        if (reset) throw new Error('remote error: 1');
        if (read++) return undefined;
        return {payload: encodeVoiceBundle([encodeVoiceObject({sequence, timestamp: sequence * 960n, duration: 960, payload: new Uint8Array([0xf8, 0xff, 0xfe])})])};
      },
      close() {},
    };
  };
  class Producer {
    announced() {return {next: () => new Promise(resolve => {announce = resolve;}), close() {announce?.();}};}
    request() {return {active: {peek: () => ({track: () => ({subscribe: () => track})}), changed: () => new Promise(() => {})}, close() {}};}
    close() {}
  }
  const library = {Origin: {Producer}, Path: {from: (...parts) => parts.join('/'), Pattern: {subtree: path => path}, stripPrefix: (prefix, path) => path.slice(prefix.length + 1)},
    Connection: {connect: async () => ({version: 'moq-lite-06', transport: 'webtransport', closed: new Promise(() => {}), close() {}})}};
  const reports = [];
  const transport = createVoiceTransport({library, carrier: async () => ({transport: {}, revoked: new Promise(() => {}), close() {}}), onReport: report => reports.push(report)});
  const session = await transport.connect({grant: {call: CALL, participation: SELF, token: '01', endpoint: 'https://localhost/moq/voice', directions: {publish: false, subscribe: true}}});
  const frames = [];
  const publishers = [];
  session.subscribe({onFrame: frame => frames.push(frame), onPublisher: (publisher, active) => publishers.push([publisher, active])});
  announce({kind: 'announced', prefix: `${CALL}/${REMOTE}`});
  await drain();
  send(group(0n));
  await drain();
  assert.equal(frames.length, 1);
  send(group(5n, {reset: true}));
  await drain();
  send(group(10n));
  await drain();
  assert.deepEqual(frames.map(frame => frame.sequence), [0n, 10n], 'the group after the reset one is heard');
  assert.deepEqual(publishers, [[REMOTE, true]], 'the publisher never left the call');
  assert.ok(reports.some(report => report.type === 'voice-receive-drop' && report.reason === 'group-reset'));
  assert.ok(!reports.some(report => report.type === 'voice-subscription-error'), 'a lost group is no subscription error');
  session.close();
});

test('a clean uplink sends each frame once; measured loss adds up to two earlier frames', async () => {
  const counters = {packetsSent: 0, packetsLost: 0};
  const {session, datagrams, send} = await publisher({getStats: async () => ({...counters})});
  for (let index = 0; index < 100; index++) {
    counters.packetsSent += 1;
    await send();
  }
  assert.ok(datagrams.every(objects => objects.length === 1), 'no loss: one object per datagram');
  assert.equal(session.stats().redundancy, 0);
  // 6% loss reported by the carrier's own QUIC connection.
  for (let index = 0; index < 100; index++) {
    counters.packetsSent += 1;
    if (index % 16 === 0) counters.packetsLost += 1;
    await send();
  }
  const lossy = datagrams.slice(-40);
  assert.ok(lossy.every(objects => objects.length === 3), JSON.stringify(lossy.map(objects => objects.length)));
  assert.ok(datagrams.every(objects => objects.length <= 3));
  assert.deepEqual(lossy.at(-1), [199n, 198n, 197n], 'newest first, then the frames just before');
  // After a DTX pause the earlier frames are long late: not resent.
  await send(20n);
  assert.deepEqual(datagrams.at(-1), [200n]);
  session.close();
});

test('a browser without WebTransport loss counters protects isolated losses blind', async () => {
  const {session, datagrams, send} = await publisher();
  for (let index = 0; index < 5; index++) await send();
  assert.deepEqual(datagrams.map(objects => objects.length), [1, 2, 2, 2, 2]);
  assert.equal(session.stats().redundancy, 1);
  session.close();
});

test('the uplink redundancy policy matches the relay\'s (tilo-moq voice::Redundancy)', async () => {
  const {createRedundancy} = await import('./voice-transport.mjs');
  const hop = createRedundancy();
  let sent = 0, lost = 0;
  const window = (second, packets, losses) => { sent += packets; lost += losses; return hop.observe(second * 1000, sent, lost); };
  assert.equal(window(0, 0, 0), 0, 'the first sample is a baseline');
  assert.equal(window(1, 100, 0), 0);
  assert.equal(window(2, 100, 1), 1, '1% loss');
  assert.equal(window(3, 100, 6), 2, '5% loss');
  assert.equal(window(4, 3, 0), 2, 'a silent window measures nothing');
  const levels = [];
  let second = 5;
  for (; second <= 20; second++) levels.push(window(second, 100, 0));
  assert.equal(levels[0], 2);
  assert.ok(levels.includes(1));
  assert.equal(levels.at(-1), 0);
  assert.equal(window(second, 100, 10), 2);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {createLiveClient, LIVE_GRANT_TARGET} from './live-client.mjs';
import {createLiveTransport} from './live-transport.mjs';
import {createFakeRelay} from './live-fake-relay.test.mjs';
import {
  createFallbackTransport, createLiveCarriers, createFrameReader, encodeFrame, FRAME,
  FALLBACK_OPEN_PATH, FALLBACK_RESUME_PATH, FALLBACK_SEND_PATH, FALLBACK_SESSION_HEADER,
} from './live-fallback.mjs';

const ROOM = 'a'.repeat(64);
const ORIGIN = 'https://host.test:19343';
const ENDPOINT = `${ORIGIN}/moq/v21/live`;
const encoder = new TextEncoder();
const decoder = new TextDecoder();
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
async function until(predicate, label, limit = 1000) {
  for (let i = 0; i < limit; i++) { if (predicate()) return; await new Promise(resolve => setTimeout(resolve, 1)); }
  assert.fail(`timed out waiting for ${label}`);
}
const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
const unhex = value => Uint8Array.from(value.match(/../g), byte => parseInt(byte, 16));

/**
 * Fake host fallback routes bridged into the fake MoQ relay exactly as the
 * real host bridges: each fallback session is a relay session (same grant
 * check, same namespace), so MoQ and fallback members interoperate.
 */
function createFallbackHost(relay) {
  const sessions = new Map();
  const counters = {opens: 0, resumes: 0, sends: 0, refused: 0};
  let next = 0;
  function stream(session) {
    let controller;
    const body = new ReadableStream({start(value) { controller = value; }, cancel() { if (session.controller === controller) session.controller = undefined; }});
    session.controller = controller;
    const flags = (session.transport.directions.publish ? 1 : 0) | (session.transport.directions.subscribe ? 2 : 0);
    controller.enqueue(encodeFrame(FRAME.HELLO, unhex(session.id), Uint8Array.of(flags)));
    return new Response(body, {status: 200, headers: {'content-type': 'application/octet-stream'}});
  }
  function emit(session, frame) {
    try { session.controller?.enqueue(frame); } catch {}
  }
  function end(session, reason) {
    emit(session, encodeFrame(FRAME.END, Uint8Array.of(reason)));
    try { session.controller?.close(); } catch {}
    session.controller = undefined;
  }
  async function open(grant) {
    let transport;
    try {
      const publisher = decoder.decode(grant);
      transport = await createLiveTransport({library: relay.library}).connect({
        endpoint: ENDPOINT, authorization: grant, namespace: ['tilo-live', ROOM, publisher],
        directions: {publish: true, subscribe: true},
      });
    } catch {
      counters.refused++;
      return new Response('permission-denied', {status: 403});
    }
    counters.opens++;
    const session = {id: (++next).toString(16).padStart(32, '0'), transport, ops: new Map(), presence: new Map()};
    sessions.set(session.id, session);
    const response = stream(session);
    transport.watchMembers({onMember: (publisher, active) => {
      emit(session, encodeFrame(FRAME.MEMBER, unhex(publisher), Uint8Array.of(active ? 1 : 0)));
      if (active) {
        session.presence.get(publisher)?.();
        session.presence.set(publisher, transport.subscribePresence(publisher, bytes => emit(session, encodeFrame(FRAME.PRESENCE, unhex(publisher), bytes))));
      } else { session.presence.get(publisher)?.(); session.presence.delete(publisher); }
    }});
    void transport.closed.then(() => { if (sessions.get(session.id) === session) { sessions.delete(session.id); end(session, 2); } });
    return response;
  }
  function send(session, body) {
    counters.sends++;
    for (const {kind, payload} of createFrameReader().push(body)) {
      if (kind === FRAME.SEND_PRESENCE) session.transport.publishPresence(payload);
      else if (kind === FRAME.SEND_OPS) session.transport.publishOps(`ops/${hex(payload.subarray(0, 32))}`, payload.slice(32));
      else if (kind === FRAME.SEND_OPS_CLOSE) session.transport.closeOps(`ops/${hex(payload)}`);
      else if (kind === FRAME.SUBSCRIBE_OPS) {
        const publisher = hex(payload.subarray(0, 32));
        const record = payload.subarray(32, 64);
        const key = `${publisher}/${hex(record)}`;
        session.ops.get(key)?.();
        session.ops.set(key, session.transport.subscribeOps(publisher, `ops/${hex(record)}`,
          bytes => emit(session, encodeFrame(FRAME.OPS, unhex(publisher), record, bytes)),
          () => { session.ops.delete(key); emit(session, encodeFrame(FRAME.OPS_END, unhex(publisher), record)); }));
      } else if (kind === FRAME.UNSUBSCRIBE_OPS) {
        const key = `${hex(payload.subarray(0, 32))}/${hex(payload.subarray(32, 64))}`;
        session.ops.get(key)?.();
        session.ops.delete(key);
      } else if (kind === FRAME.SEND_CLOSE) { sessions.delete(session.id); session.transport.close(); end(session, 2); }
    }
    return new Response(null, {status: 204});
  }
  async function fetch(url, init = {}) {
    const path = new URL(url).pathname;
    assert.equal(init.method, 'POST');
    assert.equal(init.credentials, 'omit');
    const id = init.headers?.[FALLBACK_SESSION_HEADER];
    if (path === FALLBACK_OPEN_PATH) return open(new Uint8Array(init.body));
    const session = sessions.get(id);
    if (!session) return new Response('not-found', {status: 404});
    if (path === FALLBACK_RESUME_PATH) { counters.resumes++; return stream(session); }
    if (path === FALLBACK_SEND_PATH) return send(session, new Uint8Array(init.body));
    return new Response('unsupported', {status: 426});
  }
  return {
    fetch,
    counters,
    sessions,
    /** End every open stream with a rotation (the client resumes). */
    rotate() { for (const session of sessions.values()) end(session, 0); },
    /** Revoke a publisher: its session ends with END(revoked). */
    revoke(publisher) {
      for (const session of sessions.values()) {
        if (session.transport.namespace[2] !== publisher) continue;
        sessions.delete(session.id);
        end(session, 1);
        session.transport.close();
      }
    },
  };
}

const codec = {
  encodePresence: value => encoder.encode(JSON.stringify(value)),
  decodePresence: bytes => JSON.parse(decoder.decode(bytes)),
  encodePreview: value => encoder.encode(JSON.stringify({kind: 'preview', ...value})),
  decodeOps: bytes => JSON.parse(decoder.decode(bytes)),
  opsTrack: record => `ops/${[...encoder.encode(`${record.app}/${record.record}`)].map(b => b.toString(16).padStart(2, '0')).join('').padEnd(64, '0').slice(0, 64)}`,
};
function fakeRuntime(publisher) {
  let next = 0;
  return {
    peer_live_supported: () => true,
    peer_live_grant: () => JSON.stringify({kind: 'request', id: ++next, origin: ORIGIN, target: LIVE_GRANT_TARGET, body: [1], proof: [2]}),
    peer_live_grant_complete: () => JSON.stringify({
      kind: 'grant', authorization: [...encoder.encode(publisher)], endpoint: ENDPOINT,
      namespace: ['tilo-live', ROOM, publisher], directions: {publish: true, subscribe: true},
      expiresInMs: 60000, renewInMs: 40000, membershipGeneration: '1',
    }),
    peer_live_forget() {}, peer_live_reset() {},
    peer_live_admit: () => JSON.stringify({kind: 'admitted'}),
  };
}
const fastClock = {
  requestFrame: callback => setTimeout(callback, 0),
  cancelFrame: handle => clearTimeout(handle),
  setTimeout: (callback, delay) => setTimeout(callback, delay / 100),
  clearTimeout: handle => clearTimeout(handle),
  random: () => 0.5,
};
function withGrant(fetch) {
  return async (url, init) => (new URL(url).pathname === LIVE_GRANT_TARGET
    ? {ok: true, status: 200, arrayBuffer: async () => Uint8Array.of(9).buffer}
    : fetch(url, init));
}
function client(publisher, transport, http) {
  const errors = [];
  const live = createLiveClient({runtime: fakeRuntime(publisher), peerId: 1, fetch: withGrant(http), codec, transport, clock: fastClock, onError: error => errors.push(error)});
  return {live, api: live.api, errors};
}
function setup() {
  const relay = createFakeRelay({authorize: bytes => ({namespace: `tilo-live/${ROOM}/${decoder.decode(bytes)}`})});
  const host = createFallbackHost(relay);
  const fallback = () => createFallbackTransport({fetch: host.fetch, clock: fastClock});
  const moq = () => createLiveTransport({library: relay.library});
  return {relay, host, fallback, moq};
}
const B = 'b'.repeat(64);
const C = 'c'.repeat(64);

test('frame reader reassembles frames split across chunks and bounds lengths', () => {
  const bytes = [encodeFrame(FRAME.PING), encodeFrame(FRAME.PRESENCE, new Uint8Array(32).fill(1), encoder.encode('hi'))];
  const all = new Uint8Array([...bytes[0], ...bytes[1]]);
  const reader = createFrameReader();
  const frames = [];
  for (let i = 0; i < all.length; i += 3) frames.push(...reader.push(all.subarray(i, i + 3)));
  assert.deepEqual(frames.map(frame => frame.kind), [FRAME.PING, FRAME.PRESENCE]);
  assert.equal(decoder.decode(frames[1].payload.subarray(32)), 'hi');
  assert.throws(() => createFrameReader(10).push(Uint8Array.of(3, 0, 0, 1, 0)));
});

test('presence and previews flow both ways between a WebTransport and a fallback member', async () => {
  const {host, fallback, moq} = setup();
  const a = client(B, moq(), host.fetch);
  const b = client(C, fallback(), host.fetch);
  const atA = [];
  const atB = [];
  const previews = [];
  a.api.onRemotePresence(value => atA.push(value));
  b.api.onRemotePresence(value => atB.push(value));
  a.api.onPreview(value => previews.push(value));
  a.live.start();
  b.live.start();
  await until(() => a.api.state === 'live' && b.api.state === 'live', 'both live');
  assert.equal(b.api.stats().transport.mode, 'https-stream');
  await until(() => a.api.stats().remotes === 1 && b.api.stats().remotes === 1, 'members');
  for (let i = 0; i < 5; i++) { a.api.publishPresence({pointer: [i, 1]}); b.api.publishPresence({pointer: [100 + i, 2]}); await tick(); await tick(); }
  await until(() => atB.some(r => r.frame.pointer?.[0] === 4), 'MoQ presence at the fallback member');
  await until(() => atA.some(r => r.frame.pointer?.[0] === 104), 'fallback presence at the MoQ member');
  assert.ok(atA.every((r, i) => i === 0 || r.frame.seq > atA[i - 1].frame.seq), 'latest value, ordered');
  // Previews from the fallback member reach the MoQ member in order.
  const record = {app: 'so.tilo.word-processor', record: 'doc-1'};
  a.api.watchRecord(record);
  await tick();
  await new Promise(resolve => setTimeout(resolve, 20));
  for (let i = 0; i < 30; i++) b.api.publishPreview(record, [{kind: 'insert', at: '01', text: String(i)}]);
  await until(() => previews.reduce((n, p) => n + p.ops.length, 0) === 30, 'previews');
  assert.deepEqual(previews.flatMap(p => p.ops.map(op => op.text)), [...Array(30).keys()].map(String));
  // And previews from the MoQ member reach the fallback member.
  const back = [];
  b.api.onPreview(value => back.push(value));
  b.api.watchRecord(record);
  await new Promise(resolve => setTimeout(resolve, 20));
  for (let i = 0; i < 10; i++) a.api.publishPreview(record, [{kind: 'insert', at: '01', text: `m${i}`}]);
  await until(() => back.reduce((n, p) => n + p.ops.length, 0) === 10, 'previews at fallback');
  assert.deepEqual(back.flatMap(p => p.ops.map(op => op.text)), [...Array(10).keys()].map(i => `m${i}`));
  a.live.close();
  await until(() => b.api.stats().remotes === 0, 'member left');
  b.live.close();
});

test('a fallback stream rotation resumes the same session without losing presence', async () => {
  const {host, fallback} = setup();
  const a = client(B, fallback(), host.fetch);
  const b = client(C, fallback(), host.fetch);
  const atB = [];
  b.api.onRemotePresence(value => atB.push(value));
  a.live.start();
  b.live.start();
  await until(() => a.api.stats().remotes === 1 && b.api.stats().remotes === 1, 'members');
  host.rotate();
  await until(() => host.counters.resumes >= 2, 'both resumed');
  a.api.publishPresence({pointer: [7, 7]});
  await until(() => atB.some(r => r.frame.pointer?.[0] === 7), 'presence after rotation');
  assert.equal(a.api.state, 'live');
  assert.ok(b.api.stats().transport.rotations >= 1);
  assert.equal(host.counters.opens, 2, 'no new session for a rotation');
  a.live.close();
  b.live.close();
});

test('revocation ends the fallback session and the client re-grants', async () => {
  const {host, fallback} = setup();
  const a = client(B, fallback(), host.fetch);
  a.live.start();
  await until(() => a.api.state === 'live', 'live');
  const opens = host.counters.opens;
  host.revoke(B);
  await until(() => host.counters.opens > opens && a.api.state === 'live', 're-granted and live');
  a.live.close();
});

test('carrier selection: fallback without WebTransport, after a failure (sticky), and after a hung attempt', async () => {
  const {host, fallback, moq} = setup();
  const options = {endpoint: ENDPOINT, authorization: encoder.encode(B), namespace: ['tilo-live', ROOM, B], directions: {publish: true, subscribe: true}};
  const none = createLiveCarriers({webtransport: moq(), fallback: fallback(), hasWebTransport: () => false});
  const s1 = await none.connect(options);
  assert.equal(s1.stats().mode, 'https-stream');
  s1.close();

  let attempts = 0;
  const failing = createLiveCarriers({webtransport: {connect: async () => { attempts++; throw new Error('QUIC handshake failed'); }}, fallback: fallback(), hasWebTransport: () => true});
  const s2 = await failing.connect(options);
  assert.equal(s2.stats().mode, 'https-stream');
  const s3 = await failing.connect(options);
  assert.equal(attempts, 1, 'fallback preferred after a WebTransport failure');
  assert.equal(failing.state().preferFallback, true);
  s2.close(); s3.close();

  const hung = createLiveCarriers({webtransport: {connect: () => new Promise(() => {})}, fallback: fallback(), hasWebTransport: () => true, budgetMs: 20});
  const s4 = await hung.connect(options);
  assert.equal(s4.stats().mode, 'https-stream');
  s4.close();

  const working = createLiveCarriers({webtransport: moq(), fallback: fallback(), hasWebTransport: () => true});
  const s5 = await working.connect(options);
  assert.notEqual(s5.stats().mode, 'https-stream', 'WebTransport preferred when it works');
  s5.close();
});

test('an older host without fallback routes reports unsupported and keeps WebTransport only', async () => {
  const old = async () => new Response('unsupported-version', {status: 426});
  const carriers = createLiveCarriers({
    webtransport: {connect: async () => { throw new Error('blocked'); }},
    fallback: createFallbackTransport({fetch: old}), hasWebTransport: () => true,
  });
  const options = {endpoint: ENDPOINT, authorization: encoder.encode(B), namespace: ['tilo-live', ROOM, B], directions: {publish: true, subscribe: true}};
  await assert.rejects(carriers.connect(options), error => error.code === 'unsupported');
  assert.equal(carriers.state().fallbackUnsupported, true);
  await assert.rejects(carriers.connect(options), /blocked/);
});

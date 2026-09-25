import test from 'node:test';
import assert from 'node:assert/strict';
import {createLiveClient, LIVE_GRANT_TARGET, AGENT_ACTIVITY_RECORD, AGENT_ACTIVITY_EXPIRE_MS, decodeAgentActivity} from './live-client.mjs';
import {createLiveTransport} from './live-transport.mjs';
import {createFakeRelay} from './live-fake-relay.test.mjs';

const ROOM = 'a'.repeat(64);
const ORIGIN = 'https://host.test:19343';
const tick = () => new Promise(resolve => setTimeout(resolve, 0));
async function until(predicate, label, limit = 400) {
  for (let i = 0; i < limit; i++) { if (predicate()) return; await new Promise(resolve => setTimeout(resolve, 1)); }
  assert.fail(`timed out waiting for ${label}`);
}
const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** JSON stand-in for the Rust codec (the canonical codec is tested in Rust). */
const codec = {
  encodePresence: value => encoder.encode(JSON.stringify(value)),
  decodePresence: bytes => JSON.parse(decoder.decode(bytes)),
  encodePreview: value => encoder.encode(JSON.stringify({kind: 'preview', ...value})),
  decodeOps: bytes => JSON.parse(decoder.decode(bytes)),
  opsTrack: record => `ops/${[...encoder.encode(`${record.app}/${record.record}`)].map(b => b.toString(16).padStart(2, '0')).join('').padEnd(64, '0').slice(0, 64)}`,
};
const signedChange = (record, digest) => encoder.encode(JSON.stringify({kind: 'signed-change', record, digest, canonical: [1, 2, 3]}));

/** Fake WASM runtime: grant custody and checked admission bookkeeping. */
function fakeRuntime(publisher, {supported = true, renewInMs = 40000, authorized = {publish: true, subscribe: true}} = {}) {
  const calls = {grants: [], completes: 0, forgets: 0, resets: 0, admits: []};
  const admitted = new Set();
  let next = 0;
  return {
    calls,
    options: {supported, renewInMs},
    peer_live_supported: () => supported,
    peer_live_grant(id, publish, subscribe, fresh) {
      calls.grants.push({publish, subscribe, fresh});
      return JSON.stringify({kind: 'request', id: ++next, origin: ORIGIN, target: LIVE_GRANT_TARGET, body: [1, 2], proof: [3, 4]});
    },
    peer_live_grant_complete(id, request, bytes) {
      calls.completes++;
      assert.deepEqual([...bytes], [9]);
      return JSON.stringify({
        kind: 'grant', authorization: [...encoder.encode(publisher)], endpoint: `${ORIGIN}/moq/v21/live`,
        namespace: ['tilo-live', ROOM, publisher], directions: authorized,
        expiresInMs: 60000, renewInMs: this.options.renewInMs, membershipGeneration: '4',
      });
    },
    peer_live_forget() { calls.forgets++; },
    peer_live_reset() { calls.resets++; },
    peer_live_admit(id, bytes) {
      const frame = JSON.parse(decoder.decode(bytes));
      calls.admits.push(frame.digest);
      const kind = admitted.has(frame.digest) ? 'duplicate' : 'admitted';
      admitted.add(frame.digest);
      return JSON.stringify({kind, digest: frame.digest});
    },
  };
}
function fakeFetch(status = () => 200) {
  const requests = [];
  const fetch = async (url, init) => {
    requests.push({url, init});
    const code = status(requests.length);
    return {ok: code === 200, status: code, arrayBuffer: async () => Uint8Array.of(9).buffer};
  };
  return {fetch, requests};
}
const fastClock = {
  requestFrame: callback => setTimeout(callback, 0),
  cancelFrame: handle => clearTimeout(handle),
  // Scale protocol delays (backoff, renewal, repeats) down 100x.
  setTimeout: (callback, delay) => setTimeout(callback, delay / 100),
  clearTimeout: handle => clearTimeout(handle),
  random: () => 0.5,
};

function relay() {
  return createFakeRelay({authorize: bytes => ({namespace: `tilo-live/${ROOM}/${decoder.decode(bytes)}`})});
}
function client(fake, publisher, options = {}) {
  const runtime = options.runtime ?? fakeRuntime(publisher, options);
  const http = options.http ?? fakeFetch();
  const errors = [];
  const live = createLiveClient({
    runtime, peerId: 1, fetch: http.fetch, codec,
    transport: options.transport ?? createLiveTransport({library: fake.library}),
    clock: options.clock ?? fastClock, onError: error => errors.push(error),
  });
  return {live, api: live.api, runtime, http, errors};
}

test('grant request goes through the proven data-session path and live connects', async () => {
  const fake = relay();
  const a = client(fake, 'b'.repeat(64));
  const states = [];
  a.api.onState(value => states.push(value.state));
  a.live.start();
  await until(() => a.api.state === 'live', 'live');
  assert.deepEqual(states, ['authorizing', 'connecting', 'live']);
  const request = a.http.requests[0];
  assert.equal(request.url, `${ORIGIN}${LIVE_GRANT_TARGET}`);
  assert.equal(request.init.method, 'POST');
  assert.equal(request.init.headers['x-tilo-delivery-proof'], '0304');
  assert.equal(request.init.credentials, 'omit');
  assert.deepEqual(a.runtime.calls.grants[0], {publish: true, subscribe: true, fresh: true});
  assert.equal(a.runtime.calls.forgets, 1, 'request handle released');
  assert.equal(a.api.publisherId, 'b'.repeat(64));
  a.live.close();
  assert.equal(a.api.state, 'closed');
});

test('remote presence arrives with publish/receive timestamps and interpolated state', async () => {
  const fake = relay();
  const a = client(fake, 'b'.repeat(64));
  const b = client(fake, 'c'.repeat(64));
  const published = [];
  const received = [];
  a.api.onPublish(value => published.push(value));
  b.api.onRemotePresence(value => received.push(value));
  a.live.start();
  b.live.start();
  await until(() => a.api.state === 'live' && b.api.state === 'live', 'both live');
  await until(() => b.api.stats().remotes === 1, 'member');
  for (let i = 0; i < 5; i++) { a.api.publishPresence({pointer: [i * 10, 5], focus: {app: 'so.tilo.notes', record: 'n1'}}); await tick(); await tick(); }
  await until(() => received.some(r => r.frame.pointer?.[0] === 40), 'latest pointer');
  const last = received.at(-1);
  assert.equal(last.publisher, 'b'.repeat(64));
  assert.ok(Number.isFinite(last.receivedAtMs) && Number.isFinite(last.receivedWallMs));
  assert.ok(published.every(p => Number.isFinite(p.publishedAtMs) && Number.isSafeInteger(p.sentAtMs)));
  assert.ok(received.every((r, i) => i === 0 || r.frame.seq > received[i - 1].frame.seq), 'sequence ordered');
  const [remote] = b.api.remoteState();
  assert.equal(remote.publisher, 'b'.repeat(64));
  assert.deepEqual(remote.focus, {app: 'so.tilo.notes', record: 'n1'});
  assert.ok(remote.pointer[0] <= 40 && remote.pointer[0] >= 0);
  a.live.close();
  await until(() => b.api.stats().remotes === 0, 'member left');
  b.live.close();
});

test('previews are ordered per record and only received for watched or focused records', async () => {
  const fake = relay();
  const a = client(fake, 'b'.repeat(64));
  const b = client(fake, 'c'.repeat(64));
  const previews = [];
  b.api.onPreview(value => previews.push(value));
  a.live.start();
  b.live.start();
  await until(() => a.api.state === 'live' && b.api.state === 'live' && b.api.stats().remotes === 1, 'live');
  const record = {app: 'so.tilo.word-processor', record: 'doc-1'};
  const other = {app: 'so.tilo.word-processor', record: 'doc-2'};
  const unwatch = b.api.watchRecord(record);
  await tick();
  for (let i = 0; i < 40; i++) a.api.publishPreview(record, [{kind: 'insert', at: '01', text: String(i)}]);
  a.api.publishPreview(other, [{kind: 'insert', at: '01', text: 'x'}]);
  await until(() => previews.reduce((n, p) => n + p.ops.length, 0) === 40, 'previews');
  const text = previews.flatMap(p => p.ops.map(op => op.text));
  assert.deepEqual(text, [...Array(40).keys()].map(String), 'ordered, none lost');
  assert.ok(previews.every(p => p.record.record === 'doc-1'), 'unwatched record not received');
  assert.ok(previews.every((p, i) => i === 0 || p.seq > previews[i - 1].seq));
  unwatch();
  a.live.close();
  b.live.close();
});

test('forwarded signed changes are admitted once through the runtime and snap previews', async () => {
  const fake = relay();
  const b = client(fake, 'c'.repeat(64));
  const changes = [];
  const durable = [];
  b.api.onSignedChange(value => changes.push(value));
  b.api.onDurable(value => durable.push(value));
  b.live.start();
  await until(() => b.api.state === 'live', 'live');
  const record = {app: 'so.tilo.notes', record: 'n1'};
  b.api.watchRecord(record);
  // The host forwards admitted changes on its own live namespace.
  const host = await createLiveTransport({library: fake.library}).connect({
    endpoint: `${ORIGIN}/moq/v21/live`, authorization: encoder.encode('d'.repeat(64)),
    namespace: ['tilo-live', ROOM, 'd'.repeat(64)], directions: {publish: true, subscribe: false},
  });
  await until(() => b.api.stats().remotes === 1, 'host member');
  await tick();
  const frame = signedChange(record, 'e'.repeat(64));
  host.publishOps(codec.opsTrack(record), frame);
  host.publishOps(codec.opsTrack(record), frame);
  host.publishOps(codec.opsTrack(record), signedChange(record, 'f'.repeat(64)));
  await until(() => changes.length === 2, 'changes');
  assert.deepEqual(b.runtime.calls.admits, ['e'.repeat(64), 'f'.repeat(64)], 'de-duplicated by digest before admission');
  assert.deepEqual(changes.map(c => c.result), ['admitted', 'admitted']);
  assert.ok(changes.every(c => Number.isFinite(c.receivedAtMs) && Number.isFinite(c.admittedAtMs)));
  assert.equal(durable.length, 2);
  host.close();
  b.live.close();
});

test('own signed publications are forwarded in order behind the previews of the keys they contain', async () => {
  const fake = relay();
  const runtimeA = fakeRuntime('b'.repeat(64));
  const listeners = [];
  runtimeA.peer_live_author_listen = (id, listener) => listeners.push({id, listener});
  const a = client(fake, 'b'.repeat(64), {runtime: runtimeA});
  const b = client(fake, 'c'.repeat(64));
  assert.equal(listeners.length, 1, 'registered with the runtime at creation');
  const authored = listeners[0].listener;
  const record = {app: 'so.tilo.word-processor', record: 'doc-1'};
  // Before live: nothing to forward (the durable path delivers it).
  authored(signedChange(record, '0'.repeat(64)), record.app, record.record);
  const order = [];
  b.api.onPreview(value => order.push(...value.ops.map(op => op.text)));
  b.api.onSignedChange(value => order.push(`#${value.digest.slice(0, 1)}`));
  a.live.start();
  b.live.start();
  await until(() => a.api.state === 'live' && b.api.state === 'live' && b.api.stats().remotes === 1, 'live');
  assert.equal(a.api.memberCount, 1);
  b.api.watchRecord(record);
  await tick();
  // Keys h, i are previewed; the runtime publishes the change containing them
  // (synchronously, before the queued previews are flushed); key j follows.
  a.api.publishPreview(record, [{kind: 'insert', at: '01', text: 'h'}]);
  a.api.publishPreview(record, [{kind: 'insert', at: '01', text: 'i'}]);
  authored(signedChange(record, '1'.repeat(64)), record.app, record.record);
  a.api.publishPreview(record, [{kind: 'insert', at: '01', text: 'j'}]);
  authored(signedChange(record, '2'.repeat(64)), record.app, record.record);
  await until(() => order.length === 5, 'frames');
  assert.deepEqual(order, ['h', 'i', '#1', 'j', '#2'], 'one ordered track: every preview before a change is in it');
  assert.deepEqual(b.runtime.calls.admits, ['1'.repeat(64), '2'.repeat(64)], 'the pre-live change was not forwarded');
  assert.equal(a.api.stats().signedPublished, 2);
  // Invalid input is ignored without throwing.
  authored('not bytes', record.app, record.record);
  authored(signedChange(record, '3'.repeat(64)), '', record.record);
  a.live.close();
  assert.deepEqual(listeners.at(-1), {id: 1, listener: null}, 'unregistered on close');
  b.live.close();
});

test('a deferred signed change is retried in order until the runtime admits it', async () => {
  const fake = relay();
  const runtime = fakeRuntime('c'.repeat(64));
  const admit = runtime.peer_live_admit.bind(runtime);
  let busy = 3; // a durable cut is being persisted for the first three attempts
  const attempts = [];
  runtime.peer_live_admit = (id, bytes) => {
    const digest = JSON.parse(decoder.decode(bytes)).digest;
    attempts.push(digest.slice(0, 1));
    if (busy > 0) { busy--; return JSON.stringify({kind: 'deferred', digest}); }
    return admit(id, bytes);
  };
  const b = client(fake, 'c'.repeat(64), {runtime});
  const changes = [];
  b.api.onSignedChange(value => changes.push(`${value.digest.slice(0, 1)}:${value.result}`));
  b.live.start();
  await until(() => b.api.state === 'live', 'live');
  const record = {app: 'so.tilo.word-processor', record: 'doc-1'};
  b.api.watchRecord(record);
  const author = await createLiveTransport({library: fake.library}).connect({
    endpoint: `${ORIGIN}/moq/v21/live`, authorization: encoder.encode('d'.repeat(64)),
    namespace: ['tilo-live', ROOM, 'd'.repeat(64)], directions: {publish: true, subscribe: false},
  });
  await until(() => b.api.stats().remotes === 1, 'author member');
  await tick();
  author.publishOps(codec.opsTrack(record), signedChange(record, '1'.repeat(64)));
  author.publishOps(codec.opsTrack(record), signedChange(record, '2'.repeat(64)));
  await until(() => changes.length === 3, 'retried admissions');
  assert.deepEqual(changes, ['1:deferred', '1:admitted', '2:admitted'], 'the successor waits behind the deferred change');
  assert.deepEqual(attempts, ['1', '1', '1', '1', '2']);
  assert.equal(b.api.stats().signedRetryAdmitted, 1);
  author.close();
  b.live.close();
});

test('speculative forwarded changes are reconciled at their confirmation deadline', async () => {
  const fake = relay();
  const runtime = fakeRuntime('c'.repeat(64));
  const reconciles = [];
  const admit = runtime.peer_live_admit.bind(runtime);
  runtime.peer_live_admit = (id, bytes) => {
    const result = JSON.parse(admit(id, bytes));
    const wall = (globalThis.performance?.timeOrigin ?? 0) + performance.now();
    return JSON.stringify({...result, speculative: result.kind === 'admitted', confirmByMs: wall + 1000});
  };
  runtime.peer_live_reconcile = id => {
    reconciles.push(id);
    // First pass: one change withdrawn, one still outstanding; second: done.
    return JSON.stringify(reconciles.length === 1
      ? {outstanding: 1, withdrawn: 1, nextDeadlineMs: (globalThis.performance?.timeOrigin ?? 0) + performance.now() + 500}
      : {outstanding: 0, withdrawn: 0, nextDeadlineMs: null});
  };
  const b = client(fake, 'c'.repeat(64), {runtime});
  b.live.start();
  await until(() => b.api.state === 'live', 'live');
  const record = {app: 'so.tilo.chat', record: '@qos:package-primary'};
  b.api.watchRecord(record);
  const host = await createLiveTransport({library: fake.library}).connect({
    endpoint: `${ORIGIN}/moq/v21/live`, authorization: encoder.encode('d'.repeat(64)),
    namespace: ['tilo-live', ROOM, 'd'.repeat(64)], directions: {publish: true, subscribe: false},
  });
  await until(() => b.api.stats().remotes === 1, 'host member');
  await tick();
  host.publishOps(codec.opsTrack(record), signedChange(record, 'a'.repeat(64)));
  host.publishOps(codec.opsTrack(record), signedChange(record, 'b'.repeat(64)));
  await until(() => reconciles.length === 2, 'reconciled twice (one timer per earliest deadline)');
  assert.equal(b.api.stats().signedWithdrawn, 1);
  assert.equal(b.api.stats().signedAdmitted, 2);
  host.close();
  b.live.close();
});

test('grant renews in place before expiry without reconnecting', async () => {
  const fake = relay();
  const a = client(fake, 'b'.repeat(64), {renewInMs: 500});
  a.live.start();
  await until(() => a.api.state === 'live', 'live');
  await until(() => a.api.stats().renewals >= 2, 'renewals');
  assert.deepEqual(a.runtime.calls.grants.slice(1, 3).map(g => g.fresh), [false, false]);
  assert.equal(fake.counters.connects, 1, 'relay session kept');
  a.live.close();
});

test('revocation on renewal drops live and resets grant custody', async () => {
  const fake = relay();
  const http = fakeFetch(count => (count === 1 ? 200 : 403));
  const a = client(fake, 'b'.repeat(64), {renewInMs: 500, http});
  a.live.start();
  await until(() => a.api.state === 'live', 'live');
  await until(() => a.api.state === 'revoked', 'revoked');
  assert.ok(a.runtime.calls.resets >= 1);
  assert.equal(fake.connections.size, 0, 'relay connection closed');
  assert.equal(a.api.publishPreview({app: 'a', record: 'r'}, [{kind: 'insert', at: '01', text: 'x'}]), false);
  a.live.close();
});

test('reconnects with backoff after relay loss', async () => {
  const fake = relay();
  const a = client(fake, 'b'.repeat(64));
  const states = [];
  a.api.onState(value => states.push(value.state));
  a.live.start();
  await until(() => a.api.state === 'live', 'live');
  fake.dropAll();
  await until(() => states.includes('reconnecting'), 'reconnecting');
  await until(() => a.api.state === 'live', 'live again');
  assert.equal(fake.counters.connects, 2);
  assert.equal(a.runtime.calls.grants.at(-1).fresh, true, 'fresh participation after reconnect');
  a.live.close();
});

test('falls back silently when live is unsupported or keeps failing', async () => {
  const fake = relay();
  const unsupported = client(fake, 'b'.repeat(64), {supported: false});
  unsupported.live.start();
  await until(() => unsupported.api.state === 'unavailable', 'unavailable');
  assert.equal(unsupported.http.requests.length, 0);
  assert.doesNotThrow(() => unsupported.api.publishPresence({pointer: [1, 1]}));
  assert.equal(unsupported.api.publishPreview({app: 'a', record: 'r'}, [{kind: 'insert', at: '01', text: 'x'}]), false);
  assert.deepEqual(unsupported.api.remoteState(), []);
  unsupported.live.close();

  const broken = {connect: async () => { throw new Error('UDP blocked'); }};
  const failing = client(fake, 'c'.repeat(64), {transport: broken});
  const states = [];
  failing.api.onState(value => states.push(value.state));
  failing.live.start();
  await until(() => failing.api.state === 'unavailable', 'fallback after retries', 2000);
  assert.ok(states.filter(s => s === 'reconnecting').length >= 1);
  assert.equal(failing.api.stats().fallbacks, 1);
  failing.live.close();
});

test('grant HTTP status mapping: 401 transient, 403 refused, 404 not installed', async () => {
  const fake = relay();
  const transient = client(fake, 'b'.repeat(64), {http: fakeFetch(count => (count === 1 ? 401 : 200))});
  const states = [];
  transient.api.onState(value => states.push(value.state));
  transient.live.start();
  await until(() => transient.api.state === 'live', 'live after 401');
  assert.deepEqual(states.slice(0, 2), ['authorizing', 'reconnecting'], '401 retried with backoff');
  assert.equal(transient.api.stats().revocations, 0);
  transient.live.close();

  const refused = client(fake, 'c'.repeat(64), {http: fakeFetch(() => 403)});
  refused.live.start();
  await until(() => refused.api.state === 'revoked', 'revoked');
  assert.ok(refused.runtime.calls.resets >= 1);
  refused.live.close();

  const missing = client(fake, 'd'.repeat(64), {http: fakeFetch(() => 404)});
  missing.live.start();
  await until(() => missing.api.state === 'unavailable', 'unavailable');
  assert.equal(fake.counters.connects, 1, 'only the first client connected');
  missing.live.close();
});

test('a viewer grant (publish=false) subscribes without publishing', async () => {
  const fake = relay();
  const viewer = client(fake, 'b'.repeat(64), {authorized: {publish: false, subscribe: true}});
  const editor = client(fake, 'c'.repeat(64));
  const seen = [];
  viewer.api.onRemotePresence(value => seen.push(value));
  viewer.live.start();
  editor.live.start();
  await until(() => viewer.api.state === 'live' && editor.api.state === 'live' && viewer.api.stats().remotes === 1, 'live');
  viewer.api.publishPresence({pointer: [1, 1]});
  assert.equal(viewer.api.publishPreview({app: 'a', record: 'r'}, [{kind: 'insert', at: '01', text: 'x'}]), false);
  editor.api.publishPresence({pointer: [2, 2]});
  await until(() => seen.length >= 1, 'viewer receives');
  await tick();
  assert.equal(viewer.api.stats().presence.published, 0);
  assert.equal(editor.api.stats().remotes, 0, 'viewer publishes no namespace');
  viewer.live.close();
  editor.live.close();
});

test('relay GOAWAY/close re-grants immediately without reporting a failure', async () => {
  const fake = relay();
  const a = client(fake, 'b'.repeat(64));
  const states = [];
  a.api.onState(value => states.push(value.state));
  a.live.start();
  await until(() => a.api.state === 'live', 'live');
  const errorsBefore = a.errors.length;
  for (const connection of [...fake.connections]) connection.close(); // relay GOAWAY then close
  await until(() => a.api.state === 'live' && fake.counters.connects === 2, 're-granted');
  assert.equal(a.errors.length, errorsBefore, 'not reported as a failure');
  assert.equal(a.api.stats().regrants, 1);
  assert.equal(a.api.stats().reconnects, 0, 'no backoff for the first loss');
  assert.equal(a.runtime.calls.grants.at(-1).fresh, true);
  a.live.close();
});

test('ops for the locally focused record are received from every member, echo ignored', async () => {
  const fake = relay();
  const a = client(fake, 'b'.repeat(64));
  const b = client(fake, 'c'.repeat(64));
  const previews = [];
  a.api.onPreview(value => previews.push(value));
  a.live.start();
  b.live.start();
  await until(() => a.api.state === 'live' && b.api.state === 'live' && a.api.stats().remotes === 1, 'live');
  const record = {app: 'so.tilo.notes', record: 'n9'};
  // Only a's own focus names the record; nothing is watched explicitly.
  a.api.publishPresence({focus: record});
  await tick();
  b.api.publishPreview(record, [{kind: 'insert', at: '01', text: 'hi'}]);
  a.api.publishPreview(record, [{kind: 'insert', at: '01', text: 'self'}]);
  await until(() => previews.length === 1, 'preview');
  await tick();
  assert.deepEqual(previews.map(p => [p.publisher, p.ops[0].text]), [['c'.repeat(64), 'hi']]);
  a.live.close();
  b.live.close();
});

test('presence resumes after the relay withdraws and re-announces a member namespace', async () => {
  const fake = relay();
  const a = client(fake, 'b'.repeat(64));
  const b = client(fake, 'c'.repeat(64));
  const received = [];
  const members = [];
  b.api.onRemotePresence(value => received.push(value.frame.pointer?.[0]));
  b.api.onMember(value => members.push(value.active));
  a.live.start();
  b.live.start();
  await until(() => a.api.state === 'live' && b.api.state === 'live' && b.api.stats().remotes === 1, 'live');
  a.api.publishPresence({pointer: [1, 0]});
  await until(() => received.includes(1), 'first');
  // Revocation-style withdraw (no inactive announcement, tracks left open),
  // then the relay re-announces the namespace ~100 ms later.
  const path = `tilo-live/${ROOM}/${'b'.repeat(64)}`;
  const entry = fake.published.get(path);
  fake.withdraw(path);
  await new Promise(resolve => setTimeout(resolve, 5));
  fake.published.set(path, {...entry, closed: new Promise(() => {}), end() {}});
  fake.reannounce(path);
  await tick();
  a.api.publishPresence({pointer: [2, 0]});
  await until(() => received.includes(2), 'presence after re-announce');
  assert.deepEqual(members, [true, false, true], 're-subscribed on the fresh announcement');
  a.live.close();
  b.live.close();
});

// ---- Agent activity (lane H) ---------------------------------------------------
const SCRIBE = '5'.repeat(64);
const HUMAN = 'c'.repeat(64);
const activity = (fields = {}) => JSON.stringify({v: 1, agent: 'scribe', name: 'Scribe', state: 'writing', label: 'Writing in Writer', tool: null, at_ms: 1700000000000, ...fields});
/** Runtime whose room/authority derive `scribe` -> SCRIBE, and `mole` -> a roster device. */
function agentRuntime(publisher) {
  const runtime = fakeRuntime(publisher);
  runtime.agentCalls = [];
  runtime.peer_live_agent_publisher = (id, agent) => { runtime.agentCalls.push(agent); return {scribe: SCRIBE, mole: HUMAN}[agent] ?? '0'.repeat(64); };
  runtime.peer_live_members = () => JSON.stringify([{device: HUMAN, user: 'u1', name: 'Ada', owner: false}]);
  return runtime;
}
async function publisherSession(fake, namespace) {
  return createLiveTransport({library: fake.library}).connect({
    endpoint: `${ORIGIN}/moq/v21/live`, authorization: encoder.encode(namespace),
    namespace: ['tilo-live', ROOM, namespace], directions: {publish: true, subscribe: false},
  });
}

test('agent activity decode is strict and bounded', () => {
  assert.deepEqual(decodeAgentActivity(activity({tool: 'list_operations', state: 'tool'})), {agentId: 'scribe', name: 'Scribe', state: 'tool', label: 'Writing in Writer', tool: 'list_operations', atMs: 1700000000000, status: null});
  assert.equal(decodeAgentActivity(activity({status: 'waiting'})).status, 'waiting');
  assert.equal(decodeAgentActivity(activity({status: 'sleeping'})), null);
  assert.equal(decodeAgentActivity(activity({name: '  '})).name, 'Agent');
  assert.equal(decodeAgentActivity(activity({label: ''})).label, '');
  for (const bad of [activity({v: 2}), activity({name: ''}), activity({extra: 1}), activity({tool: ''}), activity({label: '\u00e9'.repeat(61)}), activity({at_ms: 1.5}), activity({state: 'hacking'}), activity({agent: ''}), activity({agent: 7}), activity({at_ms: -1}),
    activity({at_ms: 'x'}), activity({label: 'a\nb'}), activity({tool: 3}), activity({label: 'x'.repeat(2000)}), '[]', 'nope', '']) {
    assert.equal(decodeAgentActivity(bad), null, bad.slice(0, 60));
  }
});

test('agent activity is accepted only from the recomputed agent namespace and never reaches typing previews', async () => {
  const fake = relay();
  const runtime = agentRuntime('b'.repeat(64));
  const b = client(fake, 'b'.repeat(64), {runtime});
  const events = [];
  const previews = [];
  b.api.onAgentActivity(value => events.push(value));
  b.api.onPreview(value => previews.push(value));
  b.live.start();
  await until(() => b.api.state === 'live', 'live');
  const agent = await publisherSession(fake, SCRIBE);
  const human = await publisherSession(fake, HUMAN);
  await until(() => b.api.stats().remotes === 2, 'agent and human announced');
  await tick();
  const track = codec.opsTrack(AGENT_ACTIVITY_RECORD);
  const frame = (seq, text) => codec.encodePreview({seq, record: AGENT_ACTIVITY_RECORD, ops: [{kind: 'insert', at: 'a1', text}]});
  // A human member claiming to be the agent, and the roster device the
  // derivation happens to name: both refused.
  human.publishOps(track, frame(1, activity()));
  human.publishOps(track, frame(2, activity({agent: 'mole', name: 'Mole'})));
  agent.publishOps(track, frame(1, activity({agent: 'other', name: 'Impostor'})));
  agent.publishOps(track, codec.encodePreview({seq: 2, record: AGENT_ACTIVITY_RECORD, ops: [{kind: 'insert', at: '01', text: activity({state: 'done'})}]}));
  agent.publishOps(track, frame(3, activity({state: 'thinking', label: 'Thinking'})));
  agent.publishOps(track, frame(4, activity()));
  agent.publishOps(track, frame(4, activity({state: 'done'}))); // stale seq
  await until(() => events.length === 2, 'agent activity');
  assert.deepEqual(events.map(e => [e.publisher, e.agentId, e.name, e.state, e.expired]), [
    [SCRIBE, 'scribe', 'Scribe', 'thinking', false], [SCRIBE, 'scribe', 'Scribe', 'writing', false],
  ]);
  await tick();
  assert.equal(b.api.stats().agentActivityRejected, 4, 'spoofs and a wrong anchor');
  assert.equal(b.api.stats().agentActivityAccepted, 2);
  assert.deepEqual(b.api.agents().map(a => [a.publisher, a.agentId, a.state, a.label]), [[SCRIBE, 'scribe', 'writing', 'Writing in Writer']]);
  assert.equal(runtime.agentCalls.filter(id => id === 'scribe').length, 1, 'the namespace is derived once per agent');
  assert.deepEqual(previews, [], 'activity is state, not typing');
  // The agent's Writer previews still arrive as ordinary previews.
  const writer = {app: 'so.tilo.word-processor', record: 'doc-1'};
  b.api.watchRecord(writer);
  await tick();
  agent.publishOps(codec.opsTrack(writer), codec.encodePreview({seq: 1, record: writer, ops: [{kind: 'insert', at: '01', text: 'The ocean'}]}));
  await until(() => previews.length === 1, 'agent writer preview');
  assert.equal(previews[0].publisher, SCRIBE);
  agent.close();
  human.close();
  b.live.close();
  assert.deepEqual(b.api.agents(), []);
});

test('an agent expires after it has been quiet for the activity window', async () => {
  const fake = relay();
  let skew = 0;
  const clock = {...fastClock, now: () => performance.now() + skew};
  const b = client(fake, 'b'.repeat(64), {runtime: agentRuntime('b'.repeat(64)), clock});
  const events = [];
  b.api.onAgentActivity(value => events.push(value));
  b.live.start();
  await until(() => b.api.state === 'live', 'live');
  const agent = await publisherSession(fake, SCRIBE);
  await until(() => b.api.stats().remotes === 1, 'agent announced');
  await tick();
  agent.publishOps(codec.opsTrack(AGENT_ACTIVITY_RECORD), codec.encodePreview({seq: 1, record: AGENT_ACTIVITY_RECORD, ops: [{kind: 'insert', at: 'a1', text: activity()}]}));
  await until(() => events.length === 1, 'activity');
  skew = AGENT_ACTIVITY_EXPIRE_MS - 100;
  assert.equal(b.api.agents().length, 1, 'still within the window');
  skew = AGENT_ACTIVITY_EXPIRE_MS + 1;
  assert.deepEqual(b.api.agents(), [], 'hidden at once once quiet');
  await until(() => events.length === 2, 'expiry event', 1000);
  assert.equal(events[1].expired, true);
  assert.equal(events[1].agentId, 'scribe');
  agent.close();
  b.live.close();
});


import assert from 'node:assert/strict';
import {test} from 'node:test';

import {installWebTransportGuard} from './webtransport-guard.mjs';

/** Controllable fake WebTransport constructor and timer harness. */
function harness(options = {}) {
  let current = 1_000_000;
  let nextTimer = 0;
  const timers = new Map();
  const created = [];
  const listeners = new Map();

  class FakeWebTransport {
    constructor(url, init) {
      this.url = url;
      this.init = init;
      this.closedCalls = [];
      let resolveReady, rejectReady, resolveClosed, rejectClosed;
      this.ready = new Promise((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
      this.closed = new Promise((resolve, reject) => { resolveClosed = resolve; rejectClosed = reject; });
      this.ready.catch(() => {});
      this.closed.catch(() => {});
      this._resolveReady = resolveReady;
      this._rejectReady = rejectReady;
      this._resolveClosed = resolveClosed;
      this._rejectClosed = rejectClosed;
      created.push(this);
    }

    close(info) {
      this.closedCalls.push(info ?? null);
    }
  }

  const scope = {
    WebTransport: FakeWebTransport,
    addEventListener(type, listener) {
      let bucket = listeners.get(type);
      if (!bucket) { bucket = new Set(); listeners.set(type, bucket); }
      bucket.add(listener);
    },
    removeEventListener(type, listener) {
      listeners.get(type)?.delete(listener);
    },
    dispatch(type, event) {
      for (const listener of [...(listeners.get(type) ?? [])]) listener(event);
    },
  };

  const setTimer = (callback, ms) => {
    const id = ++nextTimer;
    timers.set(id, {at: current + ms, callback});
    return id;
  };
  const clearTimer = id => { timers.delete(id); };
  const advance = ms => {
    const end = current + ms;
    for (;;) {
      const due = [...timers].filter(([, item]) => item.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      timers.delete(due[0]);
      current = due[1].at;
      due[1].callback();
    }
    current = end;
  };

  const guard = installWebTransportGuard({
    scope,
    now: () => current,
    setTimer,
    clearTimer,
    random: () => 0.5,
    ...options,
  });

  return {scope, guard, created, FakeWebTransport, advance, now: () => current};
}

const flush = () => new Promise(resolve => setImmediate(resolve));

test('closes WebTransport on ready rejection so WebKit releases its UDP socket', async () => {
  const {scope, guard, created, FakeWebTransport} = harness();
  const wt = Reflect.construct(scope.WebTransport, ['https://server.example:8443/session']);
  assert.ok(wt instanceof FakeWebTransport, 'preserves native WebTransport instance');
  assert.equal(guard.activeCount(), 1);

  wt._rejectReady(new Error('QUIC handshake failed'));
  await flush();

  assert.equal(created.length, 1);
  assert.equal(created[0].closedCalls.length, 1, 'close() called once on ready rejection');
  assert.equal(guard.activeCount(), 0);
  guard.uninstall();
});

test('closes WebTransport when the server drops a session mid-flight', async () => {
  const {scope, guard, created} = harness();
  const wt = new scope.WebTransport('https://server.example:8443/session');
  wt._resolveReady();
  await flush();
  assert.equal(guard.activeCount(), 1);

  wt._rejectClosed(new Error('peer protocol violation: authenticated device was revoked'));
  await flush();

  assert.equal(created[0].closedCalls.length, 1, 'explicit close() called on remote session end');
  assert.equal(guard.activeCount(), 0);
  assert.equal(guard.stats('https://server.example:8443').failures, 1, 'short-lived server drop counts as a failure');
  guard.uninstall();
});

test('closes a hung WebTransport handshake when its timeout expires', async () => {
  const {scope, guard, created, advance} = harness({handshakeTimeoutMs: 5_000});
  const wt = new scope.WebTransport('https://server.example:8443/session');
  assert.equal(guard.activeCount(), 1);

  advance(5_000);
  assert.equal(created[0].closedCalls.length, 1, 'timed-out handshake is closed explicitly');
  assert.equal(guard.activeCount(), 0);
  assert.equal(guard.stats('https://server.example:8443').failures, 1);
  // A subsequent rejection from the already-closed transport does not double-close.
  wt._rejectReady(new Error('aborted'));
  await flush();
  assert.equal(created[0].closedCalls.length, 1);
  guard.uninstall();
});

test('applies exponential backoff and trips a circuit breaker on repeated failures', async () => {
  const {scope, guard, created, advance} = harness({
    breakerThreshold: 3,
    breakerCooldownMs: 30_000,
    backoff: {firstMs: 1000, factor: 2, capMs: 60_000, jitter: 0},
  });
  const url = 'https://server.example:8443/session';

  // Failure 1 -> 1s backoff.
  const t1 = new scope.WebTransport(url);
  t1._rejectReady(new Error('down'));
  await flush();
  assert.equal(created.length, 1);

  // Immediate retry during backoff allocates zero native WebTransport sockets.
  const blocked1 = new scope.WebTransport(url);
  assert.equal(created.length, 1, 'no native socket allocated during backoff');
  await assert.rejects(blocked1.ready, {code: 'transport-backoff'});

  // After 1s backoff -> Failure 2 -> 2s backoff.
  advance(1000);
  const t2 = new scope.WebTransport(url);
  assert.equal(created.length, 2);
  t2._rejectReady(new Error('down'));
  await flush();

  advance(1999);
  await assert.rejects(new scope.WebTransport(url).ready, {code: 'transport-backoff'});
  assert.equal(created.length, 2);

  // After 2s backoff -> Failure 3 -> circuit breaker trips for 30s.
  advance(1);
  const t3 = new scope.WebTransport(url);
  assert.equal(created.length, 3);
  t3._rejectReady(new Error('down'));
  await flush();

  assert.equal(guard.stats(url).trips, 1);
  advance(15_000);
  await assert.rejects(new scope.WebTransport(url).ready, {code: 'transport-circuit-open'});
  assert.equal(created.length, 3, 'circuit open allocates zero native sockets');

  // After 30s cooldown, a healthy session that stays open clears failure state.
  advance(15_000);
  const t4 = new scope.WebTransport(url);
  assert.equal(created.length, 4);
  t4._resolveReady();
  await flush();
  advance(2_500);
  assert.equal(guard.stats(url).failures, 0);
  assert.equal(guard.stats(url).trips, 0);
  guard.uninstall();
});

test('haltOrigin closes active sockets immediately and blocks all future dials even after online', async () => {
  const {scope, guard, created} = harness();
  const url = 'https://server.example:8443/session';
  const wt = new scope.WebTransport(url);
  wt._resolveReady();
  await flush();
  assert.equal(guard.activeCount(url), 1);

  const closedCount = guard.haltOrigin(url);
  assert.equal(closedCount, 1);
  assert.equal(created[0].closedCalls.length, 1);
  assert.equal(guard.activeCount(url), 0);

  // Coming back online must NOT lift a revocation halt.
  scope.dispatch('online');
  const blocked = new scope.WebTransport(url);
  assert.equal(created.length, 1, 'zero sockets allocated while halted');
  await assert.rejects(blocked.ready, {code: 'device-revoked'});

  // Explicit intentional reset (`allowOrigin`) allows a fresh pairing dial.
  guard.allowOrigin(url);
  const fresh = new scope.WebTransport(url);
  assert.equal(created.length, 2);
  fresh.close();
  assert.equal(guard.activeCount(url), 0);
  guard.uninstall();
});

test('closes all active transports on pagehide and enforces active socket ceiling', async () => {
  const {scope, guard, created} = harness({maxActive: 2});
  const t1 = new scope.WebTransport('https://a.example:8443/s');
  const t2 = new scope.WebTransport('https://b.example:8443/s');
  assert.equal(created.length, 2);

  // Third concurrent socket is rejected cleanly at the active ceiling.
  const t3 = new scope.WebTransport('https://c.example:8443/s');
  assert.equal(created.length, 2);
  await assert.rejects(t3.ready, {code: 'transport-limit'});

  // Persisted bfcache pagehide leaves them; non-persisted closes all sockets.
  scope.dispatch('pagehide', {persisted: true});
  assert.equal(guard.activeCount(), 2);
  scope.dispatch('pagehide', {persisted: false});
  assert.equal(guard.activeCount(), 0);
  assert.equal(t1.closedCalls.length, 1);
  assert.equal(t2.closedCalls.length, 1);
  guard.uninstall();
});

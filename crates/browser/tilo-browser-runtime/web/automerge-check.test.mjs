import test from 'node:test';
import assert from 'node:assert/strict';
import {createAutomergeGate} from './automerge-check.mjs';

/** Fake Worker: records posted checks; the test answers, errors or stays silent. */
function fakeWorkers() {
  const workers = [];
  const createWorker = () => {
    const listeners = {message: [], error: [], messageerror: []};
    const worker = {
      posted: [],
      transfers: [],
      terminated: false,
      addEventListener(type, callback) { listeners[type].push(callback); },
      postMessage(message, transfer) { this.posted.push(message); this.transfers.push(transfer); },
      terminate() { this.terminated = true; },
      reply(ok, index = this.posted.length - 1) {
        for (const callback of listeners.message) callback({data: {id: this.posted[index].id, ok}});
      },
      fail() { for (const callback of listeners.error) callback({message: 'worker failed'}); },
    };
    workers.push(worker);
    return worker;
  };
  return {workers, createWorker};
}
/** Manual timers: `fire()` runs every pending timeout. */
function manualClock() {
  const timers = new Map();
  let next = 0;
  return {
    setTimeout(callback, delay) { timers.set(++next, {callback, delay}); return next; },
    clearTimeout(handle) { timers.delete(handle); },
    pending: () => [...timers.values()],
    fire() { const due = [...timers.values()]; timers.clear(); for (const {callback} of due) callback(); },
  };
}

test('an accepted check resolves true and keeps the worker for the next check', async () => {
  const {workers, createWorker} = fakeWorkers();
  const clock = manualClock();
  const gate = createAutomergeGate({createWorker, clock, timeoutMs: 250});
  const base = Uint8Array.of(1, 2);
  const incoming = Uint8Array.of(3);
  const first = gate.check(base, incoming);
  assert.equal(workers.length, 1);
  assert.deepEqual(clock.pending().map(timer => timer.delay), [250]);
  const [posted] = workers[0].posted;
  assert.deepEqual([...posted.base], [1, 2]);
  assert.deepEqual([...posted.incoming], [3]);
  assert.equal(workers[0].transfers[0].length, 2, 'owned buffers are transferred, not copied');
  workers[0].reply(true);
  assert.equal(await first, true);
  assert.equal(clock.pending().length, 0, 'the deadline is cleared');
  const second = gate.check(new Uint8Array(), Uint8Array.of(4));
  workers[0].reply(true);
  assert.equal(await second, true);
  assert.equal(workers.length, 1);
  assert.equal(workers[0].terminated, false);
  assert.deepEqual(gate.stats(), {checks: 2, accepted: 2, refused: 0, timeouts: 0, failures: 0, workers: 1});
});

test('a check past its deadline resolves false, terminates the worker and the next check starts a fresh one', async () => {
  const {workers, createWorker} = fakeWorkers();
  const clock = manualClock();
  const gate = createAutomergeGate({createWorker, clock, timeoutMs: 250});
  const stuck = gate.check(new Uint8Array(), Uint8Array.of(1));
  clock.fire();
  assert.equal(await stuck, false);
  assert.equal(workers[0].terminated, true);
  // A late answer from the terminated worker changes nothing.
  workers[0].reply(true);
  const next = gate.check(new Uint8Array(), Uint8Array.of(2));
  assert.equal(workers.length, 2, 'fresh worker after the timeout');
  workers[1].reply(true);
  assert.equal(await next, true);
  assert.equal(gate.stats().timeouts, 1);
});

test('a refusal or worker error fails every check in flight and never reuses that worker', async () => {
  const {workers, createWorker} = fakeWorkers();
  const clock = manualClock();
  const gate = createAutomergeGate({createWorker, clock});
  const refused = gate.check(new Uint8Array(), Uint8Array.of(1));
  const queued = gate.check(new Uint8Array(), Uint8Array.of(2));
  workers[0].reply(false, 0);
  assert.equal(await refused, false);
  assert.equal(await queued, false, 'checks owed by the discarded worker fail');
  assert.equal(workers[0].terminated, true);
  assert.equal(clock.pending().length, 0);
  const crashed = gate.check(new Uint8Array(), Uint8Array.of(3));
  assert.equal(workers.length, 2);
  workers[1].fail();
  assert.equal(await crashed, false);
  assert.equal(workers[1].terminated, true);
  const after = gate.check(new Uint8Array(), Uint8Array.of(4));
  assert.equal(workers.length, 3);
  workers[2].reply(true);
  assert.equal(await after, true);
  assert.deepEqual(gate.stats(), {checks: 4, accepted: 1, refused: 1, timeouts: 0, failures: 1, workers: 3});
});

test('without Worker support or after close every check fails closed', async () => {
  const unsupported = createAutomergeGate({createWorker: () => null, clock: manualClock()});
  assert.equal(await unsupported.check(new Uint8Array(), Uint8Array.of(1)), false);
  const throwing = createAutomergeGate({createWorker: () => { throw Error('blocked'); }, clock: manualClock()});
  assert.equal(await throwing.check(new Uint8Array(), Uint8Array.of(1)), false);
  const {workers, createWorker} = fakeWorkers();
  const gate = createAutomergeGate({createWorker, clock: manualClock()});
  const pending = gate.check(new Uint8Array(), Uint8Array.of(1));
  gate.close();
  assert.equal(await pending, false);
  assert.equal(workers[0].terminated, true);
  assert.equal(await gate.check(new Uint8Array(), Uint8Array.of(2)), false);
  assert.equal(workers.length, 1);
});

test('a view over part of a larger buffer is copied rather than transferred', async () => {
  const {workers, createWorker} = fakeWorkers();
  const gate = createAutomergeGate({createWorker, clock: manualClock()});
  const backing = new Uint8Array(8);
  const pending = gate.check(backing.subarray(2, 4), Uint8Array.of(1));
  assert.equal(workers[0].transfers[0].length, 1);
  assert.equal(backing.byteLength, 8, 'the shared backing stays attached');
  workers[0].reply(true);
  assert.equal(await pending, true);
});

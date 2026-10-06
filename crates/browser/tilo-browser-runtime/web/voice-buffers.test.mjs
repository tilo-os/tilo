import test from 'node:test';
import assert from 'node:assert/strict';

import {createMediaQueue} from './voice-buffers.mjs';

function clock(initial = 0) {
  let value = initial;
  return {
    now: () => value,
    set: next => { value = next; },
  };
}

test('drain expires every stale frame, including the newest frame', () => {
  const time = clock(20);
  const queue = createMediaQueue({maxBytes: 16, maxAgeMs: 200, maxFrames: 10, maxDurationMs: 200, maxDedupEntries: 32, now: time.now});
  assert.deepEqual(queue.push({key: 'a', receivedAtMs: 20, durationMs: 20, bytes: Uint8Array.of(1)}), {accepted: true});
  time.set(30);
  assert.deepEqual(queue.push({key: 'b', receivedAtMs: 30, durationMs: 20, bytes: Uint8Array.of(2)}), {accepted: true});

  time.set(230);
  assert.deepEqual(queue.drain(), []);
  assert.deepEqual(queue.snapshot(), {
    queuedBytes: 0,
    queuedFrames: 0,
    queuedDurationMs: 0,
    expired: 2,
    overflow: 0,
    duplicates: 0,
  });
});

test('byte pressure evicts oldest frames and keeps the newest frame within the hard bound', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 5, maxAgeMs: 100, maxFrames: 5, maxDurationMs: 100, maxDedupEntries: 8, now: time.now});
  const oldBytes = Uint8Array.of(1, 2, 3);
  const newestBytes = Uint8Array.of(4, 5, 6, 7);
  assert.deepEqual(queue.push({key: 'old', receivedAtMs: 0, durationMs: 20, bytes: oldBytes}), {accepted: true});
  assert.deepEqual(queue.push({key: 'new', receivedAtMs: 0, durationMs: 20, bytes: newestBytes}), {accepted: true});
  assert.deepEqual(queue.snapshot(), {
    queuedBytes: 4,
    queuedFrames: 1,
    queuedDurationMs: 20,
    expired: 0,
    overflow: 1,
    duplicates: 0,
  });

  const drained = queue.drain();
  assert.equal(drained.length, 1);
  assert.equal(drained[0].key, 'new');
  assert.notStrictEqual(drained[0].bytes, newestBytes, 'queue must own its compact payload allocation');
  assert.deepEqual(drained[0].bytes, newestBytes);
});

test('sender duration and frame limits retain at most five 20 ms frames during a burst', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 1024, maxAgeMs: 1000, maxFrames: 5, maxDurationMs: 100, maxDedupEntries: 16, now: time.now});
  for (let sequence = 0; sequence < 20; sequence++) {
    assert.deepEqual(queue.push({key: sequence, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(sequence)}), {accepted: true});
  }
  assert.deepEqual(queue.snapshot(), {
    queuedBytes: 5,
    queuedFrames: 5,
    queuedDurationMs: 100,
    expired: 0,
    overflow: 15,
    duplicates: 0,
  });
  assert.deepEqual(queue.drain().map(entry => entry.key), [15, 16, 17, 18, 19]);
});

test('drain resets duration accounting before the sender queue is refilled', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 1024, maxAgeMs: 1000, maxFrames: 5, maxDurationMs: 100, maxDedupEntries: 16, now: time.now});
  for (let sequence = 0; sequence < 5; sequence++) {
    queue.push({key: sequence, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(sequence)});
  }
  assert.equal(queue.drain().length, 5);
  assert.deepEqual(queue.push({key: 5, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(5)}), {accepted: true});
  assert.equal(queue.drain().length, 1);
  assert.deepEqual(queue.snapshot(), {
    queuedBytes: 0,
    queuedFrames: 0,
    queuedDurationMs: 0,
    expired: 0,
    overflow: 0,
    duplicates: 0,
  });
});

test('repeated one-frame drain cycles leave duration accounting at zero', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 1024, maxAgeMs: 1000, maxFrames: 5, maxDurationMs: 100, maxDedupEntries: 16, now: time.now});
  for (let sequence = 0; sequence < 6; sequence++) {
    assert.deepEqual(queue.push({key: sequence, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(sequence)}), {accepted: true});
    assert.equal(queue.drain().length, 1);
    assert.equal(queue.snapshot().queuedDurationMs, 0);
  }
});

test('default media limits stay bounded to ten frames and 200 ms', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 1024, maxAgeMs: 1000, now: time.now});
  for (let sequence = 0; sequence < 11; sequence++) {
    queue.push({key: sequence, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(sequence)});
  }
  assert.equal(queue.snapshot().queuedFrames, 10);
  assert.equal(queue.snapshot().overflow, 1);
});

test('frame count and buffered duration are enforced as independent limits', () => {
  const time = clock();
  const countQueue = createMediaQueue({maxBytes: 100, maxAgeMs: 1000, maxFrames: 2, maxDurationMs: 1000, maxDedupEntries: 4, now: time.now});
  for (let sequence = 0; sequence < 3; sequence++) {
    countQueue.push({key: sequence, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(sequence)});
  }
  assert.deepEqual(countQueue.drain().map(entry => entry.key), [1, 2]);

  const durationQueue = createMediaQueue({maxBytes: 100, maxAgeMs: 1000, maxFrames: 10, maxDurationMs: 30, maxDedupEntries: 12, now: time.now});
  durationQueue.push({key: 0, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(0)});
  durationQueue.push({key: 1, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(1)});
  assert.deepEqual(durationQueue.drain().map(entry => entry.key), [1]);
});

test('a single frame exceeding the media duration budget is rejected', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 100, maxAgeMs: 1000, maxFrames: 5, maxDurationMs: 100, maxDedupEntries: 8, now: time.now});
  assert.deepEqual(
    queue.push({key: 0, receivedAtMs: 0, durationMs: 101, bytes: Uint8Array.of(1)}),
    {accepted: false, reason: 'overflow'},
  );
  assert.equal(queue.snapshot().queuedFrames, 0);
});

test('a frame larger than the entire queue is rejected explicitly', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 3, maxAgeMs: 100, maxFrames: 5, maxDurationMs: 100, maxDedupEntries: 8, now: time.now});
  assert.deepEqual(
    queue.push({key: 'too-large', receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(1, 2, 3, 4)}),
    {accepted: false, reason: 'overflow'},
  );
  assert.deepEqual(queue.snapshot(), {
    queuedBytes: 0,
    queuedFrames: 0,
    queuedDurationMs: 0,
    expired: 0,
    overflow: 1,
    duplicates: 0,
  });
});

test('duplicate identities remain bounded to their live arrival window', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 16, maxAgeMs: 10, maxFrames: 5, maxDurationMs: 100, maxDedupEntries: 8, now: time.now});
  assert.deepEqual(queue.push({key: 'publication:7', receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(1)}), {accepted: true});
  assert.equal(queue.drain().length, 1);
  time.set(5);
  assert.deepEqual(
    queue.push({key: 'publication:7', receivedAtMs: 5, durationMs: 20, bytes: Uint8Array.of(2)}),
    {accepted: false, reason: 'duplicate'},
  );
  time.set(10);
  assert.deepEqual(queue.push({key: 'publication:7', receivedAtMs: 10, durationMs: 20, bytes: Uint8Array.of(3)}), {accepted: true});
  assert.equal(queue.snapshot().duplicates, 1);
});

test('drained live-window identities cannot grow beyond the independent dedup entry bound', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 100, maxAgeMs: 100, maxFrames: 2, maxDurationMs: 100, maxDedupEntries: 2, now: time.now});
  for (const key of ['a', 'b', 'c']) {
    assert.deepEqual(queue.push({key, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(1)}), {accepted: true});
    assert.equal(queue.drain().length, 1);
  }

  assert.deepEqual(
    queue.push({key: 'a', receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(2)}),
    {accepted: true},
    'the oldest drained identity must leave the bounded dedupe window',
  );
});

test('an already expired arrival is rejected without entering the queue', () => {
  const time = clock(201);
  const queue = createMediaQueue({maxBytes: 16, maxAgeMs: 200, maxFrames: 10, maxDurationMs: 200, maxDedupEntries: 16, now: time.now});
  assert.deepEqual(
    queue.push({key: 'late', receivedAtMs: 1, durationMs: 20, bytes: Uint8Array.of(1)}),
    {accepted: false, reason: 'expired'},
  );
  assert.deepEqual(queue.snapshot(), {
    queuedBytes: 0,
    queuedFrames: 0,
    queuedDurationMs: 0,
    expired: 1,
    overflow: 0,
    duplicates: 0,
  });
});

test('invalid limits, frames, and clocks are rejected', () => {
  const time = clock();
  assert.throws(() => createMediaQueue({maxBytes: 0, maxAgeMs: 100, now: time.now}), /maxBytes/);
  assert.throws(() => createMediaQueue({maxBytes: 1, maxAgeMs: 0, now: time.now}), /maxAgeMs/);
  assert.throws(() => createMediaQueue({maxBytes: 1, maxAgeMs: 100, now: 42}), /now/);

  assert.throws(() => createMediaQueue({maxBytes: 4, maxAgeMs: 100, maxFrames: 0, now: time.now}), /maxFrames/);
  assert.throws(() => createMediaQueue({maxBytes: 4, maxAgeMs: 100, maxDurationMs: 0, now: time.now}), /maxDurationMs/);
  assert.throws(() => createMediaQueue({maxBytes: 4, maxAgeMs: 100, maxFrames: 5, maxDedupEntries: 4, now: time.now}), /maxDedupEntries/);

  const queue = createMediaQueue({maxBytes: 4, maxAgeMs: 100, maxFrames: 4, maxDurationMs: 100, maxDedupEntries: 4, now: time.now});
  assert.throws(() => queue.push({key: 'bad-bytes', receivedAtMs: 0, durationMs: 20, bytes: [1]}), /Uint8Array/);
  assert.throws(() => queue.push({key: 'bad-time', receivedAtMs: Number.NaN, durationMs: 20, bytes: Uint8Array.of(1)}), /receivedAtMs/);
  assert.throws(() => queue.push({key: 'bad-duration', receivedAtMs: 0, durationMs: 0, bytes: Uint8Array.of(1)}), /durationMs/);
  assert.throws(() => queue.push({key: 'future', receivedAtMs: 1, durationMs: 20, bytes: Uint8Array.of(1)}), /future/);
  assert.throws(() => queue.push({key: {}, receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(1)}), /key/);
  assert.throws(() => queue.push({key: 'x'.repeat(257), receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(1)}), /key/);

  assert.deepEqual(queue.push({key: 'valid', receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(1)}), {accepted: true});
  time.set(-1);
  assert.throws(() => queue.drain(), /regressed/);
  time.set(0);
  assert.deepEqual(queue.snapshot(), {
    queuedBytes: 1,
    queuedFrames: 1,
    queuedDurationMs: 20,
    expired: 0,
    overflow: 0,
    duplicates: 0,
  }, 'a rejected clock reading must leave queue state unchanged');
});

test('clear releases queued payloads and duplicate identities for disposal', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 4, maxAgeMs: 100, maxFrames: 5, maxDurationMs: 100, maxDedupEntries: 8, now: time.now});
  assert.deepEqual(queue.push({key: 'reusable', receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(1, 2)}), {accepted: true});
  queue.clear();
  assert.deepEqual(queue.snapshot(), {
    queuedBytes: 0,
    queuedFrames: 0,
    queuedDurationMs: 0,
    expired: 0,
    overflow: 0,
    duplicates: 0,
  });
  assert.deepEqual(queue.push({key: 'reusable', receivedAtMs: 0, durationMs: 20, bytes: Uint8Array.of(3)}), {accepted: true});
});

test('queued payload ownership is compact and isolated from the caller backing buffer', () => {
  const time = clock();
  const queue = createMediaQueue({maxBytes: 1, maxAgeMs: 100, maxFrames: 1, maxDurationMs: 20, maxDedupEntries: 1, now: time.now});
  const backing = new Uint8Array(1024 * 1024);
  backing[500_000] = 7;
  const view = backing.subarray(500_000, 500_001);
  queue.push({key: 1, receivedAtMs: 0, durationMs: 20, bytes: view});
  view[0] = 9;

  const [entry] = queue.drain();
  assert.equal(entry.bytes[0], 7);
  assert.equal(entry.bytes.byteLength, 1);
  assert.equal(entry.bytes.buffer.byteLength, 1);
});

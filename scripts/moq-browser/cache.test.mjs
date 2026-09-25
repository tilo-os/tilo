import assert from "node:assert/strict";
import test from "node:test";

import * as Moq from "../../crates/browser/qos-browser-runtime/web/media-moq-library.mjs";

const BroadcastProducer = Moq.Broadcast.Producer;
const Timestamp = Moq.Time.Timestamp;
const TrackProducer = Moq.Track.Producer;

function writeVoiceBurst(track, groups = 10, framesPerGroup = 5) {
  for (let groupIndex = 0; groupIndex < groups; groupIndex += 1) {
    const group = track.appendGroup();
    for (let frameIndex = 0; frameIndex < framesPerGroup; frameIndex += 1) {
      group.writeFrame({
        payload: new Uint8Array([groupIndex, frameIndex]),
        timestamp: Timestamp.fromMillis(groupIndex * 100 + frameIndex * 20),
      });
    }
    group.close();
  }
}

function drainGroups(subscriber) {
  const groups = [];
  for (;;) {
    const group = subscriber.tryRecvGroup();
    if (!group) return groups;
    groups.push(group);
  }
}

function drainFrameCount(group) {
  let count = 0;
  while (group.tryReadFrame()) count += 1;
  return count;
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function blockEventLoop(milliseconds) {
  const blockedAt = performance.now();
  while (performance.now() - blockedAt < milliseconds) {
    // Exercise the read deadline before any expiry timer or microtask can run.
  }
}

test("producer cache limits bound rapid voice replay for a late subscriber", () => {
  const broadcast = new BroadcastProducer();
  const track = broadcast.createTrack("audio", { latencyMax: 100 }, { maxGroups: 1, maxFrames: 5 });

  writeVoiceBurst(track);

  const replay = drainGroups(track.subscribe());
  assert.deepEqual(replay.map((group) => group.sequence), [9]);
  assert.equal(replay.reduce((total, group) => total + drainFrameCount(group), 0), 5);
});

test("subscriber cache limits bound aggregate pending voice groups and frames", () => {
  const track = new TrackProducer("audio").accept({ latencyMax: 100 });
  const subscriber = track.subscribe({ maxGroups: 2, maxFrames: 10 });

  writeVoiceBurst(track);

  const pending = drainGroups(subscriber);
  assert.deepEqual(pending.map((group) => group.sequence), [8, 9]);
  assert.equal(pending.reduce((total, group) => total + drainFrameCount(group), 0), 10);
});

test("subscriber limits also bound the dynamically accepted incoming backing producer", async () => {
  const broadcast = new BroadcastProducer();
  const consumer = broadcast.consume();
  const subscriber = consumer.track("audio").subscribe({ maxGroups: 2, maxFrames: 10 });
  const request = await broadcast.requested();
  const backing = request.accept({ ordered: false });

  writeVoiceBurst(backing, 40, 5);

  const pending = drainGroups(subscriber);
  assert.deepEqual(pending.map((group) => group.sequence), [38, 39]);
  assert.equal(pending.reduce((total, group) => total + drainFrameCount(group), 0), 10);

  const replay = drainGroups(backing.subscribe());
  assert.deepEqual(replay.map((group) => group.sequence), [38, 39]);
  assert.equal(replay.reduce((total, group) => total + drainFrameCount(group), 0), 10);
});

test("omitting producer cache limits preserves the upstream retained window", () => {
  const track = new TrackProducer("audio").accept({ latencyMax: 100 });
  writeVoiceBurst(track);

  const replay = drainGroups(track.subscribe());
  assert.deepEqual(replay.map((group) => group.sequence), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
  assert.equal(replay.reduce((total, group) => total + drainFrameCount(group), 0), 50);
});

test("omitting subscriber cache limits preserves the upstream pending window", () => {
  const track = new TrackProducer("audio").accept({ latencyMax: 100 });
  const subscriber = track.subscribe();
  writeVoiceBurst(track);

  const pending = drainGroups(subscriber);
  assert.deepEqual(pending.map((group) => group.sequence), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
  assert.equal(pending.reduce((total, group) => total + drainFrameCount(group), 0), 50);
});

test("maxFrames is an aggregate cap even when one pending group exceeds it", () => {
  const track = new TrackProducer("audio").accept();
  const subscriber = track.subscribe({ maxGroups: 2, maxFrames: 5 });
  const group = track.appendGroup();
  for (let frame = 0; frame < 20; frame += 1) {
    group.writeFrame({ payload: new Uint8Array([frame]), timestamp: Timestamp.fromMillis(frame) });
  }
  group.close();

  const pending = drainGroups(subscriber);
  assert.equal(pending.length, 1);
  assert.equal(drainFrameCount(pending[0]), 5);
});

test("subscriber limits also bound a stalled group already handed to the reader", async () => {
  const track = new TrackProducer("audio").accept();
  const subscriber = track.subscribe({ maxGroups: 1, maxFrames: 5 });
  const source = track.appendGroup();
  const held = await subscriber.recvGroup();

  for (let frame = 0; frame < 20; frame += 1) {
    source.writeFrame({ payload: new Uint8Array([frame]), timestamp: Timestamp.fromMillis(frame) });
  }
  source.close();

  assert.equal(drainFrameCount(held), 5);
});

test("a handed group expires while its reader is idle", async () => {
  const track = new TrackProducer("audio").accept();
  const subscriber = track.subscribe({ maxGroups: 2, maxFrames: 10, maxAgeMs: 15 });
  const source = track.appendGroup();
  for (let frame = 0; frame < 5; frame += 1) {
    source.writeFrame({ payload: new Uint8Array([frame]), timestamp: Timestamp.fromMillis(frame * 20) });
  }
  source.close();
  const held = await subscriber.recvGroup();

  await delay(30);

	assert.equal(held.skipped, false);
	assert.equal(held.bufferedFrames, 0);
	assert.equal(held.tryReadFrame(), undefined);
	assert.equal(await held.readFrame(), undefined);
});

test("producer maxAgeMs expires retained source frames without another append", async () => {
  const broadcast = new BroadcastProducer();
  const track = broadcast.createTrack("audio", {}, { maxGroups: 1, maxFrames: 5, maxAgeMs: 15 });
  const source = track.appendGroup();
  for (let frame = 0; frame < 5; frame += 1) {
    source.writeFrame({ payload: new Uint8Array([frame]), timestamp: Timestamp.fromMillis(frame * 20) });
  }
  source.close();

  await delay(30);

  assert.equal(source.bufferedFrames, 0);
  assert.deepEqual(drainGroups(track.subscribe()), []);
});

test("read-time age checks reject stale bytes before a throttled timer runs", async () => {
  const track = new TrackProducer("audio").accept();
  const subscriber = track.subscribe({ maxGroups: 1, maxFrames: 5, maxAgeMs: 2 });
  const source = track.appendGroup();
  source.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.fromMillis(0) });
  source.close();
  const held = await subscriber.recvGroup();

  const blockedAt = performance.now();
  while (performance.now() - blockedAt < 8) {
    // Block the event loop so the expiry timer cannot be the mechanism that removes the frame.
  }

  assert.equal(held.tryReadFrame(), undefined);
  assert.equal(held.bufferedFrames, 0);
});

for (const { name, sourceAge, sinkAge } of [
  { name: "omitted", sourceAge: 10, sinkAge: undefined },
  { name: "looser", sourceAge: 10, sinkAge: 1_000 },
  { name: "tighter", sourceAge: 1_000, sinkAge: 10 },
]) {
  test(`held mirrors honor the strictest age with ${name} sink limits and blocked timers`, () => {
    const track = new TrackProducer("audio", { maxAgeMs: sourceAge }).accept();
    const subscriber = track.subscribe({ maxAgeMs: sinkAge });
    const source = track.appendGroup();
    const held = subscriber.tryRecvGroup();
    assert.ok(held);

    // Writing also reprunes the existing mirror, which must retain the source bound.
    source.writeFrame({ payload: new Uint8Array([42]), timestamp: Timestamp.fromMillis(0) });
    source.close();
    blockEventLoop(20);

    assert.equal(held.tryReadFrame(), undefined);
    assert.equal(held.bufferedFrames, 0);
    assert.equal(held.skipped, false);
    assert.equal(held.done, true);
    track.close();
  });

  test(`subscription updates preserve the strictest age with ${name} sink limits`, () => {
    const track = new TrackProducer("audio", { maxAgeMs: sourceAge }).accept();
    const subscriber = track.subscribe({ maxAgeMs: sourceAge / 2 });
    const source = track.appendGroup();
    const held = subscriber.tryRecvGroup();
    assert.ok(held);
    source.writeFrame({ payload: new Uint8Array([42]), timestamp: Timestamp.fromMillis(0) });
    source.close();

    subscriber.update({ maxAgeMs: sinkAge });
    blockEventLoop(20);

    assert.equal(held.tryReadFrame(), undefined);
    assert.equal(held.skipped, false);
    track.close();
  });
}

test("tightening another subscriber propagates the source deadline to existing held mirrors", () => {
  const track = new TrackProducer("audio", { maxAgeMs: 1_000 }).accept();
  const subscriber = track.subscribe();
  const sibling = track.subscribe();
  const source = track.appendGroup();
  const held = subscriber.tryRecvGroup();
  assert.ok(held);
  source.writeFrame({ payload: new Uint8Array([42]), timestamp: Timestamp.fromMillis(0) });
  source.close();

  sibling.update({ maxAgeMs: 10 });
  blockEventLoop(20);

  assert.equal(held.tryReadFrame(), undefined);
  assert.equal(held.skipped, false);
  track.close();
});

test("late mirrors inherit the source's original absolute frame deadline", () => {
  const track = new TrackProducer("audio", { maxAgeMs: 60 }).accept();
  const source = track.appendGroup();
  source.writeFrame({ payload: new Uint8Array([42]), timestamp: Timestamp.fromMillis(0) });
  source.close();
  blockEventLoop(35);

  const held = track.subscribe().tryRecvGroup();
  assert.ok(held);
  blockEventLoop(35);

  assert.equal(held.tryReadFrame(), undefined);
  assert.equal(held.skipped, false);
  track.close();
});

test("a late subscriber to a closed track can retain a tighter age bound", () => {
  const track = new TrackProducer("audio", { maxAgeMs: 1_000 }).accept();
  const source = track.appendGroup();
  source.writeFrame({ payload: new Uint8Array([42]), timestamp: Timestamp.fromMillis(0) });
  source.close();
  track.close();

  const held = track.subscribe({ maxAgeMs: 10 }).tryRecvGroup();
  assert.ok(held);
  blockEventLoop(20);

  assert.equal(held.tryReadFrame(), undefined);
  assert.equal(held.skipped, false);
});

test("producer maxGroups closes and empties an evicted group still held by the caller", () => {
  const track = new TrackProducer("audio", { maxGroups: 1, maxFrames: 5 }).accept();
  const old = track.appendGroup();
  old.writeFrame({ payload: new Uint8Array([0]), timestamp: Timestamp.fromMillis(0) });

  track.appendGroup();

  assert.equal(old.isClosed, true);
  assert.equal(old.bufferedFrames, 0);
  assert.throws(
    () => old.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.fromMillis(20) }),
    /group is closed/,
  );
});

test("local cache limits never enter the aggregate wire subscription", () => {
  const track = new TrackProducer("audio");
  const subscriber = track.subscribe({
    priority: 7,
    ordered: false,
    latencyMax: 200,
    maxGroups: 2,
    maxFrames: 10,
    maxAgeMs: 200,
  });

  assert.deepEqual(track.subscription.peek(), {
    priority: 7,
    ordered: false,
    latencyMax: 200,
    startGroup: undefined,
    endGroup: undefined,
  });
  assert.equal(subscriber.subscription.peek().maxGroups, 2);
  assert.equal(subscriber.subscription.peek().maxFrames, 10);
  assert.equal(subscriber.subscription.peek().maxAgeMs, 200);
});

test("tightening subscriber limits immediately prunes already buffered data", () => {
  const track = new TrackProducer("audio").accept();
  const subscriber = track.subscribe();
  writeVoiceBurst(track);

  subscriber.update({ maxGroups: 2, maxFrames: 10 });

  const pending = drainGroups(subscriber);
  assert.deepEqual(pending.map((group) => group.sequence), [8, 9]);
  assert.equal(pending.reduce((total, group) => total + drainFrameCount(group), 0), 10);
});

test("cache limits reject zero, fractional, and non-finite values", () => {
  for (const value of [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.throws(() => new TrackProducer("audio", { maxGroups: value }), RangeError);
    assert.throws(() => new TrackProducer("audio").subscribe({ maxFrames: value }), RangeError);
  }

  for (const value of [0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.throws(() => new TrackProducer("audio", { maxAgeMs: value }), RangeError);
    assert.throws(() => new TrackProducer("audio").subscribe({ maxAgeMs: value }), RangeError);
  }
});

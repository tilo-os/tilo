import test from 'node:test';
import assert from 'node:assert/strict';
import {
  createPresencePublisher, createRemotePresence, PRESENCE_KEEPALIVE_MS, PRESENCE_REPEATS_MS,
  ENDED_DRAG_TIMEOUT_MS, EXTRAPOLATION_MAX_MS, EXTRAPOLATION_INTERVALS, INTERPOLATION_MIN_MS, INTERPOLATION_MAX_MS,
  PRESENCE_MIN_INTERVAL_MS,
} from './live-presence.mjs';

/** A pointer over strip 1's empty desktop. */
const P = (x, y) => ({strip: '1', at: [x, y]});

function manualClock(start = 0) {
  let current = start;
  let next = 0;
  const timers = new Map();
  const frames = new Map();
  return {
    now: () => current,
    setTimeout(callback, delay) { const id = ++next; timers.set(id, {at: current + Math.max(0, delay), callback}); return id; },
    clearTimeout(id) { timers.delete(id); },
    requestFrame(callback) { const id = ++next; frames.set(id, callback); return id; },
    cancelFrame(id) { frames.delete(id); },
    /** One display refresh: advance and run animation frame callbacks. */
    frame(ms = 1000 / 60) {
      this.advance(ms);
      const due = [...frames.values()];
      frames.clear();
      for (const callback of due) callback(current);
    },
    advance(ms) {
      const end = current + ms;
      for (;;) {
        const due = [...timers.entries()].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        timers.delete(due[0]);
        current = Math.max(current, due[1].at);
        due[1].callback();
      }
      current = end;
    },
    pendingFrames: () => frames.size,
    pendingTimers: () => timers.size,
  };
}

function publisherHarness(options = {}) {
  const clock = manualClock(1000);
  const sent = [];
  const publisher = createPresencePublisher({
    encode: value => new TextEncoder().encode(JSON.stringify(value)),
    send: (bytes, value) => { sent.push(value); return true; },
    now: clock.now,
    wallNow: () => 1_700_000_000_000 + clock.now(),
    setTimer: clock.setTimeout,
    clearTimer: clock.clearTimeout,
    // End of task = the next zero-delay timer of the manual clock.
    defer: callback => clock.setTimeout(callback, 0),
    initialSeq: 0,
    ...options,
  });
  return {clock, sent, publisher};
}

test('publisher coalesces updates made in one task into one frame with monotonic seq', () => {
  const {clock, sent, publisher} = publisherHarness();
  publisher.start();
  for (let i = 0; i < 10; i++) publisher.update({pointer: P(i, i)});
  assert.equal(sent.length, 0, 'nothing leaves inside the task');
  clock.advance(0);
  assert.equal(sent.length, 1);
  assert.deepEqual(sent[0].pointer.at, [9, 9], 'latest value wins');
  publisher.update({pointer: P(10, 10)});
  clock.frame();
  assert.deepEqual(sent.map(f => f.seq), [1, 2]);
  assert.equal(publisher.stats().coalesced, 9);
});

test('publisher sends a change at once and rate-limits the next one to the 120 Hz interval, not a frame', () => {
  const {clock, sent, publisher} = publisherHarness();
  publisher.start();
  publisher.update({pointer: P(1, 1)});
  clock.advance(0);
  assert.equal(sent.length, 1, 'no animation frame wait');
  const first = clock.now();
  publisher.update({pointer: P(2, 2)});
  clock.advance(1);
  assert.equal(sent.length, 1, 'inside the minimum interval');
  publisher.update({pointer: P(3, 3)});
  clock.advance(PRESENCE_MIN_INTERVAL_MS);
  assert.equal(sent.length, 2);
  assert.deepEqual(sent[1].pointer.at, [3, 3], 'the rate-limited frame carries the latest value');
  const gap = sent[1].sentAtMs - sent[0].sentAtMs;
  assert.ok(gap >= Math.floor(PRESENCE_MIN_INTERVAL_MS) && gap <= Math.ceil(PRESENCE_MIN_INTERVAL_MS), `gap ${gap}`);
  assert.ok(first >= 1000);
  // After an idle gap the next change leaves at once again.
  clock.advance(50);
  const before = sent.length;
  publisher.update({pointer: P(4, 4)});
  clock.advance(0);
  assert.equal(sent.length, before + 1);
});

test('publisher caps presence at 120 frames per second on fast displays', () => {
  const {clock, sent, publisher} = publisherHarness();
  publisher.start();
  for (let i = 0; i < 240; i++) { publisher.update({pointer: P(i, 0)}); clock.frame(1000 / 240); }
  // Frames sent inside one second (the loop spans [0, 1000] ms inclusive).
  const published = sent.filter(frame => frame.sentAtMs - sent[0].sentAtMs < 1000).length;
  assert.ok(published <= 120 && published >= 110, `published ${published} in one second`);
});

test('publisher stops when idle after loss-repair repeats and keeps a slow keepalive', () => {
  const {clock, sent, publisher} = publisherHarness();
  publisher.start();
  publisher.update({pointer: P(1, 2)});
  clock.frame();
  assert.equal(sent.length, 1);
  clock.advance(PRESENCE_REPEATS_MS.at(-1) + 1);
  assert.equal(sent.length, 1 + PRESENCE_REPEATS_MS.length, 'repeats repair datagram loss');
  assert.ok(sent.every((f, i) => f.seq === i + 1), 'repeats take fresh sequence numbers');
  assert.equal(clock.pendingFrames(), 0, 'no animation frames while idle');
  clock.advance(PRESENCE_KEEPALIVE_MS * 2 + 1);
  assert.equal(sent.length, 1 + PRESENCE_REPEATS_MS.length + 2, 'slow keepalive only');
  // Unchanged value: no frame.
  assert.equal(publisher.update({pointer: P(1, 2)}), false);
});

test('publisher clears an ended drag after publishing and repeating it', () => {
  const {clock, sent, publisher} = publisherHarness();
  publisher.start();
  const object = {record: null, id: '0a'};
  publisher.update({drag: {object, phase: 'move', position: [1, 1], size: null}});
  clock.frame();
  publisher.update({drag: {object, phase: 'end', position: [5, 5], size: null}});
  clock.frame();
  clock.advance(PRESENCE_REPEATS_MS.at(-1) + 1);
  clock.frame();
  const phases = sent.map(f => f.drag?.phase ?? null);
  assert.deepEqual(phases.slice(0, 4), ['move', 'end', 'end', 'end']);
  assert.equal(phases.at(-1), null, 'drag cleared after end was delivered');
});

test('publisher pauses without transport and republishes state on start', () => {
  const {clock, sent, publisher} = publisherHarness();
  publisher.update({pointer: P(3, 4)});
  clock.frame();
  assert.equal(sent.length, 0, 'not started');
  publisher.start();
  clock.frame();
  assert.equal(sent.length, 1);
  publisher.pause();
  publisher.update({pointer: P(5, 6)});
  clock.frame();
  clock.advance(10_000);
  assert.equal(sent.length, 1);
  publisher.start();
  clock.frame();
  assert.deepEqual(sent.at(-1).pointer.at, [5, 6]);
  publisher.stop();
  assert.equal(publisher.update({pointer: P(0, 0)}), false);
});

test('publisher rejects unknown fields and non-finite pointers', () => {
  const {publisher} = publisherHarness();
  assert.throws(() => publisher.update({cursor: [1, 1]}));
  assert.throws(() => publisher.update({pointer: P(NaN, 1)}));
  assert.throws(() => publisher.update({pointer: [1, 1]}), 'a bare point is over nothing');
  assert.throws(() => publisher.update({pointer: {strip: 'x', at: [1, 1]}}));
  assert.throws(() => publisher.update({drag: {object: {id: '01'}, phase: 'fly', position: [0, 0]}}));
});

const frame = (seq, sentAtMs, extra = {}) => ({seq, sentAtMs, focus: null, pointer: null, selection: null, drag: null, ...extra});

test('receiver drops frames at or below the newest sequence', () => {
  const remote = createRemotePresence();
  assert.equal(remote.accept(frame(5, 100, {pointer: P(5, 5)}), 110), true);
  assert.equal(remote.accept(frame(4, 90, {pointer: P(4, 4)}), 111), false, 'reordered older frame');
  assert.equal(remote.accept(frame(5, 100, {pointer: P(5, 5)}), 112), false, 'duplicate');
  assert.equal(remote.accept(frame(6, 116, {pointer: P(6, 6)}), 126), true);
  assert.deepEqual(remote.stats(), {accepted: 2, stale: 1, duplicate: 1});
  assert.equal(remote.lastSeq(), 6);
  // A restarted publisher (lower seq, clearly newer send time) is accepted.
  assert.equal(remote.accept(frame(1, 5000, {pointer: P(9, 9)}), 5010), true);
  assert.deepEqual(remote.sample(5100).pointer.at, [9, 9]);
});

test('publisher seeds sequence numbers from the wall clock by default', () => {
  const sent = [];
  const publisher = createPresencePublisher({encode: v => new Uint8Array(1), send: (b, v) => sent.push(v), wallNow: () => 1_700_000_000_000,
    defer: cb => cb(), setTimer: () => 1, clearTimer() {}});
  publisher.start();
  publisher.update({pointer: P(1, 1)});
  assert.equal(sent[0].seq, 1_700_000_000_000_000 + 1);
  assert.ok(Number.isSafeInteger(sent[0].seq));
});

test('receiver renders a clean link at the minimum delay and interpolates between frames', () => {
  const remote = createRemotePresence();
  // Publisher at 60 Hz, x advances 10 per frame; constant 20 ms one-way delay.
  for (let i = 0; i < 6; i++) remote.accept(frame(i + 1, 1000 + i * 16, {pointer: P(i * 10, 0)}), 1020 + i * 16);
  const delay = remote.interpolationDelayMs();
  assert.equal(delay, INTERPOLATION_MIN_MS, 'no jitter: floor delay');
  // Render time 8 ms after sample 3's (mapped) arrival -> halfway to sample 4.
  const at = 1020 + 3 * 16 + 8 + delay;
  const {pointer} = remote.sample(at);
  assert.ok(Math.abs(pointer.at[0] - 35) < 0.01, `x=${pointer.at[0]}`);
  // Monotonic across render frames at 120 Hz, including the extrapolated
  // stretch between arrivals.
  let previous = -Infinity;
  for (let t = 1040; t <= 1020 + 5 * 16 + delay; t += 1000 / 120) {
    const x = remote.sample(t).pointer.at[0];
    assert.ok(x >= previous - 1e-9, 'no backwards jumps');
    previous = x;
  }
});

test('receiver delay follows measured arrival jitter within bounds', () => {
  const remote = createRemotePresence();
  // 120 Hz publisher; arrivals late by 0..6 ms (alternating).
  for (let i = 0; i < 40; i++) remote.accept(frame(i + 1, i * 8, {pointer: P(i, 0)}), 20 + i * 8 + (i % 2 ? 6 : 0));
  const jittery = remote.interpolationDelayMs();
  assert.ok(jittery > 4 && jittery <= 1.5 * 6 + 0.01, `delay ${jittery}`);
  // A very jittery link is capped.
  const bad = createRemotePresence();
  for (let i = 0; i < 20; i++) bad.accept(frame(i + 1, i * 16, {pointer: P(i, 0)}), 20 + i * 16 + (i % 2 ? 80 : 0));
  assert.equal(bad.interpolationDelayMs(), INTERPOLATION_MAX_MS);
  // The link cleans up: the delay decays back towards the floor.
  let t = 20 * 16;
  for (let i = 20; i < 400; i++, t += 8) bad.accept(frame(i + 1, t, {pointer: P(i, 0)}), 20 + t);
  assert.ok(bad.interpolationDelayMs() < 6, `decayed to ${bad.interpolationDelayMs()}`);
});

test('receiver keeps a smooth path at the minimum delay (interpolation error vs the true path)', () => {
  const remote = createRemotePresence();
  // True path x(t) = t (1 px/ms), published at 120 Hz with 10 ms one-way delay
  // and +-1 ms scheduling noise; sampled every 1000/120 ms on the receiver.
  const noise = i => [0, 1, 0.5, 0, 1][i % 5];
  let worst = 0;
  let received = 0;
  for (let step = 0; step < 240; step++) {
    const renderAt = 100 + step * (1000 / 120);
    while (received * (1000 / 120) + 10 + noise(received) <= renderAt) {
      const sent = Math.round(received * (1000 / 120));
      remote.accept(frame(received + 1, sent, {pointer: P(received * (1000 / 120), 0)}), received * (1000 / 120) + 10 + noise(received));
      received++;
    }
    const sample = remote.sample(renderAt);
    if (!sample?.pointer || step < 10) continue;
    // Expected: the true position (renderAt - one-way delay - render delay).
    const expected = renderAt - 10 - remote.interpolationDelayMs();
    worst = Math.max(worst, Math.abs(sample.pointer.at[0] - expected));
  }
  assert.ok(worst < 3, `max error ${worst.toFixed(2)} px at 1000 px/s`);
});

test('receiver extrapolates briefly on loss, then holds', () => {
  const remote = createRemotePresence();
  for (let i = 0; i < 4; i++) remote.accept(frame(i + 1, i * 16, {pointer: P(i * 16, 0)}), i * 16);
  const delay = remote.interpolationDelayMs();
  const last = 3 * 16;
  const interval = remote.intervalMs();
  const ahead = remote.sample(last + delay + interval).pointer.at[0];
  assert.ok(Math.abs(ahead - (last + interval)) < 0.5, `extrapolated ${ahead}`);
  const far = remote.sample(last + delay + 1000).pointer.at[0];
  const cap = Math.min(EXTRAPOLATION_MAX_MS, EXTRAPOLATION_INTERVALS * interval);
  assert.ok(Math.abs(far - (last + cap)) < 0.5, `bounded extrapolation ${far}`);
});

test('receiver snaps on drag end/cancel and keeps ended drags until durable change or timeout', () => {
  const remote = createRemotePresence();
  const object = {record: null, id: '0a'};
  remote.accept(frame(1, 0, {drag: {object, phase: 'start', position: [0, 0], size: null}}), 0);
  remote.accept(frame(2, 16, {drag: {object, phase: 'move', position: [16, 0], size: null}}), 16);
  const moving = remote.sample(8 + remote.interpolationDelayMs());
  assert.equal(moving.drag.snapped, false);
  assert.ok(moving.drag.position[0] < 16, 'interpolated');
  remote.accept(frame(3, 32, {drag: {object, phase: 'end', position: [100, 50], size: null}}), 32);
  const ended = remote.sample(33);
  assert.deepEqual(ended.drag.position, [100, 50], 'snapped immediately');
  assert.equal(ended.drag.snapped, true);
  remote.accept(frame(4, 300, {}), 300);
  const after = remote.sample(301);
  assert.equal(after.drag, null);
  assert.deepEqual(after.endedDrags.map(d => d.position), [[100, 50]], 'ended drag kept for rendering');
  remote.snap('0a');
  assert.deepEqual(remote.sample(302).endedDrags, [], 'durable change drops it');
  remote.accept(frame(5, 400, {drag: {object, phase: 'cancel', position: [1, 1], size: null}}), 400);
  remote.accept(frame(6, 410, {}), 410);
  assert.equal(remote.sample(420).endedDrags.length, 1);
  assert.equal(remote.sample(410 + ENDED_DRAG_TIMEOUT_MS + 1).endedDrags.length, 0, 'timeout');
});

test('receiver honours a cleared pointer immediately and reports staleness', () => {
  const remote = createRemotePresence();
  remote.accept(frame(1, 0, {pointer: P(1, 1)}), 0);
  remote.accept(frame(2, 16, {pointer: null}), 16);
  assert.equal(remote.sample(17).pointer, null);
  assert.equal(remote.sample(17).stale, false);
  assert.equal(remote.sample(20_000).stale, true);
});

test('a pointer moves smoothly over one window and jumps to what it moved over', () => {
  const inWindow = at => ({window: {app: 'a', record: 'one'}, at});
  const remote = createRemotePresence();
  for (let i = 0; i < 4; i++) remote.accept(frame(i + 1, i * 16, {pointer: inWindow([i * 16, 0])}), i * 16);
  const delay = remote.interpolationDelayMs();
  const between = remote.sample(2 * 16 + 8 + delay).pointer;
  assert.deepEqual(between.window, {app: 'a', record: 'one'});
  assert.ok(Math.abs(between.at[0] - 40) < 0.01, 'interpolated within the window');
  remote.accept(frame(5, 64, {pointer: P(500, 300)}), 64);
  for (const at of [64, 70, 120]) {
    assert.deepEqual(remote.sample(at + delay).pointer, P(500, 300), 'no motion carried from the window into the strip');
  }
});

test('a pointer beside a window moves smoothly along it and stays on its side', () => {
  const chat = {app: 'so.tilo.chat', record: ''};
  const beside = (fy, dx) => ({beside: chat, edge: [1, fy], at: [dx, 0]});
  const remote = createRemotePresence();
  for (let i = 0; i < 4; i++) remote.accept(frame(i + 1, i * 16, {pointer: beside(i * 0.25, 40)}), i * 16);
  const delay = remote.interpolationDelayMs();
  const between = remote.sample(2 * 16 + 8 + delay).pointer;
  assert.deepEqual(between.beside, chat);
  assert.ok(Math.abs(between.edge[1] - 0.625) < 1e-9, 'interpolated along the side');
  assert.deepEqual(between.at, [40, 0]);
  const ahead = remote.sample(3 * 16 + delay + 1000).pointer;
  assert.equal(ahead.edge[1], 1, 'extrapolation keeps to the side');
  // Its own page normalizes what it publishes; a bad edge is refused.
  const {publisher} = publisherHarness();
  assert.equal(publisher.update({pointer: beside(0.5, 40)}), true);
  assert.throws(() => publisher.update({pointer: {beside: chat, edge: [2, 0], at: [1, 1]}}));
  assert.throws(() => publisher.update({pointer: {beside: chat, at: [1, 1]}}), 'beside a window needs an edge');
});

test('a change of focused app or record snaps without carrying old-surface motion', () => {
  for (const focus of [{app:'a',record:'other'}, {app:'b',record:'one'}, null]) {
    const remote = createRemotePresence();
    remote.accept(frame(1, 0, {focus:{app:'a',record:'one'},pointer: P(900,900)}), 0);
    remote.accept(frame(2, 16, {focus,pointer: P(10,20)}), 16);
    for (const at of [16,20,60]) assert.deepEqual(remote.sample(at).pointer.at, [10,20]);
  }
});

test("receiver times a sent cursor-chat message on its own clock and plays each reaction once", () => {
  const remote = createRemotePresence();
  remote.accept(frame(1, 100, {pointer: P(1, 1), chat: {text: 'hel', sent: false}}), 110);
  assert.deepEqual(remote.sample(120).chat, {text: 'hel', sent: false});
  assert.equal(remote.sample(120).chatSentAt, null, 'being typed');
  remote.accept(frame(2, 200, {pointer: P(1, 1), chat: {text: 'hello', sent: true}}), 210);
  assert.equal(remote.sample(220).chatSentAt, 210, 'sent when it arrived here');
  remote.accept(frame(3, 3200, {pointer: P(1, 1), chat: {text: 'hello', sent: true}}), 3210);
  assert.equal(remote.sample(3220).chatSentAt, 210, 'a keepalive does not restart it');
  remote.accept(frame(4, 3300, {pointer: P(1, 1), reaction: {seq: 7, emoji: '\u{1f44d}'}}), 3310);
  assert.deepEqual(remote.sample(3320).reaction, {seq: 7, emoji: '\u{1f44d}', at: 3310});
  assert.equal(remote.sample(3320).chat, null, 'the message was cleared');
  remote.accept(frame(5, 3400, {pointer: P(2, 2), reaction: {seq: 7, emoji: '\u{1f44d}'}}), 3410);
  assert.equal(remote.sample(3420).reaction.at, 3310, 'a restated reaction is the same one');
});

test('publisher carries a wave at one member, restated with the frame', () => {
  const {clock, sent, publisher} = publisherHarness();
  publisher.start();
  publisher.update({wave: {seq: 2, target: 'ab12'}});
  clock.advance(0);
  assert.deepEqual(sent.at(-1).wave, {seq: 2, target: 'ab12'});
  publisher.update({pointer: P(1, 1)});
  clock.frame();
  assert.deepEqual(sent.at(-1).wave, {seq: 2, target: 'ab12'}, 'the same wave, which shows nothing again');
});

test('publisher carries where its windows are, by app and digest only', () => {
  const {clock, sent, publisher} = publisherHarness();
  publisher.start();
  const layout = {size: [1440, 852], windows: [
    {app: 'so.tilo.notes', record: '0123456789abcdef', rect: [0, 0, 5000, 10000], focused: true, title: 'never sent'},
    {app: 'x'.repeat(49), record: '0123456789abcdef', rect: [0, 0, 1, 1]},
    {app: 'so.tilo.chat', record: 'fedcba9876543210', rect: [12000, -3, 9000, 4.4]},
  ]};
  publisher.update({layout});
  clock.advance(0);
  assert.deepEqual(sent.at(-1).layout, {size: [1440, 852], windows: [
    {app: 'so.tilo.notes', record: '0123456789abcdef', rect: [0, 0, 5000, 10000], focused: true},
    {app: 'so.tilo.chat', record: 'fedcba9876543210', rect: [10000, 0, 10000, 4], focused: false},
  ]});
  assert.throws(() => publisher.update({layout: {size: [1, 1], windows: [{app: 'a', record: 'not hex', rect: [0, 0, 1, 1]}]}}));
});

test('publisher says whom it follows while it lasts, and stops saying so', () => {
  const {clock, sent, publisher} = publisherHarness();
  publisher.start();
  publisher.update({following: 'ab12cd'});
  clock.advance(0);
  assert.equal(sent.at(-1).following, 'ab12cd');
  publisher.update({pointer: P(1, 1)});
  clock.frame();
  assert.equal(sent.at(-1).following, 'ab12cd', 'restated with every frame');
  publisher.update({following: null});
  clock.frame();
  assert.equal(sent.at(-1).following, null);
  assert.throws(() => publisher.update({following: 'Not Hex'}));
});

import test from "node:test";
import assert from "node:assert/strict";
import {
  summarize,
  scrollResult,
  FrameParser,
  pacingSteps,
} from "./benchmark-metrics.mjs";

test("failed and non-finite samples never become zero-time successes", () => {
  assert.deepEqual(summarize([10, 20, null, NaN, -1, Infinity, 30]), {
    count: 3,
    median: 20,
    p95: 30,
    min: 10,
    max: 30,
  });
  assert.equal(summarize([]), null);
});
test("scroll success requires actual movement and a response after input", () => {
  assert.equal(
    scrollResult({ startY: 0, endY: 0, sent: 10, first: 12, settled: 20 })
      .valid,
    false,
  );
  assert.equal(
    scrollResult({ startY: 0, endY: 100, sent: 10, first: 9, settled: 20 })
      .valid,
    false,
  );
  assert.deepEqual(
    scrollResult({ startY: 0, endY: 120, sent: 10, first: 22, settled: 180 }),
    { valid: true, movedPixels: 120, responseMs: 12, settleMs: 170 },
  );
});
test("framing handles arbitrary chunks and rejects excessive lengths/dimensions", () => {
  const packets = [];
  const parser = new FrameParser((x) => packets.push(x));
  const frame = Buffer.alloc(17);
  frame.writeUInt32LE(13);
  frame[4] = 2;
  frame.writeUInt32LE(1, 5);
  frame.writeUInt32LE(1, 9);
  for (const byte of frame) parser.push(Buffer.from([byte]));
  assert.equal(packets.length, 1);
  assert.equal(packets[0].width, 1);
  assert.equal(packets[0].height, 1);
  const huge = Buffer.alloc(4);
  huge.writeUInt32LE(64 * 1024 * 1024 + 1);
  assert.throws(() => new FrameParser(() => {}).push(huge), /length/);
  frame.writeUInt32LE(9000, 5);
  assert.throws(() => new FrameParser(() => {}).push(frame), /frame/);
});

test("continuous pacing reverses before short-page boundaries", () => {
  const steps = pacingSteps(414);
  let y = 0,
    max = 0;
  for (const dy of steps) {
    y -= dy;
    max = Math.max(max, y);
    assert.ok(y >= 0 && y < 414);
  }
  assert.equal(y, 0);
  assert.ok(max > 0);
  assert.equal(steps.length, 60);
  assert.equal(pacingSteps(10), null);
});

// Import the harness without launching CEF so reports can be checked offline.
import * as metrics from "./benchmark-metrics.mjs";
import { markdown } from "./benchmark-pages.mjs";

test("warm reset failure retains prior samples and records the failed run", async () => {
  const result = await metrics.collectRuns({
    count: 3,
    createSample: (run) => ({ run: run + 1, cache: run ? "warm" : "fresh" }),
    reset: async () => { throw Error("reset timeout"); },
    measure: async (sample) => { sample.loadValid = true; },
  });
  assert.equal(result.runs.length, 2);
  assert.equal(result.runs[0].loadValid, true);
  assert.equal(result.runs[1].loadValid, false);
  assert.equal(result.runs[1].failurePhase, "reset");
  assert.match(result.runs[1].error, /reset timeout/);
  assert.match(result.error, /reset timeout/);
  const report = markdown({ recordedAt: "now", environment: {}, sites: [
    { site: { name: "partial" }, ...result },
    { site: { name: "startup" }, runs: [], error: "host failed" },
  ] });
  assert.match(report, /partial.*reset timeout/);
  assert.match(report, /startup.*host failed/);
});

test("final document load must match and its HTTP status must succeed", () => {
  const url = "https://example.com/final";
  const sample = () => ({ document: { url }, httpStatus: 200 });
  const event = (at, status, eventUrl = url) => ({ at, value: { event: "loaded", url: eventUrl, status } });
  const failed = sample();
  assert.throws(() => metrics.validateFinalLoad(failed, [event(12, 200, "https://example.com/"), event(30, 404)], 10), /Unsuccessful/);
  assert.equal(failed.httpStatus, 404);
  assert.equal(failed.requestToLoadEventMs, 20);
  assert.throws(() => metrics.validateFinalLoad(sample(), [event(9, 200), event(12, 200, "https://example.com/")], 10), /No matching/);
  const passed = sample();
  metrics.validateFinalLoad(passed, [event(12, 200), event(30, 201)], 10);
  assert.equal(passed.httpStatus, 201);
  assert.equal(passed.requestToLoadEventMs, 20);
});

test("raw frame timings reproduce pacing summaries after JSON serialization", () => {
  const result = JSON.parse(JSON.stringify(metrics.framePacingMetrics([
    { at: 99, width: 1280 }, { at: 100, width: 1280 },
    { at: 110, width: 640 }, { at: 116.5, width: 1280 },
    { at: 150, width: 1280 }, { at: 161, width: 1280 },
  ], 100, 160)));
  assert.deepEqual(result.frameTimestampsMs, [100, 116.5, 150]);
  assert.deepEqual(result.rawFrameIntervalsMs, [16.5, 33.5]);
  assert.deepEqual(result.frameIntervalsMs, summarize(result.rawFrameIntervalsMs));
  assert.equal(result.frames, result.frameTimestampsMs.length);
  assert.equal(result.deliveredFps, 2000 / 50);
});

test("post-load measurement failures preserve load validity and allow later runs", async () => {
  const result = await metrics.collectRuns({
    count: 2,
    createSample: (run) => ({ run: run + 1 }),
    reset: async () => {},
    measure: async (sample) => {
      sample.loadValid = true;
      throw Error("scroll failed");
    },
  });
  assert.equal(result.runs.length, 2);
  assert.equal(result.error, undefined);
  for (const sample of result.runs) {
    assert.equal(sample.loadValid, true);
    assert.equal(sample.failurePhase, "measure");
    assert.match(sample.error, /scroll failed/);
  }
});

test("tabbed benchmark frames require a positive signed native tab ID", () => {
  const frame = Buffer.alloc(21);
  frame.writeUInt32LE(17);
  frame[4] = 3;
  frame.writeUInt32LE(1, 9);
  frame.writeUInt32LE(1, 13);
  for (const id of [0, 0x80000000, 0xffffffff]) {
    frame.writeUInt32LE(id, 5);
    assert.throws(() => new FrameParser(() => {}).push(frame), /tab ID/);
  }
  for (const id of [1, 0x7fffffff]) {
    frame.writeUInt32LE(id, 5);
    const packets = [];
    new FrameParser((packet) => packets.push(packet)).push(frame);
    assert.equal(packets.length, 1);
    assert.equal(packets[0].tab, id);
  }
});

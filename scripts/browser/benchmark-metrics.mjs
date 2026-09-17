import { performance } from "node:perf_hooks";
export function pacingSteps(scrollRange) {
  if (!Number.isFinite(scrollRange) || scrollRange < 60) return null;
  const delta = Math.min(30, Math.floor(scrollRange / 40));
  return Array.from({ length: 60 }, (_, index) =>
    index < 30 ? -delta : delta,
  );
}
export function summarize(values) {
  const sorted = values
    .filter((x) => typeof x === "number" && Number.isFinite(x) && x >= 0)
    .sort((a, b) => a - b);
  if (!sorted.length) return null;
  const middle = (sorted.length - 1) / 2;
  return {
    count: sorted.length,
    median: (sorted[Math.floor(middle)] + sorted[Math.ceil(middle)]) / 2,
    p95: sorted[Math.ceil(sorted.length * 0.95) - 1],
    min: sorted[0],
    max: sorted.at(-1),
  };
}
export function scrollResult({ startY, endY, sent, first, settled }) {
  if (
    ![startY, endY, sent, first, settled].every(Number.isFinite) ||
    Math.abs(endY - startY) < 1 ||
    first < sent ||
    settled < first
  )
    return {
      valid: false,
      reason: "No valid observed scroll movement/response",
    };
  return {
    valid: true,
    movedPixels: endY - startY,
    responseMs: first - sent,
    settleMs: settled - sent,
  };
}
// Drain pixel bytes without copying full frames; benchmark parsing must not
// introduce quadratic Buffer.concat overhead into the measured frame pacing.
export class FrameParser {
  constructor(onPacket) {
    this.onPacket = onPacket;
    this.header = Buffer.alloc(4);
    this.headerUsed = 0;
    this.length = 0;
    this.used = 0;
    this.prefix = Buffer.alloc(13);
    this.json = [];
  }
  push(chunk) {
    let offset = 0;
    while (offset < chunk.length) {
      if (!this.length) {
        const n = Math.min(4 - this.headerUsed, chunk.length - offset);
        chunk.copy(this.header, this.headerUsed, offset, offset + n);
        this.headerUsed += n;
        offset += n;
        if (this.headerUsed < 4) continue;
        this.length = this.header.readUInt32LE();
        if (this.length < 1 || this.length > 64 * 1024 * 1024)
          throw Error("Invalid packet length");
        this.used = 0;
        this.json = [];
      }
      if (offset === chunk.length) break;
      const n = Math.min(this.length - this.used, chunk.length - offset);
      if (this.used < 13)
        chunk.copy(
          this.prefix,
          this.used,
          offset,
          offset + Math.min(n, 13 - this.used),
        );
      const kind = this.prefix[0];
      if (![1, 2, 3].includes(kind)) throw Error("Unknown frame kind");
      if (kind === 1) {
        if (this.length > 4 * 1024 * 1024)
          throw Error("JSON length exceeds benchmark event bound");
        this.json.push(Buffer.from(chunk.subarray(offset, offset + n)));
      }
      this.used += n;
      offset += n;
      if (this.used === this.length) {
        const at = performance.now();
        if (kind === 1) {
          const body = Buffer.concat(this.json);
          this.onPacket({
            kind: "event",
            at,
            value: JSON.parse(body.subarray(1).toString()),
          });
        } else {
          const base = kind === 2 ? 1 : 5;
          const width = this.prefix.readUInt32LE(base),
            height = this.prefix.readUInt32LE(base + 4);
          if (
            !width ||
            !height ||
            width > 8192 ||
            height > 8192 ||
            this.length !== base + 8 + width * height * 4
          )
            throw Error("Invalid frame dimensions/size");
          const tab = kind === 2 ? 1 : this.prefix.readUInt32LE(1);
          if (tab < 1 || tab > 0x7fffffff)
            throw Error("Invalid native tab ID");
          this.onPacket({
            kind: "frame",
            at,
            width,
            height,
            tab,
          });
        }
        this.length = 0;
        this.headerUsed = 0;
      }
    }
  }
}

export async function collectRuns({ count, createSample, reset, measure, onSample = () => {}, runs = [] }) {
  const result = { runs };
  for (let run = 0; run < count; run++) {
    const sample = createSample(run);
    runs.push(sample);
    let phase = run ? "reset" : "measure";
    try {
      if (run) await reset();
      phase = "measure";
      await measure(sample, run);
    } catch (error) {
      sample.error = String(error);
      sample.failurePhase = phase;
      sample.loadValid ??= false;
      if (phase === "reset") result.error = `Warm reset failed: ${error}`;
    }
    onSample(sample);
    if (result.error) break;
  }
  return result;
}

export function validateFinalLoad(sample, events, start) {
  const finalLoad = events.filter((event) =>
    event.at >= start && event.value.event === "loaded" &&
    event.value.url === sample.document.url,
  ).at(-1);
  if (!finalLoad) {
    sample.httpStatus = null;
    throw Error("No matching final document load event: " + sample.document.url);
  }
  sample.requestToLoadEventMs = finalLoad.at - start;
  sample.httpStatus = finalLoad.value.status ?? null;
  if (!(sample.httpStatus >= 200 && sample.httpStatus < 400))
    throw Error("Unsuccessful final document load: " + JSON.stringify(finalLoad.value));
}

export function framePacingMetrics(frames, start, end) {
  const frameTimestampsMs = frames.filter((frame) =>
    frame.at >= start && frame.at <= end && frame.width === 1280,
  ).map((frame) => frame.at);
  const rawFrameIntervalsMs = frameTimestampsMs.slice(1).map((at, i) => at - frameTimestampsMs[i]);
  return {
    frameTimestampsMs,
    rawFrameIntervalsMs,
    frames: frameTimestampsMs.length,
    deliveredFps: frameTimestampsMs.length > 1
      ? ((frameTimestampsMs.length - 1) * 1000) / (frameTimestampsMs.at(-1) - frameTimestampsMs[0])
      : null,
    frameIntervalsMs: summarize(rawFrameIntervalsMs),
  };
}

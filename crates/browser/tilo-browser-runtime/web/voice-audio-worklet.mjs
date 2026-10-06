/**
 * The call's audio thread: cuts the captured input into 20 ms frames for the
 * codec worker (bounded by credits it returns), and plays each remote
 * publisher's decoded frames through an adaptive jitter buffer with
 * loss concealment, mixed into one output. Reports speaking levels.
 */
const DEFAULT_FRAME_SAMPLES = 960;
const DEFAULT_CAPTURE_CREDITS = 4;
const MAX_PLAYBACK_SAMPLES = 9_600;
const MIN_JITTER_SAMPLES = 1_920;
const TARGET_JITTER_SAMPLES = 2_880;
const MAX_JITTER_SAMPLES = 5_760;
const MAX_REMOTE_PUBLISHERS = 7;
// Adaptive playout keeps the last DELAY_WINDOW frame delays (arrival render
// time minus sender timestamp, ~2 s) and targets their 95th percentile above
// the window minimum: frames arriving within that spread play on time, the
// buffer shrinks as soon as the link calms down, and reordering (common with
// datagrams on jittery links) is absorbed without resynchronising on a late
// frame.
const DELAY_WINDOW = 100;
const DELAY_PERCENTILE = 0.95;
const MIN_DELAY_SAMPLES = 5;
// Packet-loss concealment: an unrepaired gap after a played frame repeats that
// frame with a falling gain for at most two frames instead of a hard dropout.
const PLC_MAX_FRAMES = 2;
// Latency trim: a continuous stream never drains the queue, so a mapping set
// during startup (or a past jitter burst) would keep its excess delay forever.
// When the playout offset exceeds the adaptive target by 1.5 frames, one quiet
// arriving frame is dropped and later frames are scheduled a frame earlier.
const QUIET_PEAK = 0.01;
const TRIM_EXCESS_FRAMES = 1.5;
// Speaking levels: the RMS of the captured input and of each publisher's
// played audio, posted every LEVEL_SAMPLES (100 ms).
const LEVEL_SAMPLES = 4_800;

class VoiceAudioProcessor extends AudioWorkletProcessor {
  constructor({processorOptions = {}} = {}) {
    super();
    this.frameSamples = processorOptions.frameSamples ?? DEFAULT_FRAME_SAMPLES;
    this.maxCaptureCredits = processorOptions.captureCredits ?? DEFAULT_CAPTURE_CREDITS;
    this.captureCredits = this.maxCaptureCredits;
    this.captureDrops = 0;
    this.muted = Boolean(processorOptions.muted);
    this.captureGeneration = processorOptions.captureGeneration ?? 0;
    this.capture = new Float32Array(this.frameSamples);
    this.captureLength = 0;
    this.captureTimestamp = 0n;
    this.sampleTimestamp = 0n;
    this.publishers = new Map();
    this.renderTimestamp = 0n;
    // Playout delay. Fixed at 60 ms unless the controller enables the adaptive
    // jitter buffer: then each publisher starts at `initial` and re-targets at
    // every resynchronisation to ~3x its measured interarrival jitter plus
    // 10 ms, bounded by [min, max]. Late arrivals raise the estimate at once.
    const jitter = processorOptions.jitter ?? {};
    this.jitterAdaptive = jitter.adaptive === true;
    this.jitterInitial = Number.isInteger(jitter.initial) ? jitter.initial : TARGET_JITTER_SAMPLES;
    this.jitterMin = Number.isInteger(jitter.min) ? jitter.min : MIN_JITTER_SAMPLES;
    this.jitterMax = Number.isInteger(jitter.max) ? Math.min(jitter.max, MAX_PLAYBACK_SAMPLES - this.frameSamples) : MAX_JITTER_SAMPLES;
    this.levelSamples = 0;
    this.captureEnergy = 0;
    this.captureEnergySamples = 0;
    this.port.addEventListener('message', event => this.handleMessage(event.data));
    this.port.start?.();
  }

  handleMessage(message) {
    if (message.type === 'muted') {
      this.muted = Boolean(message.value);
      this.captureGeneration = message.generation;
      this.captureLength = 0;
      return;
    }
    if (message.type === 'capture-generation') {
      this.captureGeneration = message.generation;
      this.captureLength = 0;
      return;
    }
    if (message.type === 'capture-credit') {
      if (this.captureDrops > 0) {
        this.port.postMessage({type: 'drop', stage: 'capture-worklet', count: this.captureDrops});
        this.captureDrops = 0;
      }
      this.captureCredits = Math.min(this.maxCaptureCredits, this.captureCredits + 1);
      return;
    }
    if (message.type === 'remove-publisher') {
      this.removePublisher(message.publisherId, message.generation);
      return;
    }
    if (message.type !== 'play') return;
    const samples = new Float32Array(message.samples);
    const reservedSamples = message.reservedSamples ?? samples.length;
    if (samples.length !== reservedSamples) {
      this.drop(message, 'playback-buffer', reservedSamples);
      return;
    }
    let publisher = this.publishers.get(message.publisherId);
    if (publisher && publisher.generation !== message.generation) {
      this.removePublisher(message.publisherId, publisher.generation);
      publisher = undefined;
    }
    if (!publisher) {
      if (this.publishers.size >= MAX_REMOTE_PUBLISHERS) {
        this.drop(message, 'playback-publisher-limit', reservedSamples);
        return;
      }
      publisher = {
        publisherId: message.publisherId,
        generation: message.generation,
        baseRemoteTimestamp: message.timestamp,
        baseLocalTimestamp: this.renderTimestamp + BigInt(this.jitterInitial),
        bufferedSamples: 0,
        lastRenderedTimestamp: null,
        queue: [],
        jitter: 0,
        lastArrival: null,
        lastRemote: null,
        target: this.jitterInitial,
        late: 0,
        delays: new Float64Array(DELAY_WINDOW),
        delayCount: 0,
        delayIndex: 0,
        offset: null,
        lastFrame: null,
        lastEnd: null,
        energy: 0,
      };
      this.publishers.set(message.publisherId, publisher);
    }
    if ((publisher.lastRenderedTimestamp !== null && message.timestamp <= publisher.lastRenderedTimestamp)
      || publisher.queue.some(entry => entry.timestamp === message.timestamp)) {
      this.drop(message, 'playback-replay', reservedSamples);
      return;
    }
    if (publisher.bufferedSamples + samples.length > MAX_PLAYBACK_SAMPLES) {
      this.drop(message, 'playback-buffer', reservedSamples);
      return;
    }
    if (this.jitterAdaptive) this.observeArrival(publisher, message.timestamp);
    let localTimestamp = publisher.baseLocalTimestamp + (message.timestamp - publisher.baseRemoteTimestamp);
    if (this.jitterAdaptive && localTimestamp < this.renderTimestamp) {
      // Missed its playout slot: grow the estimate so the next resync covers it.
      publisher.late++;
      publisher.jitter = Math.max(publisher.jitter, Math.min(this.jitterMax, Number(this.renderTimestamp - localTimestamp)));
    }
    if (publisher.queue.length === 0 && publisher.lastRenderedTimestamp !== null) {
      if (this.jitterAdaptive && publisher.delayCount >= MIN_DELAY_SAMPLES) {
        // Re-base only while nothing is queued (the jump is not audible
        // mid-frame), and only when the mapping has drifted a frame or more
        // from the offset the observed delay distribution calls for.
        const {offset, target} = this.adaptiveOffset(publisher);
        const current = Number(publisher.baseLocalTimestamp - publisher.baseRemoteTimestamp);
        if (localTimestamp + BigInt(samples.length) < this.renderTimestamp - BigInt(this.jitterMax + MAX_JITTER_SAMPLES)) {
          this.drop(message, 'playback-expired', reservedSamples);
          return;
        }
        if (Math.abs(current - offset) >= this.frameSamples) {
          publisher.target = target;
          publisher.baseRemoteTimestamp = message.timestamp;
          publisher.baseLocalTimestamp = message.timestamp + BigInt(offset);
          localTimestamp = publisher.baseLocalTimestamp;
        }
      } else {
        const distance = localTimestamp - this.renderTimestamp;
        const target = this.jitterAdaptive ? this.adaptiveTarget(publisher) : TARGET_JITTER_SAMPLES;
        const minimum = this.jitterAdaptive ? Math.round(target * 2 / 3) : MIN_JITTER_SAMPLES;
        const maximum = this.jitterAdaptive ? target * 2 : MAX_JITTER_SAMPLES;
        if (localTimestamp + BigInt(samples.length) < this.renderTimestamp - BigInt(Math.max(maximum, MAX_JITTER_SAMPLES))) {
          this.drop(message, 'playback-expired', reservedSamples);
          return;
        }
        if (distance < BigInt(minimum) || distance > BigInt(maximum)) {
          publisher.target = target;
          publisher.baseRemoteTimestamp = message.timestamp;
          publisher.baseLocalTimestamp = this.renderTimestamp + BigInt(target);
          localTimestamp = publisher.baseLocalTimestamp;
        }
      }
    }
    if (this.jitterAdaptive && publisher.delayCount >= MIN_DELAY_SAMPLES && publisher.queue.length > 0
      && localTimestamp >= this.renderTimestamp) {
      const {offset} = this.adaptiveOffset(publisher);
      const current = Number(publisher.baseLocalTimestamp - publisher.baseRemoteTimestamp);
      if (current - offset >= TRIM_EXCESS_FRAMES * this.frameSamples) {
        let peak = 0;
        for (let index = 0; index < samples.length; index++) peak = Math.max(peak, Math.abs(samples[index]));
        if (peak < QUIET_PEAK) {
          publisher.baseLocalTimestamp -= BigInt(samples.length);
          this.drop(message, 'playback-latency-trim', reservedSamples);
          return;
        }
      }
    }
    publisher.queue.push({
      samples,
      timestamp: message.timestamp,
      localTimestamp,
      expiresAt: this.renderTimestamp + BigInt(MAX_PLAYBACK_SAMPLES),
      releasedSamples: 0,
    });
    publisher.queue.sort((left, right) => left.localTimestamp < right.localTimestamp ? -1 : left.localTimestamp > right.localTimestamp ? 1 : 0);
    publisher.bufferedSamples += samples.length;
  }

  // Records the frame delay (render clock minus sender clock, in samples) and
  // keeps the RFC 3550-style interarrival jitter for diagnostics.
  observeArrival(publisher, remoteTimestamp) {
    const arrival = Number(this.renderTimestamp);
    const remote = Number(remoteTimestamp);
    if (publisher.lastArrival !== null) {
      const deviation = Math.abs((arrival - publisher.lastArrival) - (remote - publisher.lastRemote));
      if (deviation < 48_000) publisher.jitter += (deviation - publisher.jitter) / 16;
    }
    publisher.lastArrival = arrival;
    publisher.lastRemote = remote;
    const delay = arrival - remote;
    if (publisher.delayCount > 0) {
      // A sender clock discontinuity (restart, long mute) moves every delay at
      // once: shift the window so its learned spread survives the jump.
      const previous = publisher.delays[(publisher.delayIndex + DELAY_WINDOW - 1) % DELAY_WINDOW];
      const jump = delay - previous;
      if (Math.abs(jump) > 24_000) {
        for (let index = 0; index < publisher.delayCount; index++) publisher.delays[index] += jump;
      }
    }
    publisher.delays[publisher.delayIndex] = delay;
    publisher.delayIndex = (publisher.delayIndex + 1) % DELAY_WINDOW;
    publisher.delayCount = Math.min(DELAY_WINDOW, publisher.delayCount + 1);
  }

  // Buffer target: the delay spread (95th percentile above the window minimum)
  // plus half a frame of scheduling margin, within [min, max].
  adaptiveOffset(publisher) {
    const delays = Array.from(publisher.delays.subarray(0, publisher.delayCount)).sort((a, b) => a - b);
    const floor = delays[0];
    const spread = delays[Math.min(delays.length - 1, Math.floor(DELAY_PERCENTILE * (delays.length - 1) + 0.5))] - floor;
    const target = Math.max(this.jitterMin, Math.min(this.jitterMax, Math.round(spread + this.frameSamples / 2)));
    return {offset: Math.round(floor + target), target};
  }

  adaptiveTarget(publisher) {
    if (publisher.delayCount >= MIN_DELAY_SAMPLES) return this.adaptiveOffset(publisher).target;
    return this.jitterInitial;
  }

  // Fill [from, to) of this block with the last played frame at a falling gain.
  conceal(publisher, output, blockStart, from, to) {
    if (!publisher.lastFrame || publisher.lastEnd === null || to <= from) return;
    const limit = publisher.lastEnd + BigInt(PLC_MAX_FRAMES * this.frameSamples);
    const end = to < limit ? to : limit;
    const frame = publisher.lastFrame;
    for (let position = from; position < end; position++) {
      const since = Number(position - publisher.lastEnd);
      const gain = 1 - since / (PLC_MAX_FRAMES * this.frameSamples);
      output[Number(position - blockStart)] += frame[since % frame.length] * gain * 0.8;
    }
  }

  drop(message, stage, samples) {
    this.port.postMessage({type: 'drop', stage, publisherId: message.publisherId, count: 1});
    this.port.postMessage({
      type: 'playback-released',
      publisherId: message.publisherId,
      generation: message.generation,
      samples,
    });
  }

  release(publisher, type, samples) {
    if (samples <= 0) return;
    this.port.postMessage({
      type,
      publisherId: publisher.publisherId,
      generation: publisher.generation,
      samples,
    });
  }

  removePublisher(publisherId, generation) {
    const publisher = this.publishers.get(publisherId);
    if (!publisher || publisher.generation !== generation) return;
    this.publishers.delete(publisherId);
    this.release(publisher, 'playback-released', publisher.bufferedSamples);
  }

  captureInput(input) {
    if (!input?.length) return;
    if (this.muted) {
      this.captureLength = 0;
      this.sampleTimestamp += BigInt(input.length);
      return;
    }

    for (let index = 0; index < input.length; index++) this.captureEnergy += input[index] * input[index];
    this.captureEnergySamples += input.length;
    let inputOffset = 0;
    while (inputOffset < input.length) {
      if (this.captureLength === 0) this.captureTimestamp = this.sampleTimestamp + BigInt(inputOffset);
      const count = Math.min(this.frameSamples - this.captureLength, input.length - inputOffset);
      this.capture.set(input.subarray(inputOffset, inputOffset + count), this.captureLength);
      this.captureLength += count;
      inputOffset += count;
      if (this.captureLength !== this.frameSamples) continue;

      if (this.captureCredits > 0) {
        this.captureCredits--;
        const samples = this.capture;
        this.capture = new Float32Array(this.frameSamples);
        this.port.postMessage({
          type: 'capture',
          samples: samples.buffer,
          timestamp: this.captureTimestamp,
          duration: this.frameSamples,
          generation: this.captureGeneration,
        }, [samples.buffer]);
      } else {
        this.captureDrops++;
      }
      this.captureLength = 0;
    }
    this.sampleTimestamp += BigInt(input.length);
  }

  fillOutput(output) {
    output.fill(0);
    const blockStart = this.renderTimestamp;
    const blockEnd = blockStart + BigInt(output.length);
    for (const publisher of this.publishers.values()) {
      const live = [];
      for (const entry of publisher.queue) {
        if (entry.expiresAt > blockStart) {
          live.push(entry);
          continue;
        }
        const expired = entry.samples.length - entry.releasedSamples;
        publisher.bufferedSamples -= expired;
        this.release(publisher, 'playback-released', expired);
        this.port.postMessage({type: 'drop', stage: 'playback-expired', publisherId: publisher.publisherId, count: 1});
      }
      publisher.queue = live;
      while (publisher.queue.length) {
        const entry = publisher.queue[0];
        const entryEnd = entry.localTimestamp + BigInt(entry.samples.length);
        if (entryEnd <= blockStart) {
          const expired = entry.samples.length - entry.releasedSamples;
          publisher.bufferedSamples -= expired;
          this.release(publisher, 'playback-released', expired);
          this.port.postMessage({type: 'drop', stage: 'playback-expired', publisherId: publisher.publisherId, count: 1});
          publisher.lastRenderedTimestamp = entry.timestamp;
          publisher.queue.shift();
          continue;
        }
        if (entry.localTimestamp >= blockEnd) break;

        const sourceOffset = Math.max(0, Number(blockStart - entry.localTimestamp));
        if (entry.releasedSamples < sourceOffset) {
          const skipped = sourceOffset - entry.releasedSamples;
          entry.releasedSamples = sourceOffset;
          publisher.bufferedSamples -= skipped;
          this.release(publisher, 'playback-released', skipped);
        }
        const outputOffset = Math.max(0, Number(entry.localTimestamp - blockStart));
        if (outputOffset > 0 && publisher.lastEnd !== null && publisher.lastEnd < entry.localTimestamp) {
          const from = publisher.lastEnd > blockStart ? publisher.lastEnd : blockStart;
          this.conceal(publisher, output, blockStart, from, entry.localTimestamp);
        }
        const count = Math.min(output.length - outputOffset, entry.samples.length - sourceOffset);
        for (let index = 0; index < count; index++) {
          const sample = entry.samples[sourceOffset + index];
          output[outputOffset + index] += sample;
          publisher.energy += sample * sample;
        }
        entry.releasedSamples += count;
        publisher.bufferedSamples -= count;
        this.release(publisher, 'playback-consumed', count);
        publisher.lastEnd = entry.localTimestamp + BigInt(sourceOffset + count);
        publisher.lastFrame = entry.samples;
        if (entry.releasedSamples >= entry.samples.length) {
          publisher.lastRenderedTimestamp = entry.timestamp;
          publisher.queue.shift();
          continue;
        }
        break;
      }
      // Nothing (more) to play in this block: conceal a fresh gap.
      if (publisher.lastEnd !== null && publisher.lastEnd < blockEnd) {
        const next = publisher.queue[0]?.localTimestamp;
        const to = next !== undefined && next < blockEnd ? next : blockEnd;
        const from = publisher.lastEnd > blockStart ? publisher.lastEnd : blockStart;
        if (next === undefined || next > from) this.conceal(publisher, output, blockStart, from, to);
      }
    }
    for (let index = 0; index < output.length; index++) output[index] = Math.max(-1, Math.min(1, output[index]));
    this.renderTimestamp = blockEnd;
  }

  reportLevels(samples) {
    this.levelSamples += samples;
    if (this.levelSamples < LEVEL_SAMPLES) return;
    const rms = (energy, count) => count > 0 ? Math.sqrt(energy / count) : 0;
    const publishers = [];
    for (const publisher of this.publishers.values()) {
      publishers.push([publisher.publisherId, rms(publisher.energy, LEVEL_SAMPLES)]);
      publisher.energy = 0;
    }
    this.port.postMessage({type: 'levels', capture: this.muted ? 0 : rms(this.captureEnergy, this.captureEnergySamples), publishers});
    this.levelSamples -= LEVEL_SAMPLES;
    this.captureEnergy = 0;
    this.captureEnergySamples = 0;
  }

  process(inputs, outputs) {
    this.captureInput(inputs[0]?.[0]);
    const output = outputs[0]?.[0] ?? new Float32Array();
    this.fillOutput(output);
    this.reportLevels(output.length || 128);
    return true;
  }
}

registerProcessor('tilo-voice-audio', VoiceAudioProcessor);

/**
 * A call's audio in the page: the microphone through getUserMedia (echo
 * cancellation, noise suppression and gain control), an AudioWorklet that
 * frames capture and plays each remote publisher through a jitter buffer
 * (web/voice-audio-worklet.mjs), and a worker that encodes and decodes
 * 48 kHz mono 20 ms Opus with WebCodecs (web/voice-audio-worker.mjs).
 * Muting stops the microphone track; nothing is captured while muted.
 */
const CODEC = 'opus';
const SAMPLE_RATE = 48_000;
const CHANNELS = 1;
const FRAME_SAMPLES = 960;
const BITRATE = 32_000;
const FRAME_MICROSECONDS = 20_000;
const MAX_CAPTURE_TRANSFERS = 4;
const MAX_PLAYBACK_SAMPLES = 9_600;
const MAX_OPUS_BYTES = 4_096;
const MAX_REMOTE_PUBLISHERS = 7;
const MAX_U64 = (1n << 64n) - 1n;
const PUBLISHER_ID = /^[0-9a-f]{64}$/;
// Adaptive playout: start at 40 ms, then ~3x measured jitter + 10 ms within
// [20 ms, 160 ms] -- low mouth-to-ear delay on good links, fewer late-frame
// gaps on jittery ones (see voice-audio-worklet.mjs).
const PLAYOUT_JITTER = Object.freeze({adaptive: true, initial: 1_920, min: 960, max: 7_680});

const encoderConfig = Object.freeze({
  codec: CODEC,
  sampleRate: SAMPLE_RATE,
  numberOfChannels: CHANNELS,
  bitrate: BITRATE,
  opus: {format: 'opus', frameDuration: FRAME_MICROSECONDS},
});
// Opus DTX: in silence the encoder sends one comfort-noise update per 400 ms
// instead of 50 packets a second, entering DTX only after ~200 ms without
// activity and leaving it on the first active frame, so onsets are coded
// normally. Used wherever the browser's encoder confirms it (the support
// answer echoes only the members it understood); the worker accounts for
// the frames it then never outputs (web/voice-audio-worker.mjs).
const dtxEncoderConfig = Object.freeze({
  ...encoderConfig,
  opus: {...encoderConfig.opus, usedtx: true},
});
const decoderConfig = Object.freeze({
  codec: CODEC,
  sampleRate: SAMPLE_RATE,
  numberOfChannels: CHANNELS,
});

function stopStream(stream) {
  for (const track of stream?.getTracks?.() ?? []) track.stop();
}

function abortError(signal) {
  return signal?.reason ?? new DOMException('Audio start aborted', 'AbortError');
}

function baseCapabilities() {
  return {
    supported: false,
    capture: false,
    playback: false,
    qualified: false,
    codec: CODEC,
    sampleRate: SAMPLE_RATE,
    channels: CHANNELS,
    frameSamples: FRAME_SAMPLES,
    bitrate: BITRATE,
    dtx: false,
    reason: null,
  };
}

/** Whether the encoder confirms Opus DTX; any doubt keeps every frame. */
async function supportsDtx(environment) {
  try {
    const support = await environment.AudioEncoder.isConfigSupported(dtxEncoderConfig);
    return support?.supported === true && support.config?.opus?.usedtx === true;
  } catch {
    return false;
  }
}

async function qualifyCodecs(environment) {
  let encoded;
  let decodedFrames = 0;
  let encoderError;
  let decoderError;
  const encoder = new environment.AudioEncoder({
    output(chunk) {
      encoded = new Uint8Array(chunk.byteLength);
      chunk.copyTo(encoded);
    },
    error(error) { encoderError = error; },
  });
  const decoder = new environment.AudioDecoder({
    output(frame) {
      decodedFrames++;
      if (decodedFrames !== 1
        || frame.numberOfFrames !== FRAME_SAMPLES
        || frame.sampleRate !== SAMPLE_RATE
        || frame.numberOfChannels !== CHANNELS) {
        decoderError = new Error('Decoded Opus did not match 48 kHz mono 960-sample audio');
      }
      frame.close();
    },
    error(error) { decoderError = error; },
  });
  try {
    encoder.configure(encoderConfig);
    const frame = new environment.AudioData({
      format: 'f32-planar',
      sampleRate: SAMPLE_RATE,
      numberOfFrames: FRAME_SAMPLES,
      numberOfChannels: CHANNELS,
      timestamp: 0,
      data: new Float32Array(FRAME_SAMPLES),
    });
    encoder.encode(frame);
    frame.close();
    await encoder.flush();
    if (encoderError) throw encoderError;
    if (!encoded?.byteLength) throw new Error('Opus encoder produced no bytes');

    decoder.configure(decoderConfig);
    decoder.decode(new environment.EncodedAudioChunk({
      type: 'key',
      timestamp: 0,
      duration: FRAME_MICROSECONDS,
      data: encoded,
    }));
    await decoder.flush();
    if (decoderError) throw decoderError;
    if (decodedFrames === 0) throw new Error('Opus decoder produced no audio');
  } finally {
    encoder.close();
    decoder.close();
  }
}

async function inspectCapabilities(environment) {
  const result = baseCapabilities();
  const codecTypes = ['AudioEncoder', 'AudioDecoder', 'AudioData', 'EncodedAudioChunk'];
  if (codecTypes.some(name => typeof environment[name] !== 'function')) {
    result.reason = 'WebCodecs Opus audio is unavailable';
    return result;
  }

  let encoderSupport;
  let decoderSupport;
  try {
    [encoderSupport, decoderSupport] = await Promise.all([
      environment.AudioEncoder.isConfigSupported(encoderConfig),
      environment.AudioDecoder.isConfigSupported(decoderConfig),
    ]);
  } catch (error) {
    result.reason = `WebCodecs Opus support check failed: ${error.message}`;
    return result;
  }
  if (!encoderSupport.supported || !decoderSupport.supported) {
    result.reason = 'WebCodecs Opus configuration is unsupported';
    return result;
  }

  try {
    await qualifyCodecs(environment);
    result.qualified = true;
    result.dtx = await supportsDtx(environment);
  } catch (error) {
    result.reason = `WebCodecs Opus qualification failed: ${error.message}`;
    return result;
  }

  const audioGraph = typeof environment.AudioContext === 'function'
    && typeof environment.AudioWorkletNode === 'function'
    && typeof environment.Worker === 'function';
  result.playback = audioGraph;
  result.capture = audioGraph
    && typeof environment.navigator?.mediaDevices?.getUserMedia === 'function';
  result.supported = result.capture && result.playback;
  if (!result.supported) result.reason = 'Browser audio capture or worklet support is unavailable';
  return result;
}

/** The microphones the page may choose, as `{deviceId, label}`. */
export async function audioInputs(environment = globalThis) {
  const devices = await environment.navigator?.mediaDevices?.enumerateDevices?.() ?? [];
  return devices.filter(device => device.kind === 'audioinput').map(({deviceId, label}) => ({deviceId, label}));
}

export function createBrowserAudio({environment = globalThis, onReport = () => {}} = {}) {
  let capabilityPromise;
  const capabilities = () => capabilityPromise ??= inspectCapabilities(environment);

  return {
    capabilities,
    async start({inputId, muted = true, onFrame, onLevels = () => {}, onError = () => {}, signal} = {}) {
      if (typeof onFrame !== 'function') throw new TypeError('onFrame must be a function');
      if (signal?.aborted) throw abortError(signal);
      if (['AudioEncoder', 'AudioDecoder', 'AudioData', 'EncodedAudioChunk'].some(name => typeof environment[name] !== 'function')) {
        throw new Error('WebCodecs Opus audio is unavailable');
      }
      if (typeof environment.AudioContext !== 'function') {
        throw new Error('Browser audio capture or worklet support is unavailable');
      }

      // Calling resume before the first await preserves the user activation carried by start().
      const context = new environment.AudioContext({sampleRate: SAMPLE_RATE, latencyHint: 'interactive'});
      const resumePromise = context.resume();
      resumePromise.catch(() => {});
      let currentStream;
      let currentTrackCleanup;
      let source;
      let worklet;
      let worker;
      let stopped = false;
      let captureMuted = Boolean(muted);
      let captureGeneration = 0;
      let captureInFlight = 0;
      let selectedInputId = inputId;
      let unmuteOperation;
      const publishers = new Map();
      let nextPublisherGeneration = 0;
      let selectionGeneration = 0;
      let contextClosePromise;
      let abortListener;
      let rejectAbort;
      const aborted = new Promise((_, reject) => { rejectAbort = reject; });
      let readyResolve;
      let readyReject;
      const workerReady = new Promise((resolve, reject) => {
        readyResolve = resolve;
        readyReject = reject;
      });

      const report = value => {
        try { onReport(value); } catch {}
        if (value.type === 'error' && !value.publisherId && !['capture-callback', 'capture-track-muted', 'audio-context-close'].includes(value.stage)) {
          try { onError(new Error(value.error ?? 'Audio failed')); } catch {}
        }
      };
      const releasePlayback = message => {
        const publisher = publishers.get(message.publisherId);
        if (!publisher || publisher.generation !== message.generation) return;
        publisher.reservedSamples = Math.max(0, publisher.reservedSamples - message.samples);
      };
      const removePlaybackPublisher = publisherId => {
        const publisher = publishers.get(publisherId);
        if (!publisher) return false;
        publishers.delete(publisherId);
        worker?.postMessage({type: 'remove-publisher', publisherId, generation: publisher.generation});
        worklet?.port.postMessage({type: 'remove-publisher', publisherId, generation: publisher.generation});
        return true;
      };
      const returnCaptureCredit = () => worklet?.port.postMessage({type: 'capture-credit'});
      const handleWorkerMessage = event => {
        if (stopped) return;
        const message = event.data;
        if (message.type === 'ready') return readyResolve();
        if (message.type === 'init-error') return readyReject(new Error(message.error));
        if (message.type === 'capture-consumed') {
          captureInFlight = Math.max(0, captureInFlight - 1);
          returnCaptureCredit();
          return;
        }
        if (message.type === 'playback-released') {
          releasePlayback(message);
          return;
        }
        if (message.type === 'encoded') {
          if (captureMuted || message.generation !== captureGeneration) return;
          try {
            onFrame({
              payload: new Uint8Array(message.payload),
              timestamp: message.timestamp,
              duration: message.duration,
            });
          } catch (error) {
            report({type: 'error', stage: 'capture-callback', error: error.message});
          }
          return;
        }
        if (message.type === 'decoded') {
          const publisher = publishers.get(message.publisherId);
          if (!publisher || publisher.generation !== message.generation) return;
          worklet.port.postMessage({
            type: 'play',
            publisherId: message.publisherId,
            generation: message.generation,
            samples: message.samples,
            timestamp: message.timestamp,
            duration: message.duration,
            reservedSamples: message.reservedSamples ?? message.duration,
          }, [message.samples]);
          return;
        }
        if (message.type === 'drop') {
          report(message);
          return;
        }
        if (message.type === 'error') {
          if (PUBLISHER_ID.test(message.publisherId)) {
            const publisher = publishers.get(message.publisherId);
            if (!publisher || publisher.generation !== message.generation) return;
            report(message);
            removePlaybackPublisher(message.publisherId);
            return;
          }
          report(message);
          void stop().catch(error => report({type: 'error', stage: 'audio-context-close', error: error.message}));
        }
      };
      const handleWorkletMessage = event => {
        if (stopped) return;
        const message = event.data;
        if (message.type === 'playback-consumed' || message.type === 'playback-released') {
          releasePlayback(message);
          return;
        }
        if (message.type === 'drop' || message.type === 'error') {
          report(message);
          return;
        }
        if (message.type === 'levels') {
          try { onLevels({capture: captureMuted ? 0 : message.capture, publishers: new Map(message.publishers)}); } catch {}
          return;
        }
        if (message.type !== 'capture' || stopped) return;
        if (captureMuted || message.generation !== captureGeneration) {
          returnCaptureCredit();
          return;
        }
        if (captureInFlight >= MAX_CAPTURE_TRANSFERS) {
          report({type: 'drop', stage: 'capture-transfer', count: 1});
          returnCaptureCredit();
          return;
        }
        captureInFlight++;
        worker.postMessage({
          type: 'encode',
          samples: message.samples,
          timestamp: message.timestamp,
          duration: message.duration,
          generation: message.generation,
        }, [message.samples]);
      };

      const postCaptureState = (type, value) => {
        worklet?.port.postMessage({type, ...(value === undefined ? {} : {value}), generation: captureGeneration});
      };
      const advanceCaptureGeneration = () => {
        captureGeneration++;
        postCaptureState('capture-generation');
      };
      const detachInput = () => {
        currentTrackCleanup?.();
        currentTrackCleanup = undefined;
        const previousStream = currentStream;
        const previousSource = source;
        currentStream = undefined;
        source = undefined;
        previousSource?.disconnect();
        stopStream(previousStream);
      };

      const stop = () => {
        if (stopped) return contextClosePromise ?? Promise.resolve();
        stopped = true;
        signal?.removeEventListener('abort', abortListener);
        selectionGeneration++;
        publishers.clear();
        detachInput();
        worklet?.disconnect();
        worklet?.port.close?.();
        worker?.terminate();
        try { contextClosePromise = Promise.resolve(context.close()); }
        catch (error) { contextClosePromise = Promise.reject(error); }
        return contextClosePromise;
      };

      const attachInput = async requestedInputId => {
        const generation = ++selectionGeneration;
        const audio = {
          channelCount: CHANNELS,
          sampleRate: SAMPLE_RATE,
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true,
        };
        if (requestedInputId) audio.deviceId = {exact: requestedInputId};
        const stream = await environment.navigator.mediaDevices.getUserMedia({audio, video: false});
        if (stopped || generation !== selectionGeneration || signal?.aborted) {
          stopStream(stream);
          if (signal?.aborted) throw abortError(signal);
          if (stopped) throw new Error('Audio session is stopped');
          return;
        }
        let nextSource;
        try {
          nextSource = context.createMediaStreamSource(stream);
        } catch (error) {
          nextSource?.disconnect();
          stopStream(stream);
          throw error;
        }
        const tracks = stream.getTracks?.() ?? [];
        const trackListeners = [];
        const cleanupTrackListeners = () => {
          for (const [track, type, listener] of trackListeners) track.removeEventListener?.(type, listener);
        };
        const handleTrackEnded = () => {
          if (stopped || currentStream !== stream) return;
          selectionGeneration++;
          captureMuted = true;
          captureGeneration++;
          postCaptureState('muted', true);
          detachInput();
          report({type: 'error', stage: 'capture-track-ended', error: 'Audio input track ended'});
        };
        const handleTrackMute = () => {
          if (stopped || currentStream !== stream) return;
          report({type: 'error', stage: 'capture-track-muted', error: 'Audio input track is muted'});
        };
        for (const track of tracks) {
          track.addEventListener?.('ended', handleTrackEnded);
          track.addEventListener?.('mute', handleTrackMute);
          trackListeners.push([track, 'ended', handleTrackEnded], [track, 'mute', handleTrackMute]);
        }
        const previousStream = currentStream;
        const previousSource = source;
        const previousTrackCleanup = currentTrackCleanup;
        currentStream = stream;
        source = nextSource;
        currentTrackCleanup = cleanupTrackListeners;
        advanceCaptureGeneration();
        try {
          nextSource.connect(worklet);
        } catch (error) {
          currentStream = previousStream;
          source = previousSource;
          currentTrackCleanup = previousTrackCleanup;
          cleanupTrackListeners();
          stopStream(stream);
          throw error;
        }
        const settings = tracks[0]?.getSettings?.();
        if (settings) {
          const names = ['deviceId', 'sampleRate', 'channelCount', 'echoCancellation', 'noiseSuppression', 'autoGainControl'];
          report({type: 'capture-config', stage: 'capture-input', settings: Object.fromEntries(names.filter(name => settings[name] !== undefined).map(name => [name, settings[name]]))});
        }
        previousTrackCleanup?.();
        previousSource?.disconnect();
        stopStream(previousStream);
        return true;
      };
      const beginUnmute = () => {
        if (unmuteOperation) return unmuteOperation;
        let operation;
        operation = (async () => {
          const attached = await attachInput(selectedInputId);
          if (!attached || stopped || unmuteOperation !== operation) return;
          captureMuted = false;
          postCaptureState('muted', false);
        })();
        unmuteOperation = operation;
        void operation.finally(() => {
          if (unmuteOperation === operation) unmuteOperation = undefined;
        }).catch(() => {});
        return operation;
      };

      abortListener = () => {
        rejectAbort(abortError(signal));
        void stop().catch(error => report({type: 'error', stage: 'audio-context-close', error: error.message}));
      };
      signal?.addEventListener('abort', abortListener, {once: true});

      try {
        const support = await Promise.race([capabilities(), aborted]);
        if (!support.supported) throw new Error(support.reason);
        await Promise.race([resumePromise, aborted]);
        if (signal?.aborted) throw abortError(signal);

        await Promise.race([
          context.audioWorklet.addModule(new URL('./voice-audio-worklet.mjs', import.meta.url)),
          aborted,
        ]);
        if (signal?.aborted) throw abortError(signal);
        worklet = new environment.AudioWorkletNode(context, 'tilo-voice-audio', {
          numberOfInputs: 1,
          numberOfOutputs: 1,
          outputChannelCount: [CHANNELS],
          processorOptions: {frameSamples: FRAME_SAMPLES, captureCredits: MAX_CAPTURE_TRANSFERS, muted: captureMuted, captureGeneration, jitter: PLAYOUT_JITTER},
        });
        worklet.port.addEventListener('message', handleWorkletMessage);
        worklet.port.start?.();
        worklet.connect(context.destination);
        worklet.addEventListener?.('processorerror', event => {
          if (stopped) return;
          report({type: 'error', stage: 'audio-worklet', error: event.error?.message ?? 'Audio worklet processor failed'});
          void stop().catch(error => report({type: 'error', stage: 'audio-context-close', error: error.message}));
        });

        worker = new environment.Worker(new URL('./voice-audio-worker.mjs', import.meta.url), {type: 'module', name: 'tilo-voice-audio'});
        worker.addEventListener('message', handleWorkerMessage);
        worker.addEventListener('error', event => {
          if (stopped) return;
          const error = event.error ?? new Error(event.message ?? 'Audio worker failed');
          report({type: 'error', stage: 'codec-worker', error: error.message});
          readyReject(error);
          void stop().catch(closeError => report({type: 'error', stage: 'audio-context-close', error: closeError.message}));
        });
        worker.postMessage({type: 'init', encoderConfig: support.dtx ? dtxEncoderConfig : encoderConfig, decoderConfig});
        await Promise.race([Promise.all([workerReady, captureMuted ? Promise.resolve() : attachInput(selectedInputId)]), aborted]);
        if (signal?.aborted) throw abortError(signal);

        return {
          async setMuted(value) {
            if (stopped) return;
            const nextMuted = Boolean(value);
            if (nextMuted === captureMuted && !(nextMuted && unmuteOperation)) return;
            if (nextMuted) {
              unmuteOperation = undefined;
              selectionGeneration++;
              captureMuted = true;
              captureGeneration++;
              postCaptureState('muted', true);
              detachInput();
              return;
            }
            return beginUnmute();
          },
          selectInput(requestedInputId) {
            if (stopped) return Promise.reject(new Error('Audio session is stopped'));
            selectedInputId = requestedInputId;
            if (captureMuted) {
              if (!unmuteOperation) return Promise.resolve();
              selectionGeneration++;
              unmuteOperation = undefined;
              return beginUnmute();
            }
            return attachInput(requestedInputId);
          },
          play({publisherId, timestamp, duration, payload}) {
            if (stopped) return false;
            if (!PUBLISHER_ID.test(publisherId)
              || typeof timestamp !== 'bigint'
              || timestamp < 0n
              || timestamp > MAX_U64
              || !(payload instanceof Uint8Array)
              || duration !== FRAME_SAMPLES
              || payload.byteLength === 0
              || payload.byteLength > MAX_OPUS_BYTES) {
              report({type: 'drop', stage: 'playback-invalid', count: 1});
              return false;
            }
            let publisher = publishers.get(publisherId);
            let created = false;
            if (!publisher) {
              if (publishers.size >= MAX_REMOTE_PUBLISHERS) {
                report({type: 'drop', stage: 'playback-publisher-limit', count: 1});
                return false;
              }
              publisher = {generation: ++nextPublisherGeneration, reservedSamples: 0};
              publishers.set(publisherId, publisher);
              created = true;
            }
            if (publisher.reservedSamples > MAX_PLAYBACK_SAMPLES - duration) {
              report({type: 'drop', stage: 'playback-admission', count: 1});
              return false;
            }
            publisher.reservedSamples += duration;
            const bytes = payload.slice();
            try {
              worker.postMessage({
                type: 'decode',
                publisherId,
                generation: publisher.generation,
                payload: bytes.buffer,
                timestamp,
                duration,
              }, [bytes.buffer]);
            } catch (error) {
              publisher.reservedSamples -= duration;
              if (created) publishers.delete(publisherId);
              report({type: 'error', stage: 'decoder-transfer', error: error.message});
              void stop().catch(closeError => report({type: 'error', stage: 'audio-context-close', error: closeError.message}));
              return false;
            }
            return true;
          },
          /** Play through output device `sinkId` where the browser can choose one. */
          async selectOutput(sinkId) {
            if (stopped || typeof context.setSinkId !== 'function') return false;
            await context.setSinkId(sinkId ?? '');
            return true;
          },
          get captureActive() { return !stopped && !captureMuted && Boolean(currentStream); },
          get muted() { return captureMuted; },
          get inputId() { return selectedInputId; },
          removePublisher(publisherId) {
            if (stopped) return false;
            return removePlaybackPublisher(publisherId);
          },
          stop,
        };
      } catch (error) {
        await stop();
        throw error;
      }
    },
  };
}

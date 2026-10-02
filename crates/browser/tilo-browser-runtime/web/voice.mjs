/**
 * A browser participant in a call the host governs (tilo-relay `rooms::voice`).
 *
 * The page's peer session holds the participation (tilo-browser-peer
 * `voice`): it joins, beats and rejoins after reconnects, and hands on each
 * grant the host sends. This module follows those grants with the audio:
 *
 * - a voice carrier per grant whose token or publication changed
 *   (web/voice-transport.mjs); a roster change keeps the carrier, since the
 *   host offers new publishers in place;
 * - the carrier reopened with backoff when it drops, and at once when the
 *   network changes (`online`, `navigator.connection` `change`): QUIC paths
 *   do not survive a new network;
 * - the microphone and playback (web/voice-audio.mjs): muting stops the
 *   microphone and the publication, a grant that may not publish mutes;
 * - who is speaking, from the levels the audio thread measures.
 *
 * `signalling` is the session side: `{join(call), leave(), onEvent(listener)}`
 * where events are `{kind: 'granted', grant}` and `{kind: 'ended', call,
 * reason}` (`peer.voice` in peer.mjs adapts the wasm peer).
 */
import {audioInputs, createBrowserAudio} from './voice-audio.mjs';
import {createVoiceTransport} from './voice-transport.mjs';

/** RMS above which a participant counts as speaking, and how long it stays so. */
const SPEAKING_LEVEL = 0.015;
const SPEAKING_HOLD_MS = 400;
const RECONNECT_MS = [250, 1000, 3000, 10_000];

/**
 * `state` is `{phase, call, muted, canPublish, participants, inputId,
 * reason, carrier}` where `phase` is `idle`, `joining`, `connecting`, `live`,
 * `reconnecting` or `ended`, and each participant is `{namespace, device,
 * user, publishes, speaking, self}`.
 */
export function createVoiceCall({
  signalling,
  audio = createBrowserAudio(),
  transport = createVoiceTransport(),
  environment = globalThis,
  clock = {},
  onReport = () => {},
} = {}) {
  if (!signalling) throw new TypeError('voice signalling is required');
  const now = clock.now ?? (() => performance.now());
  const setTimer = clock.setTimeout ?? ((callback, delay) => globalThis.setTimeout(callback, delay));
  const clearTimer = clock.clearTimeout ?? (handle => globalThis.clearTimeout(handle));
  const report = value => { try { onReport(value); } catch {} };
  const listeners = new Set();
  let wantMuted = false;
  let inputId;
  let state = idle();
  let grant;
  let audioSession;
  let audioStarting;
  let audioController;
  let carrier;
  let carrierKey;
  let connecting;
  let attempt = 0;
  let retryTimer;
  let capturePaused = false;
  let generation = 0;
  const speaking = new Map();

  function idle() {
    return {phase: 'idle', call: null, muted: wantMuted, canPublish: false, participants: [], inputId: inputId ?? null, reason: null, carrier: null};
  }
  const emit = () => { for (const listener of [...listeners]) { try { listener(state); } catch (error) { report({type: 'voice-listener-error', error}); } } };
  const update = change => { state = {...state, ...change}; emit(); };

  function participants() {
    if (!grant) return [];
    const own = `${grant.call}/${grant.participation}`;
    const at = now();
    return grant.participants.map(participant => ({
      namespace: participant.namespace,
      device: participant.device,
      user: participant.user,
      publishes: participant.publishes,
      self: participant.namespace === own,
      speaking: (speaking.get(participant.namespace.split('/')[1]) ?? -Infinity) > at - SPEAKING_HOLD_MS,
    }));
  }

  function onLevels({capture, publishers}) {
    if (!grant) return;
    const at = now();
    let changed = false;
    const hear = (id, level) => {
      if (level < SPEAKING_LEVEL) return;
      const was = (speaking.get(id) ?? -Infinity) > at - SPEAKING_HOLD_MS;
      speaking.set(id, at);
      changed ||= !was;
    };
    if (!state.muted) hear(grant.participation, capture);
    for (const [id, level] of publishers) hear(id, level);
    const next = participants();
    changed ||= next.some((participant, index) => participant.speaking !== state.participants[index]?.speaking);
    if (changed) update({participants: next});
  }

  function onFrame(frame) {
    if (!carrier || state.muted || !grant?.directions.publish) return;
    try { carrier.publish(frame); } catch (error) { report({type: 'voice-publish-error', error}); }
  }

  function startAudio() {
    if (audioStarting) return audioStarting;
    const controller = new AbortController();
    audioController = controller;
    const startingMuted = wantMuted;
    audioStarting = audio.start({inputId, muted: startingMuted, onFrame, onLevels, signal: controller.signal, onError: error => {
      if (audioController !== controller || controller.signal.aborted) return;
      report({type: 'voice-audio-error', error});
      const call = state.call;
      signalling.leave();
      ended(call, String(error?.message ?? error));
    }}).then(session => {
      if (audioController !== controller || controller.signal.aborted) { void session.stop(); return undefined; }
      audioSession = session;
      // A direction change or mute can arrive during microphone acquisition.
      if (capturePaused || state.muted !== startingMuted) synchronizeCapture(capturePaused || state.muted);
      return session;
    }, error => {
      if (audioController !== controller || controller.signal.aborted) return undefined;
      report({type: 'voice-audio-error', error});
      const call = state.call;
      signalling.leave();
      ended(call, String(error?.message ?? error));
      return undefined;
    });
    return audioStarting;
  }

  function synchronizeCapture(muted) {
    const session = audioSession;
    if (!session) return;
    const failed = error => {
      if (audioSession !== session) return;
      report({type: 'voice-audio-error', error});
      signalling.leave();
      ended(state.call, String(error?.message ?? error));
    };
    try { Promise.resolve(session.setMuted(muted)).catch(failed); } catch (error) { failed(error); }
  }

  function pauseCapture() {
    if (capturePaused) return;
    capturePaused = true;
    synchronizeCapture(true);
  }

  function closeCarrier(pause = true) {
    if (retryTimer !== undefined) clearTimer(retryTimer);
    retryTimer = undefined;
    connecting?.abort();
    connecting = undefined;
    const current = carrier;
    if (current && pause) pauseCapture();
    carrier = undefined;
    carrierKey = undefined;
    try { current?.close(); } catch {}
  }

  /** The grant a carrier is opened for: same token, same publication. */
  const keyOf = value => `${value.participation}:${value.token}:${value.directions.publish}:${value.directions.subscribe}`;

  function connect() {
    if (!grant || state.phase === 'idle' || state.phase === 'ended') return;
    if (carrier && carrierKey === keyOf(grant)) return;
    closeCarrier();
    const opening = grant;
    const ticket = ++generation;
    const controller = new AbortController();
    connecting = controller;
    update({phase: attempt === 0 && state.phase !== 'live' ? 'connecting' : 'reconnecting', carrier: 'connecting'});
    void transport.connect({grant: opening, signal: controller.signal}).then(session => {
      if (ticket !== generation) { session.close(); return; }
      connecting = undefined;
      carrier = session;
      carrierKey = keyOf(opening);
      attempt = 0;
      session.subscribe({
        onFrame: frame => audioSession?.play({publisherId: frame.publisher, timestamp: frame.timestamp, duration: frame.duration, payload: frame.payload}),
        onPublisher: (publisher, active) => { if (!active) audioSession?.removePublisher(publisher); },
      });
      update({phase: 'live', carrier: 'open', reason: null});
      if (capturePaused) {capturePaused = false; synchronizeCapture(state.muted);}
      void session.closed.then(() => {
        if (carrier !== session) return;
        carrier = undefined;
        carrierKey = undefined;
        retry();
      });
    }, error => {
      if (ticket !== generation) return;
      connecting = undefined;
      report({type: 'voice-carrier-error', error});
      retry();
    });
  }

  function retry() {
    if (!grant || state.phase === 'idle' || state.phase === 'ended') return;
    if (retryTimer !== undefined) return;
    pauseCapture();
    const delay = RECONNECT_MS[Math.min(attempt, RECONNECT_MS.length - 1)];
    attempt++;
    update({phase: 'reconnecting', carrier: 'waiting'});
    retryTimer = setTimer(() => { retryTimer = undefined; connect(); }, delay);
  }

  /** The network changed: the old path is gone, open a carrier on the new one now. */
  function networkChanged() {
    if (!grant || state.phase === 'idle' || state.phase === 'ended') return;
    attempt = 0;
    closeCarrier();
    connect();
  }

  function granted(next) {
    if (state.phase === 'idle' || state.phase === 'ended' || next.call !== state.call) return;
    grant = next;
    const canPublish = next.directions.publish;
    if (!canPublish && !state.muted) void audioSession?.setMuted(true);
    update({canPublish, muted: canPublish ? state.muted : true, participants: participants()});
    connect();
  }

  function ended(call, reason) {
    if (call !== state.call) return;
    stopAll();
    update({...idle(), phase: 'ended', call, reason});
  }

  function stopAll() {
    closeCarrier(false);
    capturePaused = false;
    generation++;
    grant = undefined;
    speaking.clear();
    const session = audioSession;
    audioSession = undefined;
    audioController?.abort();
    audioController = undefined;
    audioStarting = undefined;
    if (session) void session.stop();
  }

  const unsubscribe = signalling.onEvent(event => {
    if (event.kind === 'granted') granted(event.grant);
    else if (event.kind === 'ended') ended(event.call, event.reason);
  });
  const online = () => networkChanged();
  environment.addEventListener?.('online', online);
  const connection = environment.navigator?.connection;
  connection?.addEventListener?.('change', online);

  return {
    get state() { return state; },
    /** Call `listener(state)` on every change; returns its removal. */
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    /**
     * Join `call` (hex). Call it from the person's gesture: the audio
     * context resumes and the microphone is asked for within it.
     */
    join(call, {muted = false} = {}) {
      if (state.call === call && state.phase !== 'ended' && state.phase !== 'idle') return;
      if (state.phase !== 'idle' && state.phase !== 'ended') this.leave();
      wantMuted = muted;
      attempt = 0;
      state = {...idle(), phase: 'joining', call, muted};
      emit();
      void startAudio();
      signalling.join(call);
    },
    leave() {
      if (state.phase === 'idle') return;
      const wasIn = state.phase !== 'ended';
      stopAll();
      if (wasIn) signalling.leave();
      state = idle();
      emit();
    },
    async setMuted(value) {
      wantMuted = Boolean(value) || !(grant?.directions.publish ?? true);
      if (state.phase === 'idle' || state.phase === 'ended') { update({muted: wantMuted}); return; }
      update({muted: wantMuted});
      await (audioSession ?? await audioStarting)?.setMuted(wantMuted || capturePaused);
    },
    /** The microphones to choose from. Labels appear once the page may use one. */
    inputs: () => audioInputs(environment),
    async selectInput(deviceId) {
      inputId = deviceId || undefined;
      update({inputId: inputId ?? null});
      await (audioSession ?? await audioStarting)?.selectInput(inputId);
    },
    async selectOutput(deviceId) {
      return (audioSession ?? await audioStarting)?.selectOutput(deviceId) ?? false;
    },
    /** Carrier and audio counters, for diagnostics and tests. */
    stats: () => ({carrier: carrier?.stats() ?? null, phase: state.phase, captureActive: audioSession?.captureActive === true}),
    networkChanged,
    dispose() {
      this.leave();
      unsubscribe?.();
      environment.removeEventListener?.('online', online);
      connection?.removeEventListener?.('change', online);
      listeners.clear();
    },
  };
}

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
 * - the microphone and playback (web/voice-audio.mjs): a call starts muted
 *   and asks the browser for nothing; the first unmute asks for the
 *   microphone, and a refusal keeps the person in the call, muted, with the
 *   reason in `micIssue`; muting stops the microphone and the publication,
 *   and a grant that may not publish mutes;
 * - who is speaking, from the levels the audio thread measures;
 * - the camera and a shared screen (web/voice-video.mjs): published on the
 *   carrier while on, again on each new carrier, and told to the host
 *   (`signalling.show`); and the pictures of others the app shows, read off
 *   the carrier into an inbox the app collects from;
 * - whether this person is muted, told to the host with what they show
 *   whenever it changes, so everyone in the call sees it.
 *
 * `signalling` is the session side: `{join(call), leave(), show(camera,
 * screen, muted), onEvent(listener)}`
 * where events are `{kind: 'granted', grant}` and `{kind: 'ended', call,
 * reason}` (`peer.voice` in peer.mjs adapts the wasm peer).
 */
import {audioInputs, createBrowserAudio} from './voice-audio.mjs';
import {createPictureInbox, openSource} from './voice-video.mjs';
import {createVoiceTransport} from './voice-transport.mjs';
import {CarrierUnsupportedError, MediaAdmissionError} from './moq-carrier.mjs';

/** RMS above which a participant counts as speaking, and how long it stays so. */
const SPEAKING_LEVEL = 0.015;
const SPEAKING_HOLD_MS = 400;
const RECONNECT_MS = [250, 1000, 3000, 10_000];
/** How long a join waits for the host's grant before it gives up: a host
 * that never answers ends the call with a reason, never a call that stays
 * joining. */
export const GRANT_DEADLINE_MS = 15_000;
/** How often the grant deadline looks at the time the page ran. A look
 * that comes on time counts its interval; one that comes late counts it
 * less by its lateness, and one a whole interval late or more counts
 * nothing: the page stood still or crawled (a busy page beside a call:
 * software rendering while a video loads, a background tab), the session's
 * beats and grants with it, so that time is no sign that the host will not
 * answer. */
export const GRANT_LOOK_MS = 2_000;
/** Why a join ended when no grant came in time. */
export const NO_GRANT = 'no grant arrived in time';

/** Why the browser gave no microphone, as the call's status names it: the
 * person or the browser refused it, there is none, or it would not start. */
export function microphoneIssue(error) {
  const name = error?.name;
  if (name === 'NotAllowedError' || name === 'SecurityError') return 'permission_denied';
  if (name === 'NotFoundError' || name === 'OverconstrainedError') return 'source_lost';
  return 'capture_failed';
}

/**
 * `state` is `{phase, call, muted, canPublish, participants, inputId,
 * reason, carrier, micIssue, unsupported}` where `unsupported` says an
 * ended call ended because this browser cannot open the carrier, and `micIssue` is why the microphone could
 * not start (`permission_denied`, `source_lost`, `capture_failed`) or null; `phase` is `idle`, `joining`, `connecting`, `live`,
 * `reconnecting` or `ended`, `admitted` says whether the host granted the
 * call (an ended call that never was is a refusal), and each participant is `{namespace, device,
 * user, publishes, camera, screen, muted, speaking, self}`.
 */
export function createVoiceCall({
  signalling,
  audio = createBrowserAudio(),
  transport = createVoiceTransport(),
  environment = globalThis,
  clock = {},
  onReport = () => {},
  open = source => openSource(source, environment),
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
  let grantTimer;
  let capturePaused = false;
  let generation = 0;
  /** Whether the host was last told this person is muted. */
  let toldMuted;
  const speaking = new Map();
  /** The camera and screen streams this person shows, by source. */
  const streams = new Map();
  /** Their publishers on the current carrier, by source. */
  const publishers = new Map();
  /** What the app shows: `[{person, source, layer}]`. */
  let shown = [];
  /** The layer this person's own camera tile shows, if any. */
  let ownLayer = null;
  const inbox = createPictureInbox();

  function idle() {
    return {phase: 'idle', call: null, admitted: false, muted: wantMuted, canPublish: false, participants: [], inputId: inputId ?? null, reason: null, carrier: null, camera: false, screen: false, videoIssue: null, micIssue: null, unsupported: false};
  }
  const emit = () => { for (const listener of [...listeners]) { try { listener(state); } catch (error) { report({type: 'voice-listener-error', error}); } } };
  const update = change => {
    state = {...state, ...change};
    // Muting, a refused or lost microphone and a grant that may not publish
    // all reach the others.
    if (state.muted !== toldMuted && inCall()) tellShowing();
    emit();
  };
  const inCall = () => state.phase !== 'idle' && state.phase !== 'ended';

  function participants() {
    if (!grant) return [];
    const own = `${grant.call}/${grant.participation}`;
    const at = now();
    return grant.participants.map(participant => ({
      namespace: participant.namespace,
      device: participant.device,
      user: participant.user,
      publishes: participant.publishes,
      camera: participant.camera === true,
      screen: participant.screen === true,
      muted: participant.muted === true,
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
    // The microphone going away mutes the person, with the reason; the
    // call goes on and they still hear it.
    const onCaptureLost = error => {
      if (audioController !== controller || controller.signal.aborted) return;
      microphoneFailed(audioSession, error);
    };
    const onInputChanged = id => {
      if (audioController !== controller || controller.signal.aborted) return;
      inputId = id;
      update({inputId: id ?? null});
    };
    audioStarting = audio.start({inputId, muted: startingMuted, onFrame, onLevels, onCaptureLost, onInputChanged, signal: controller.signal, onError: error => {
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
    // A microphone that will not start leaves the person in the call, muted.
    const failed = error => {
      if (audioSession !== session) return;
      microphoneFailed(session, error);
    };
    try { Promise.resolve(session.setMuted(muted)).catch(failed); } catch (error) { failed(error); }
  }

  function microphoneFailed(session, error) {
    report({type: 'voice-microphone-error', error});
    wantMuted = true;
    try { Promise.resolve(session?.setMuted(true)).catch(() => {}); } catch {}
    update({muted: true, micIssue: microphoneIssue(error)});
  }

  function pauseCapture() {
    if (capturePaused) return;
    capturePaused = true;
    synchronizeCapture(true);
  }

  /** Publish every shown source on the current carrier. */
  function publishVideo() {
    if (!carrier?.publishPictures) return;
    for (const [source, stream] of streams) {
      if (publishers.has(source)) continue;
      try {
        publishers.set(source, carrier.publishPictures({
          source,
          stream,
          environment,
          ownLayer: () => (source === 'camera' ? ownLayer : null),
          own: (layer, object, keyframe) => {
            const self = participants().find(participant => participant.self);
            if (self) inbox.admit({person: self.user, stream: 'own/camera', source, object}, keyframe);
          },
          // The person ended it in the browser's own controls.
          onEnded: () => { void setVideo(source, false); },
        }));
      } catch (error) {
        report({type: 'video-publish-error', error});
      }
    }
  }

  function unpublishVideo() {
    for (const publisher of publishers.values()) publisher.stop({keepStream: true});
    publishers.clear();
  }

  /** Read the pictures the app shows of the others, by their publications. */
  function applyShown() {
    if (!carrier?.showPictures || !grant) return;
    const others = participants().filter(participant => !participant.self && participant.publishes);
    const wanted = [];
    for (const item of shown) {
      for (const participant of others) {
        if (participant.user === item.person) wanted.push({publisher: participant.namespace.split('/')[1], source: item.source, layer: item.layer});
      }
    }
    const carrierAt = generation;
    carrier.showPictures(wanted, (publisher, source, object, keyframe) => {
      const person = participants().find(participant => participant.namespace === `${grant?.call}/${publisher}`)?.user;
      if (person) inbox.admit({person, stream: `${publisher}/${carrierAt}`, source, object}, keyframe);
    });
  }

  function tellShowing() {
    toldMuted = state.muted;
    try { signalling.show?.(streams.has('camera'), streams.has('screen'), state.muted); } catch (error) { report({type: 'voice-signalling-error', error}); }
  }

  function stopVideo() {
    unpublishVideo();
    for (const stream of streams.values()) for (const track of stream.getTracks()) track.stop();
    streams.clear();
    shown = [];
    ownLayer = null;
    inbox.clear();
  }

  /** Turn `source` (`camera`, `screen`) on or off in the call. */
  async function setVideo(source, on) {
    if (!on) {
      publishers.get(source)?.stop();
      publishers.delete(source);
      const stream = streams.get(source);
      streams.delete(source);
      for (const track of stream?.getTracks() ?? []) track.stop();
      tellShowing();
      update({[source]: false});
      return;
    }
    if (streams.has(source)) return;
    const call = state.call;
    let stream;
    try {
      stream = await open(source);
    } catch (error) {
      report({type: 'video-source-error', error});
      update({videoIssue: source === 'camera' ? 'camera_denied' : 'screen_denied'});
      return;
    }
    if (state.call !== call || state.phase !== 'live' || streams.has(source)) {
      for (const track of stream.getTracks()) track.stop();
      return;
    }
    streams.set(source, stream);
    publishVideo();
    tellShowing();
    update({[source]: true, videoIssue: null});
  }

  function closeCarrier(pause = true) {
    if (retryTimer !== undefined) clearTimer(retryTimer);
    retryTimer = undefined;
    connecting?.abort();
    connecting = undefined;
    unpublishVideo();
    const current = carrier;
    if (current && pause) pauseCapture();
    carrier = undefined;
    carrierKey = undefined;
    try { current?.close(); } catch {}
  }

  /** What a carrier is opened for: its participation and directions. The
   * token only admits new carrier sessions; an admitted one keeps the
   * participation's authority, so a renewed token (every half minute) keeps
   * the carrier and is used when one is opened next. */
  const keyOf = value => `${value.participation}:${value.directions.publish}:${value.directions.subscribe}`;

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
      publishVideo();
      applyShown();
      // A lost signalling connection must not retain audio after the
      // authenticated carrier says this grant no longer has authority.
      void session.authorityLost?.then(() => {
        if (ticket !== generation || !grant || state.call !== opening.call) return;
        admissionLost(opening.call, new MediaAdmissionError().message);
      });
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
      if (error instanceof CarrierUnsupportedError) {
        unsupportedEnded(opening.call, error.message);
        return;
      }
      if (error instanceof MediaAdmissionError) {
        admissionLost(opening.call, error.message);
        return;
      }
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
    clearGrantDeadline();
    // Back after the room took the carrier away: audio again, as wanted.
    if (!audioSession && !audioStarting) void startAudio();
    const canPublish = next.directions.publish;
    if (!canPublish && !state.muted) void audioSession?.setMuted(true);
    update({admitted: true, canPublish, muted: canPublish ? state.muted : true, participants: participants()});
    connect();
    // Someone joined or left: the shown pictures follow their publications.
    applyShown();
  }

  function ended(call, reason) {
    if (call !== state.call) return;
    const admitted = state.admitted;
    stopAll();
    update({...idle(), phase: 'ended', call, admitted, reason});
  }

  function clearGrantDeadline() {
    if (grantTimer !== undefined) clearTimer(grantTimer);
    grantTimer = undefined;
  }

  /** `expire` once the page ran `GRANT_DEADLINE_MS` without a grant,
   * looked at every `GRANT_LOOK_MS`. */
  function startGrantDeadline(expire) {
    clearGrantDeadline();
    let ran = 0;
    let last = now();
    const look = () => {
      const at = now();
      const late = Math.max(at - last - GRANT_LOOK_MS, 0);
      ran += Math.max(GRANT_LOOK_MS - late, 0);
      last = at;
      if (ran >= GRANT_DEADLINE_MS) {
        grantTimer = undefined;
        expire();
        return;
      }
      grantTimer = setTimer(look, GRANT_LOOK_MS);
    };
    grantTimer = setTimer(look, GRANT_LOOK_MS);
  }

  /** The room took the carrier's admission away (revoked it, or refused
   * its token): nothing is sent or played on it any more. The session
   * still holds the participation and joins afresh when the host forgot it
   * (a page that stood still past its lifetime, a token renewed meanwhile),
   * so the call waits for that grant; one that does not come while the
   * page runs for the deadline ends the call, and an end the session
   * reports ends it at once. */
  function admissionLost(call, reason) {
    if (call !== state.call) return;
    stopAll();
    update({phase: 'reconnecting', carrier: 'waiting', camera: false, screen: false, participants: []});
    startGrantDeadline(() => {
      if (state.call === call && !grant && state.phase !== 'idle' && state.phase !== 'ended') admissionEnded(call, reason);
    });
  }

  /** This browser can never open the carrier: the call ends at once with
   * why, instead of reconnecting forever. */
  function unsupportedEnded(call, reason) {
    if (call !== state.call) return;
    stopAll();
    update({...idle(), phase: 'ended', call, admitted: false, unsupported: true, reason});
    try { signalling.leave(); } catch (error) { report({type: 'voice-signalling-error', error}); }
  }

  function admissionEnded(call, reason) {
    ended(call, reason);
    try { signalling.leave(); } catch (error) { report({type: 'voice-signalling-error', error}); }
  }

  function stopAll() {
    clearGrantDeadline();
    closeCarrier(false);
    capturePaused = false;
    generation++;
    grant = undefined;
    toldMuted = undefined;
    speaking.clear();
    stopVideo();
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
      // A browser that cannot open the carrier never joins: the others
      // would see someone in the call who hears nothing.
      const unsupported = transport.unsupported?.();
      if (unsupported) {
        state = {...idle(), phase: 'ended', call, muted, unsupported: true, reason: unsupported};
        emit();
        return;
      }
      state = {...idle(), phase: 'joining', call, muted};
      emit();
      void startAudio();
      signalling.join(call);
      // Said before the grant: the session tells the host once granted.
      tellShowing();
      startGrantDeadline(() => {
        if (state.call === call && !grant && state.phase !== 'idle' && state.phase !== 'ended') admissionEnded(call, NO_GRANT);
      });
    },
    leave() {
      if (state.phase === 'idle') return;
      const wasIn = state.phase !== 'ended';
      stopAll();
      if (wasIn) signalling.leave();
      state = idle();
      emit();
    },
    /** Mute or unmute. Unmuting asks the browser for the microphone; a
     * refusal resolves with the call still on, muted, and `micIssue` set. */
    async setMuted(value) {
      wantMuted = Boolean(value) || !(grant?.directions.publish ?? true);
      if (state.phase === 'idle' || state.phase === 'ended') { update({muted: wantMuted}); return; }
      update({muted: wantMuted, ...(wantMuted ? {} : {micIssue: null})});
      const call = state.call;
      const session = audioSession ?? await audioStarting;
      try {
        await session?.setMuted(wantMuted || capturePaused);
      } catch (error) {
        if (state.call !== call || audioSession !== session) return;
        microphoneFailed(session, error);
      }
    },
    /** Turn the camera or a shared screen on or off in the call. */
    setVideo,
    /** The pictures the app shows: `[{person, source, layer}]`. */
    showPictures(pictures) {
      shown = pictures.filter(item => !participants().some(participant => participant.self && participant.user === item.person));
      const self = participants().find(participant => participant.self);
      ownLayer = pictures.find(item => item.source === 'camera' && item.person === self?.user)?.layer ?? null;
      applyShown();
    },
    /** The pictures received after `after`: `{pictures, cursor}`, objects in base64. */
    takePictures: (after, budget) => inbox.take(after, budget),
    /** Resolves when a picture arrives or `ms` passed. */
    waitPictures: ms => inbox.wait(ms),
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
    /** Why this browser cannot take part in calls, or null when it can. */
    unsupported: () => transport.unsupported?.() ?? null,
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

/**
 * A browser participant in a call: its voice and its pictures on the
 * workspace's live plane (`tilo-session-client` `session/plane.rs`).
 *
 * The page is a member of the call's channel and holds its content keys, so
 * it seals what its microphone and camera make itself and opens what the
 * call's devices send. Its voice is published frame by frame on the
 * workspace's plane (`signalling.audio`), and a picture the moment an
 * encoder makes one (`signalling.picture`): every device in the call hears
 * and sees them as the plane delivers them. This module is the app around
 * that:
 *
 * - the microphone and playback (web/voice-audio.mjs): a call starts muted
 *   and asks the browser for nothing; the first unmute asks for the
 *   microphone, and a refusal keeps the person in the call, muted, with the
 *   reason in `micIssue`; muting stops the microphone; what the call
 *   publishes is played as it arrives (`heard` events);
 * - the camera and a shared screen (web/voice-video.mjs), encoded in the
 *   layers the call's devices show: the plane says which layers those are
 *   (`demand` events), what the page shows of each device is told to the
 *   plane (`signalling.showing`), and a device that starts showing a source
 *   is asked for a keyframe (`signalling.keyframe`) until one arrives;
 * - the pictures the plane delivers for the call's devices, admitted to the
 *   inbox as they come (`picture` events), for the app to draw;
 * - who is speaking, from the levels the audio thread measures;
 * - whether this person is muted or deafened, told to the host with what they
 *   show whenever it changes; a deafened person hears nothing of the call and
 *   is muted.
 *
 * `signalling` is the session side: `{join(call), leave(), audio(json),
 * picture(json), showing(json), keyframe(device, source), show(camera, screen,
 * muted, deafened), onEvent(listener)}` where events are `{kind: 'granted',
 * grant}`, `{kind: 'ended', call, reason}`, `{kind: 'heard', from, sequence,
 * timestamp48k, opus}` (`opus` in hex), `{kind: 'picture', from, source,
 * keyframe, object}` (`object` the tilo-video/1 object in hex) and `{kind:
 * 'demand', wanted: [{source, layers}], keyframes: [source]}`, and `device` is
 * this page's own device: the sender its frames carry (peer.voice in
 * peer.mjs adapts the wasm peer).
 */
import {audioInputs, createBrowserAudio} from './voice-audio.mjs';
import {createPictureEncoder, createPictureInbox, openSource, pictureObject} from './voice-video.mjs';
import {callIdOf} from './voice-controls.mjs';

/** RMS above which a participant counts as speaking, and how long it stays so. */
const SPEAKING_LEVEL = 0.015;
const SPEAKING_HOLD_MS = 400;
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
 * `state` is `{phase, call, app, record, muted, canPublish, participants,
 * inputId, reason, micIssue, unsupported}` where `micIssue` is why the
 * microphone could not start (`permission_denied`, `source_lost`,
 * `capture_failed`) or null, and `phase` is `idle`, `joining`,
 * `connecting`, `live`, `reconnecting` or `ended`; `admitted` says whether the
 * host granted the call (an ended call that never was is a refusal), and each
 * participant is `{device, user, publishes, camera, screen, muted, deafened,
 * speaking, self}`.
 */
export function createVoiceCall({
  signalling,
  device = null,
  audio = createBrowserAudio(),
  environment = globalThis,
  clock = {},
  onReport = () => {},
  open = source => openSource(source, environment),
  encoder = createPictureEncoder,
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
  /** The call's app and record: the channel they and the workspace name
   * seals its frames. */
  let app = '';
  let record = '';
  let grantTimer;
  let capturePaused = false;
  let generation = 0;
  /** Whether the host was last told this person is muted, and deafened. */
  let toldMuted;
  let toldDeafened;
  /** Whether the microphone was muted when the person deafened: what
   * undeafening puts back. */
  let mutedBeforeDeafen = true;
  const speaking = new Map();
  /** How loud each participant plays here, by id (`callIdOf`). */
  const volumes = new Map();
  /** The newest frame played from each device, by hex: a plane that is joined
   * again announces its stream's first object again, which plays once. */
  const heardUpTo = new Map();
  /** The layers each device in the call shows of this page's sources, by
   * source, as its last answer said. */
  let shownOfPage = {camera: new Set(), screen: new Set()};
  /** Devices asking this page's pictures to start over, by source. */
  let keyframesAsked = {camera: false, screen: false};
  /** The camera and screen streams this person shows. */
  const streams = new Map();
  /** Their encoders, by source. */
  const encoders = new Map();
  /** What the app shows: `[{person, source, layer}]`. */
  let shown = [];
  /** The layer this person's own camera tile shows, if any. */
  let ownLayer = null;
  /** Devices this page asks to start over, by `${device}|${source}` until
   * the keyframe arrives, and those whose keyframe already did. */
  const asking = new Set();
  const ready = new Set();
  const inbox = createPictureInbox();
  /** What left and arrived over the plane, for diagnostics and tests. */
  const counters = {frames: 0, heard: 0, sent: 0, pictures: 0, showing: 0};

  function idle() {
    return {phase: 'idle', call: null, app: '', record: '', admitted: false, muted: wantMuted, canPublish: false, participants: [], inputId: inputId ?? null, reason: null, camera: false, screen: false, deafened: false, videoIssue: null, micIssue: null, unsupported: false, audioFailed: false};
  }
  const emit = () => { for (const listener of [...listeners]) { try { listener(state); } catch (error) { report({type: 'voice-listener-error', error}); } } };
  const update = change => {
    state = {...state, ...change};
    // Muting, deafening, a refused or lost microphone and a call that may
    // not publish all reach the others.
    if ((state.muted !== toldMuted || state.deafened !== toldDeafened) && inCall()) tellShowing();
    emit();
  };
  const inCall = () => state.phase !== 'idle' && state.phase !== 'ended';

  function participants() {
    if (!grant) return [];
    const at = now();
    return grant.participants.map(participant => ({
      device: participant.device,
      user: participant.user,
      publishes: participant.publishes,
      camera: participant.camera === true,
      screen: participant.screen === true,
      muted: participant.muted === true,
      deafened: participant.deafened === true,
      self: participant.device === device,
      speaking: (speaking.get(participant.device) ?? -Infinity) > at - SPEAKING_HOLD_MS,
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
    if (!state.muted) {
      // This page's frames go out as the host's device: its participation
      // in the call is this person speaking.
      for (const participant of grant.participants) {
        if (participant.device && participant.device === device) hear(participant.device, capture);
      }
    }
    for (const [id, level] of publishers) hear(id, level);
    const next = participants();
    changed ||= next.some((participant, index) => participant.speaking !== state.participants[index]?.speaking);
    if (changed) update({participants: next});
  }

  /** How loud `sender` plays here: its person's volume, or the call's. */
  function gainOf(sender) {
    const person = grant?.participants.find(participant => participant.device === sender)?.user;
    return (person && volumes.get(person)) ?? 1;
  }

  /** One frame of the microphone, published as it is made: a call's voice is
   * current, never queued behind a turn. */
  function onFrame(frame) {
    if (!inCall() || state.muted) return;
    try {
      signalling.audio?.([{timestamp48k: Number(frame.timestamp), opus: hexOfBytes(frame.payload)}]);
      counters.frames += 1;
    } catch (error) {
      report({type: 'voice-signalling-error', error});
    }
  }

  /** One frame of the call's voice, as a device published it: play it unless
   * it was already played. */
  function heard(frame) {
    if (!inCall() || state.deafened) return;
    counters.heard += 1;
    const played = heardUpTo.get(frame.from);
    // A plane joined again announces the first object of its stream again,
    // and a stream that started over numbers its frames from the start.
    if (played !== undefined && frame.sequence <= played && frame.sequence !== 0) return;
    heardUpTo.set(frame.from, frame.sequence);
    try {
      audioSession?.play({publisherId: frame.from, timestamp: BigInt(frame.timestamp48k), duration: 960, payload: bytesOfHex(frame.opus), gain: gainOf(frame.from)});
    } catch (error) {
      report({type: 'voice-play-error', error});
    }
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
      leaveCall();
      ended(call, String(error?.message ?? error), {audioFailed: true});
    }}).then(session => {
      if (audioController !== controller || controller.signal.aborted) { void session.stop(); return undefined; }
      audioSession = session;
      session.setDeafened?.(state.deafened);
      // A direction change or mute can arrive during microphone acquisition.
      if (capturePaused || state.muted !== startingMuted) synchronizeCapture(capturePaused || state.muted);
      return session;
    }, error => {
      if (audioController !== controller || controller.signal.aborted) return undefined;
      report({type: 'voice-audio-error', error});
      const call = state.call;
      leaveCall();
      ended(call, String(error?.message ?? error), {audioFailed: true});
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

  /** The layers of this page's `source` the call's devices show. */
  function encoderWanted(source) {
    return layer => layer !== null && shownOfPage[source].has(layer);
  }

  /** Encode every shown source for the layers the call shows of it. */
  function encodeVideo() {
    for (const [source, stream] of streams) {
      if (encoders.has(source)) continue;
      try {
        encoders.set(source, encoder({
          source,
          stream,
          environment,
          wanted: encoderWanted(source),
          onPicture: picture => ownPicture(picture),
          // The person ended it in the browser's own controls.
          onEnded: () => { void setVideo(source, false); },
          report,
        }));
      } catch (error) {
        report({type: 'video-encode-error', error});
      }
    }
  }

  /** One picture this page's encoder made: published on the plane as it is
   * made, never queued for a later turn. */
  function ownPicture(picture) {
    try {
      signalling.picture?.([{
        source: picture.source,
        layer: picture.layer,
        sequence: picture.sequence,
        keyframe: picture.keyframe,
        timestampUs: picture.timestampUs,
        width: picture.width,
        height: picture.height,
        payload: hexOfBytes(picture.payload),
      }]);
      counters.sent += 1;
    } catch (error) {
      report({type: 'voice-signalling-error', error});
    }
    // This person's own tile draws what their camera makes at its layer.
    if (picture.source === 'camera' && ownLayer === picture.layer) {
      const self = participants().find(participant => participant.self);
      if (self) inbox.admit({person: self.user, stream: 'own/camera', source: picture.source, object: pictureObject(picture)}, picture.keyframe);
    }
  }

  function stopEncoders() {
    for (const encoder of encoders.values()) encoder.stop({keepStream: true});
    encoders.clear();
  }

  /** What this page shows of each device, as the plane is told it: the
   * devices' layers the page draws (`signalling.showing`). */
  function showsOfPage() {
    const byDevice = new Map();
    const add = (source, person, layer) => {
      for (const participant of grant?.participants ?? []) {
        if (participant.self || participant.user !== person || !participant.device) continue;
        const entry = byDevice.get(participant.device) ?? {device: participant.device, camera: [], screen: []};
        if (!entry[source].includes(layer)) entry[source].push(layer);
        byDevice.set(participant.device, entry);
      }
    };
    for (const item of shown) add(item.source, item.person, item.layer);
    return [...byDevice.values()];
  }

  /** Tell the plane what this page shows of each device, so it reads exactly
   * those layers and their owners encode exactly those. */
  function sendShowing() {
    counters.showing += 1;
    try { signalling.showing?.(showsOfPage()); } catch (error) { report({type: 'voice-signalling-error', error}); }
  }

  function tellShowing() {
    toldMuted = state.muted;
    toldDeafened = state.deafened;
    try { signalling.show?.(streams.has('camera'), streams.has('screen'), state.muted, state.deafened); } catch (error) { report({type: 'voice-signalling-error', error}); }
  }

  function stopVideo() {
    stopEncoders();
    for (const stream of streams.values()) for (const track of stream.getTracks()) track.stop();
    streams.clear();
    shown = [];
    ownLayer = null;
    shownOfPage = {camera: new Set(), screen: new Set()};
    asking.clear();
    ready.clear();
    inbox.clear();
  }

  /** Turn `source` (`camera`, `screen`) on or off in the call. */
  async function setVideo(source, on) {
    if (!on) {
      encoders.get(source)?.stop();
      encoders.delete(source);
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
    encodeVideo();
    tellShowing();
    update({[source]: true, videoIssue: null});
  }

  /** One picture the plane delivered for a device: admitted to the inbox as
   * it comes, for the app to draw. */
  function heardPicture(picture) {
    const person = grant?.participants.find(participant => participant.device === picture.from)?.user;
    const source = picture.source;
    if (!person || (source !== 'camera' && source !== 'screen')) return;
    counters.pictures += 1;
    if (picture.keyframe) {
      asking.delete(`${picture.from}|${source}`);
      ready.add(`${picture.from}|${source}`);
    }
    inbox.admit({person, stream: `${picture.from}/${generation}`, source, object: bytesOfHex(picture.object ?? '')}, picture.keyframe);
  }

  /** What the plane says the call shows of this page: the layers its encoders
   * make, and the sources a device needs started over. */
  function demanded(demand) {
    const shownNow = {camera: new Set(), screen: new Set()};
    for (const entry of demand.wanted ?? []) {
      if (!Object.hasOwn(shownNow, entry.source)) continue;
      for (const layer of entry.layers ?? []) shownNow[entry.source].add(layer);
    }
    shownOfPage = shownNow;
    for (const source of demand.keyframes ?? []) encoders.get(source)?.keyframe();
  }

  function granted(next) {
    if (state.phase === 'idle' || state.phase === 'ended' || next.call !== state.call) return;
    grant = next;
    clearGrantDeadline();
    // Back after the host took the call away: audio again, as wanted.
    if (!audioSession && !audioStarting) void startAudio();
    const canPublish = next.directions.publish;
    if (!canPublish && !state.muted) void audioSession?.setMuted(true);
    update({phase: 'live', admitted: true, canPublish, muted: canPublish ? state.muted : true, participants: participants()});
    encodeVideo();
    sendShowing();
    if (capturePaused) { capturePaused = false; synchronizeCapture(state.muted); }
  }

  /** The call ended for `reason`; `audioFailed` when this device's audio
   * would not start, which no host can mend. */
  function ended(call, reason, {audioFailed = false} = {}) {
    if (call !== state.call) return;
    const admitted = state.admitted;
    stopAll();
    update({...idle(), phase: 'ended', call, admitted, reason, audioFailed});
  }

  function clearGrantDeadline() {
    if (grantTimer !== undefined) clearTimer(grantTimer);
    grantTimer = undefined;
  }

  /** `expire` once the page ran, online, `GRANT_DEADLINE_MS` without a
   * grant, looked at every `GRANT_LOOK_MS`. */
  function startGrantDeadline(expire) {
    clearGrantDeadline();
    let ran = 0;
    let last = now();
    const look = () => {
      const at = now();
      const late = Math.max(at - last - GRANT_LOOK_MS, 0);
      // Offline, the session cannot reach the host: no grant can come, so
      // that time is no sign the host will not answer either. The call
      // waits for the network.
      if (environment.navigator?.onLine !== false) ran += Math.max(GRANT_LOOK_MS - late, 0);
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

  function admissionEnded(call, reason) {
    ended(call, reason);
    leaveCall();
  }

  /** Silence or play what the call plays here, and say it. */
  function applyDeafened(deafened) {
    try { audioSession?.setDeafened?.(deafened); } catch (error) { report({type: 'voice-audio-error', error}); }
    update({deafened});
  }

  /** Leave the call on the host's side: this page's participation ends. */
  function leaveCall() {
    try { signalling.leave?.(); } catch (error) { report({type: 'voice-signalling-error', error}); }
  }

  function stopAll() {
    clearGrantDeadline();
    capturePaused = false;
    generation++;
    grant = undefined;
    app = '';
    record = '';
    toldMuted = undefined;
    toldDeafened = undefined;
    speaking.clear();
    volumes.clear();
    heardUpTo.clear();
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
    else if (event.kind === 'heard') heard(event);
    else if (event.kind === 'ended') ended(event.call, event.reason);
    else if (event.kind === 'picture') heardPicture(event);
    else if (event.kind === 'demand') demanded(event);
  });
  const online = () => networkChanged();
  environment.addEventListener?.('online', online);
  const connection = environment.navigator?.connection;
  connection?.addEventListener?.('change', online);

  /** The network changed: the session joins the plane again, and the
   * microphone stays quiet until the call is granted again. */
  function networkChanged() {
    if (!inCall()) return;
    pauseCapture();
  }

  return {
    get state() { return state; },
    /** Call `listener(state)` on every change; returns its removal. */
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    /**
     * Join `call` (hex) of `app`'s record `record`. Call it from the person's
     * gesture: the audio context resumes and the microphone is asked for
     * within it.
     */
    join(call, {muted = false, app: callApp = '', record: callRecord = ''} = {}) {
      if (state.call === call && state.phase !== 'ended' && state.phase !== 'idle') return;
      if (state.phase !== 'idle' && state.phase !== 'ended') this.leave();
      wantMuted = muted;
      app = callApp;
      record = callRecord;
      state = {...idle(), phase: 'joining', call, app, record, muted};
      emit();
      void startAudio();
      signalling.join(call);
      // Said before the grant: the session tells the host once granted.
      tellShowing();
      startGrantDeadline(() => {
        if (state.call === call && !grant && inCall()) admissionEnded(call, NO_GRANT);
      });
    },
    leave() {
      if (state.phase === 'idle') return;
      const wasIn = state.phase !== 'ended';
      stopAll();
      if (wasIn) leaveCall();
      state = idle();
      emit();
    },
    /** Mute or unmute. Unmuting asks the browser for the microphone; a
     * refusal resolves with the call still on, muted, and `micIssue` set. */
    async setMuted(value) {
      // Unmuting is hearing the call again too.
      if (!value && state.deafened) applyDeafened(false);
      wantMuted = Boolean(value) || !(grant?.directions.publish ?? true);
      if (!inCall()) { update({muted: wantMuted}); return; }
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
    /** Deafen or undeafen: a deafened person hears nothing of the call
     * and is muted. Undeafening unmutes again when they were unmuted
     * before and `unmute` allows it (the browser asks for the microphone
     * only from a gesture). */
    async setDeafened(value, {unmute = true} = {}) {
      const deafen = Boolean(value);
      if (!inCall() || deafen === state.deafened) return;
      if (deafen) {
        mutedBeforeDeafen = state.muted;
        applyDeafened(true);
        await this.setMuted(true);
        return;
      }
      applyDeafened(false);
      if (!mutedBeforeDeafen && unmute) await this.setMuted(false);
    },
    /** Turn the camera or a shared screen on or off in the call. */
    setVideo,
    /** Play participant `id` (a person or `<service>:<account>`) at `gain`
     * (0 silences, 1 as they arrive, at most 2) on this device only. */
    setVolume(id, gain) {
      if (typeof id !== 'string' || !Number.isFinite(gain) || gain < 0 || gain > 2) return;
      if (gain === 1) volumes.delete(id); else volumes.set(id, gain);
    },
    /** The pictures the app shows: `[{person, source, layer}]`. */
    showPictures(items) {
      const self = participants().find(participant => participant.self);
      shown = items.filter(item => !(self && item.person === self.user));
      ownLayer = items.find(item => item.source === 'camera' && item.person === self?.user)?.layer ?? null;
      // A device that starts showing a source is asked for a keyframe until
      // one arrives: nothing before it decodes.
      const keys = new Set();
      for (const item of shown) {
        for (const participant of grant?.participants ?? []) {
          if (participant.self || participant.user !== item.person || !participant.device) continue;
          const key = `${participant.device}|${item.source}`;
          keys.add(key);
          if (asking.has(key) || ready.has(key)) continue;
          asking.add(key);
          try { signalling.keyframe?.(participant.device, item.source); } catch (error) { report({type: 'voice-signalling-error', error}); }
        }
      }
      for (const key of [...asking, ...ready]) if (!keys.has(key)) { asking.delete(key); ready.delete(key); }
      if (inCall()) sendShowing();
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
    unsupported: () => null,
    /** What left and arrived over the plane, for diagnostics and tests. */
    stats: () => ({phase: state.phase, captureActive: audioSession?.captureActive === true, counters: {...counters}}),
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

/** `bytes` in hex, as the plane's voice events carry an Opus frame. */
export function hexOfBytes(bytes) {
  let text = '';
  for (const byte of bytes) text += byte.toString(16).padStart(2, '0');
  return text;
}

/** Bytes from hex, as the plane's voice events carry an Opus frame. */
export function bytesOfHex(text) {
  const bytes = new Uint8Array(text.length >> 1);
  for (let index = 0; index < bytes.length; index++) bytes[index] = parseInt(text.slice(index * 2, index * 2 + 2), 16);
  return bytes;
}

/**
 * A browser participant in a call its host carries on the call lane
 * (`tilo-call-lane` `page`, `tilo-browser-peer` `call_media`).
 *
 * The page is a member of the call's channel and holds its content keys, so
 * it seals what its microphone and camera make itself and opens what the
 * call's devices send: the host only copies sealed bytes between the page and
 * the lane (`POST {API_PREFIX}/call/lane`, `signalling.turn`). This module is
 * the app around that exchange:
 *
 * - the microphone and playback (web/voice-audio.mjs): a call starts muted
 *   and asks the browser for nothing; the first unmute asks for the
 *   microphone, and a refusal keeps the person in the call, muted, with the
 *   reason in `micIssue`; muting stops the microphone;
 * - one turn every `TURN_MS`, one at a time: the microphone frames since the
 *   last turn, the pictures the encoders made, what the page shows of each
 *   device, and the keyframes it asks for; the answer is played, admitted to
 *   the picture inbox, and says which layers of the page's pictures each
 *   device shows and which it wants started over;
 * - the camera and a shared screen (web/voice-video.mjs), encoded in the
 *   layers the call's devices show, and told to the host
 *   (`signalling.show`), so everyone in the call sees who shows what;
 * - who is speaking, from the levels the audio thread measures;
 * - a host that cannot be reached is retried with backoff and the microphone
 *   waits meanwhile; a turn the host refuses (the call ended, this page lost
 *   its admission) waits for the host's next grant, and a call whose grant
 *   does not come within the deadline ends with a reason;
 * - whether this person is muted or deafened, told to the host with what they
 *   show whenever it changes; a deafened person hears nothing of the call and
 *   is muted.
 *
 * `signalling` is the session side: `{join(call), leave(), end(), turn(json),
 * show(camera, screen, muted, deafened), reachable(), onEvent(listener)}`
 * where events are `{kind: 'granted', grant}` and `{kind: 'ended', call,
 * reason}`, and `device` is the device the host bridges this page as: the
 * sender its frames carry (peer.voice in peer.mjs adapts the wasm peer).
 */
import {audioInputs, createBrowserAudio} from './voice-audio.mjs';
import {LAYER_CODES, SOURCE_CODES, createPictureEncoder, createPictureInbox, layerOfCode, openSource, pictureObject, sourceOfCode} from './voice-video.mjs';
import {callIdOf} from './voice-controls.mjs';

/** RMS above which a participant counts as speaking, and how long it stays so. */
const SPEAKING_LEVEL = 0.015;
const SPEAKING_HOLD_MS = 400;
const RECONNECT_MS = [250, 1000, 3000, 10_000];
/** How often the page takes a turn with its host, and how long one may take
 * before the page gives up on it and asks again. */
export const TURN_MS = 20;
export const TURN_DEADLINE_MS = 5_000;
/** Microphone frames kept for the next turn: a page that stood still sends
 * the newest `MAX_TURN_AUDIO` and drops the rest, as the call plays only what
 * is current. */
const MAX_TURN_AUDIO = 20;
/** How far behind its newest frame a datagram reaches back with repeats
 * ([`tilo_call_lane::voice::MAX_BUNDLE_OBJECTS`]): a frame within that reach
 * that was already played is a repeat, not a stream that started over. */
const REPEAT_REACH = 3;
/** Pictures kept for the next turn: encoded layers of a loaded page. */
const MAX_TURN_PICTURES = 256;
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
/** Why a turn cannot be taken at all: this page holds none of the call's
 * keys, or only watches the workspace. */
const NO_KEYS = /no keys|only watches/i;
/** Why a turn was refused: the host does not admit this page to the call's
 * workspace any more, or the call is over. */
const REFUSED = /not admitted|refused|not a member|no call lane/i;

/** Why the browser gave no microphone, as the call's status names it: the
 * person or the browser refused it, there is none, or it would not start. */
export function microphoneIssue(error) {
  const name = error?.name;
  if (name === 'NotAllowedError' || name === 'SecurityError') return 'permission_denied';
  if (name === 'NotFoundError' || name === 'OverconstrainedError') return 'source_lost';
  return 'capture_failed';
}

/** `bytes` in base64, as the turn's JSON carries an Opus frame. */
export function base64OfBytes(bytes) {
  let text = '';
  for (let index = 0; index < bytes.length; index += 0x8000) text += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
  return btoa(text);
}

/**
 * `state` is `{phase, call, app, record, muted, canPublish, participants,
 * inputId, reason, turn, micIssue, unsupported}` where `turn` is `idle`,
 * `waiting` or `open` (whether the host answered the last turn), `micIssue`
 * is why the microphone could not start (`permission_denied`,
 * `source_lost`, `capture_failed`) or null, and `phase` is `idle`, `joining`,
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
  let attempt = 0;
  let retryTimer;
  let turnTimer;
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
  /** The microphone frames and encoded pictures the next turn carries. */
  let microphone = [];
  let pictures = [];
  /** The newest frame played from each device, by hex: a call's datagrams
   * repeat the frames before their newest one, which play once. */
  const heardUpTo = new Map();
  /** Whether a turn is with the host, and when the last one was answered. */
  let turning = false;
  let turnTimerHandle;
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
   * the keyframe arrives. */
  const asking = new Map();
  const inbox = createPictureInbox();
  const turns = {sent: 0, refused: 0, unreachable: 0, audio: 0, pictures: 0};

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

  function onFrame(frame) {
    if (!inCall() || state.muted) return;
    microphone.push({timestamp48k: Number(frame.timestamp), opus: base64OfBytes(frame.payload)});
    if (microphone.length > MAX_TURN_AUDIO) microphone = microphone.slice(-MAX_TURN_AUDIO);
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
        encoders.set(source, createPictureEncoder({
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

  function ownPicture(picture) {
    if (pictures.length < MAX_TURN_PICTURES) pictures.push(picture);
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

  /** What this page shows of each device, as the turn sends it. */
  function showsOfPage() {
    const byDevice = new Map();
    const add = (source, person, layer) => {
      const code = LAYER_CODES[layer];
      if (code === undefined) return;
      for (const participant of grant?.participants ?? []) {
        if (participant.self || participant.user !== person || !participant.device) continue;
        const entry = byDevice.get(participant.device) ?? {device: participant.device, camera: [], screen: []};
        if (!entry[source].includes(code)) entry[source].push(code);
        byDevice.set(participant.device, entry);
      }
    };
    for (const item of shown) add(item.source, item.person, item.layer);
    return [...byDevice.values()];
  }

  /** What the page asks the call's devices to start over on, as the turn
   * sends it. */
  function keyframesOfPage() {
    return [...asking].map(([key]) => {
      const [device, source] = key.split('|');
      return {device, source: SOURCE_CODES[source]};
    }).filter(entry => entry.source !== undefined);
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

  /** A turn with the host: what the page sealed since the last one goes out,
   * and what the call sent comes back. */
  function pump() {
    turnTimerHandle = undefined;
    if (!inCall() || turning || state.phase === 'joining') return;
    const frames = microphone;
    const encoded = pictures;
    microphone = [];
    pictures = [];
    const turn = {
      call: state.call,
      app,
      record,
      audio: frames,
      pictures: encoded.map(picture => ({
        source: SOURCE_CODES[picture.source],
        layer: LAYER_CODES[picture.layer],
        sequence: picture.sequence,
        keyframe: picture.keyframe,
        timestampUs: picture.timestampUs,
        width: picture.width,
        height: picture.height,
        payload: base64OfBytes(picture.payload),
      })),
      shows: showsOfPage(),
      keyframes: keyframesOfPage(),
    };
    turning = true;
    let answered;
    try {
      answered = Promise.resolve(signalling.turn(JSON.stringify(turn)));
    } catch (error) {
      answered = Promise.reject(error);
    }
    const deadline = setTimer(() => {
      if (turning) unreachable(new Error('the host did not answer the turn in time'));
    }, TURN_DEADLINE_MS);
    void answered.then(json => {
      clearTimer(deadline);
      if (!turning) return;
      turning = false;
      attempt = 0;
      turns.sent += 1;
      const answer = JSON.parse(json);
      apply(answer);
      if (state.phase !== 'live') update({phase: 'live', reason: null});
      if (capturePaused) { capturePaused = false; synchronizeCapture(state.muted); }
      schedule();
    }, error => {
      clearTimer(deadline);
      if (!turning) return;
      turning = false;
      const text = String(error?.message ?? error);
      if (NO_KEYS.test(text)) {
        ended(state.call, text);
        leaveCall();
      } else if (REFUSED.test(text)) {
        refused(state.call, text);
      } else {
        unreachable(error);
      }
    });
  }

  /** What the answer brings: play what the call said, queue the pictures,
   * and follow what its devices show and ask of the page. */
  function apply(answer) {
    for (const frame of answer.audio ?? []) {
      turns.audio += 1;
      const played = heardUpTo.get(frame.from);
      if (played !== undefined && frame.sequence <= played) {
        // A datagram carries the frames just before its newest one, and a
        // device whose stream started over numbers them from the start
        // again: past the repeats' reach, this is that.
        if (played - frame.sequence < REPEAT_REACH) continue;
      }
      heardUpTo.set(frame.from, frame.sequence);
      try {
        audioSession?.play({publisherId: frame.from, timestamp: BigInt(frame.timestamp48k), duration: 960, payload: base64OfDecoded(frame.opus), gain: gainOf(frame.from)});
      } catch (error) {
        report({type: 'voice-play-error', error});
      }
    }
    for (const picture of answer.pictures ?? []) {
      turns.pictures += 1;
      const person = grant?.participants.find(participant => participant.device === picture.from)?.user;
      const source = sourceOfCode(picture.source);
      if (!person || !source) continue;
      if (picture.keyframe) asking.delete(`${picture.from}|${source}`);
      inbox.admit({person, stream: `${picture.from}/${generation}`, source, object: base64OfDecoded(picture.object)}, picture.keyframe);
    }
    const shownNow = {camera: new Set(), screen: new Set()};
    for (const entry of answer.shows ?? []) {
      for (const code of entry.camera ?? []) shownNow.camera.add(layerOfCode(code));
      for (const code of entry.screen ?? []) shownNow.screen.add(layerOfCode(code));
    }
    shownNow.camera.delete(null);
    shownNow.screen.delete(null);
    shownOfPage = shownNow;
    for (const asked of answer.keyframes ?? []) {
      const source = sourceOfCode(asked.source);
      if (source) encoders.get(source)?.keyframe();
    }
  }

  function schedule() {
    if (turnTimerHandle !== undefined || !inCall() || state.phase === 'joining') return;
    turnTimerHandle = setTimer(pump, TURN_MS);
  }

  function stopTurns() {
    if (turnTimerHandle !== undefined) clearTimer(turnTimerHandle);
    turnTimerHandle = undefined;
    if (retryTimer !== undefined) clearTimer(retryTimer);
    retryTimer = undefined;
    turning = false;
    microphone = [];
    pictures = [];
  }

  /** The host could not be reached: retry, and keep the microphone quiet
   * meanwhile, so nothing piles up behind a link that is not there. */
  function unreachable(error) {
    turns.unreachable += 1;
    report({type: 'voice-turn-error', error: error instanceof Error ? error : new Error(String(error?.message ?? error))});
    if (!inCall() || state.phase === 'ended') return;
    pauseCapture();
    const delay = RECONNECT_MS[Math.min(attempt, RECONNECT_MS.length - 1)];
    attempt++;
    update({phase: 'reconnecting'});
    if (retryTimer === undefined) {
      retryTimer = setTimer(() => { retryTimer = undefined; schedule(); }, delay);
    }
  }

  /** The host refused the turn: this page is in no call it can seal, or the
   * call is over. It waits for the host's next grant, which a session that is
   * still a member gets again. */
  function refused(call, reason) {
    turns.refused += 1;
    stopTurns();
    stopEncoders();
    // No call is granted any more: the device list is the host's to say again.
    grant = undefined;
    update({phase: 'reconnecting', camera: false, screen: false, participants: []});
    try { signalling.reachable?.(); } catch (error) { report({type: 'voice-signalling-error', error}); }
    startGrantDeadline(() => {
      if (state.call === call && !grant && inCall()) admissionEnded(call, reason);
    });
  }

  function granted(next) {
    if (state.phase === 'idle' || state.phase === 'ended' || next.call !== state.call) return;
    grant = next;
    clearGrantDeadline();
    // Back after the host took the call away: audio again, as wanted.
    if (!audioSession && !audioStarting) void startAudio();
    const canPublish = next.directions.publish;
    if (!canPublish && !state.muted) void audioSession?.setMuted(true);
    update({phase: state.phase === 'joining' ? 'connecting' : state.phase, admitted: true, canPublish, muted: canPublish ? state.muted : true, participants: participants()});
    encodeVideo();
    pump();
    schedule();
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
      // waits, reconnecting, for the network.
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

  /** Leave the call on the host's side: its bridge forgets this page. */
  function leaveCall() {
    try { signalling.end?.(); } catch (error) { report({type: 'voice-signalling-error', error}); }
    try { signalling.leave?.(); } catch (error) { report({type: 'voice-signalling-error', error}); }
  }

  function stopAll() {
    clearGrantDeadline();
    stopTurns();
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
    else if (event.kind === 'ended') ended(event.call, event.reason);
  });
  const online = () => networkChanged();
  environment.addEventListener?.('online', online);
  const connection = environment.navigator?.connection;
  connection?.addEventListener?.('change', online);

  /** The network changed: the turn in flight is gone, or will be soon. */
  function networkChanged() {
    if (!inCall()) return;
    attempt = 0;
    if (retryTimer !== undefined) { clearTimer(retryTimer); retryTimer = undefined; }
    stopTurns();
    pauseCapture();
    schedule();
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
      attempt = 0;
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
      for (const item of shown) {
        for (const participant of grant?.participants ?? []) {
          if (participant.self || participant.user !== item.person || !participant.device) continue;
          const key = `${participant.device}|${item.source}`;
          if (!asking.has(key)) asking.set(key, true);
        }
      }
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
    /** Turn and audio counters, for diagnostics and tests. */
    stats: () => ({phase: state.phase, captureActive: audioSession?.captureActive === true, turns: {...turns, inFlight: turning}}),
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

/** Bytes from the turn's JSON: base64, as every payload of it travels. */
export function base64OfDecoded(text) {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

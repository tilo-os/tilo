/**
 * The browser's half of a page's call: the steps a view asks for, carried out by the browser's
 * own call (`web/voice.mjs`, behind `peer.voice`), and the status it answers with, in the
 * vocabulary the view's call service reads (`runtime_collab::native_media::Snapshot`).
 *
 * A view that shows a call is laid out where the page's device hosts it, and each step arrives
 * over the page's own stream. The microphone, camera, screen and the workspace's live session
 * are here, in the browser, so the step is carried out here and the status goes back the same
 * way. Nothing of a call is stored: who is in one is its presence.
 *
 * A `watch` is held, as a native call service holds it: until the status it names has moved, or
 * for `WATCH_HOLD_MS`. That is how the view learns who joined, left, talks or mutes the moment
 * it happens, with one question outstanding instead of a poll.
 *
 * The call's pictures do not go back to the view: they are decoded here, where the page draws
 * its tiles, and the status says so (`pictures_local`). `paint(person, source, frame)` gets each
 * decoded frame and `onStatus(status, me)` the status whenever it moves, which is what keeps a
 * tile's speaking ring and microphone right.
 */

/** The longest a `watch` is held before it is answered unchanged (`WATCH_HOLD`). */
export const WATCH_HOLD_MS = 20_000;

/** `phase` as the status names the call's state. */
export function stateOfPhase(phase) {
  switch (phase) {
    case 'idle': return 'idle';
    case 'joining':
    case 'connecting': return 'joining';
    case 'live': return 'live';
    case 'reconnecting': return 'reconnecting';
    case 'ended': return 'ended';
    default: return 'unavailable';
  }
}

/** Why the browser's call cannot carry what it was asked for, as the status names it, or null. */
export function issueOfState(state) {
  if (state?.unsupported) return 'unsupported';
  if (state?.micIssue) return state.micIssue;
  if (state?.videoIssue) return state.videoIssue;
  return null;
}

/** The people a listing names in one of its lists, sorted and distinct: what a roster holds. */
const sortedPeople = list => [...new Set((list ?? []).filter(id => typeof id === 'string'))].sort();

/** One call as the listing says it is, in the roster's vocabulary. */
export function callIn(call) {
  return {
    channel_id: call.channel,
    people: sortedPeople(call.people),
    cameras: sortedPeople(call.cameras),
    screens: sortedPeople(call.screens),
    muted: sortedPeople(call.muted),
    deafened: sortedPeople(call.deafened),
    bridges: sortedPeople(call.bridges),
    bridged: (call.bridged ?? []).map(person => ({
      person: person.person,
      service: person.service,
      account: person.account,
      name: person.name,
      ...(person.avatar ? {avatar: person.avatar} : {}),
    })),
  };
}

/**
 * A page's calls. `peerVoice` is `peer.voice` (`web/voice-peer.mjs`): its `call` is this page's
 * call and its `calls` is what the workspace's presence lists. `send` takes one answer to give
 * the view — `{answered: status}`, the step's answer, or `{refused: why}`. `paint` draws each
 * picture the call's decoder makes where the page's tiles stand, and `onStatus` hears the status
 * whenever it moves, with this page's person.
 */
export function createPageCalls({
  peerVoice,
  send,
  paint = () => {},
  onStatus = () => {},
  holdMs = WATCH_HOLD_MS,
  setTimeout: after = (callback, ms) => globalThis.setTimeout(callback, ms),
  clearTimeout: cancel = handle => globalThis.clearTimeout(handle),
} = {}) {
  if (!peerVoice?.call) throw new TypeError('peerVoice.call is required');
  if (typeof send !== 'function') throw new TypeError('send is required');
  const call = peerVoice.call;
  /** The status number: it moves whenever anything the status says does. */
  let revision = 0;
  let said = '';
  /** The `watch` the view waits on, if any: the revision it last saw, and the timer that ends
   * its hold. */
  let held = null;
  /** The status `onStatus` last heard. */
  let told = 0;
  let disposed = false;

  /** The microphones and cameras the browser offers, as the status says them. They are listed
   * again as the call moves, since a browser names its devices only once the page may use one. */
  let devices = {inputs: [], cameras: []};
  let listingDevices = false;
  function listDevices() {
    if (listingDevices || disposed) return;
    listingDevices = true;
    const named = list => {
      const seen = new Set();
      return (list ?? []).filter(device => device.deviceId && !seen.has(device.deviceId) && seen.add(device.deviceId))
        .map(device => ({id: device.deviceId, label: device.label || device.deviceId}));
    };
    Promise.all([
      typeof call.inputs === 'function' ? call.inputs() : [],
      typeof call.cameras === 'function' ? call.cameras() : [],
    ]).then(([microphones, cameras]) => {
      const next = {inputs: named(microphones), cameras: named(cameras)};
      if (JSON.stringify(next) === JSON.stringify(devices)) return;
      devices = next;
      status();
      moved();
    }).catch(() => {}).finally(() => { listingDevices = false; });
  }

  const listing = () => {
    try { return peerVoice.calls ?? []; } catch { return []; }
  };

  /** The status now, as the view's call service reads it. */
  function status() {
    const state = call.state ?? {};
    const calls = listing();
    const rosters = calls.map(callIn);
    const speaking = sortedPeople(calls.flatMap(one => (one.members ?? [])
      .filter(member => member.speaking)
      .map(member => member.user)));
    const issue = issueOfState(state);
    const volumes = typeof call.volumes === 'function' ? call.volumes() : [];
    const now = JSON.stringify([stateOfPhase(state.phase), state.muted, state.camera, state.screen,
      state.deafened, state.inputId, state.cameraId, issue, rosters, speaking, volumes, devices]);
    if (now !== said) {
      said = now;
      revision += 1;
    }
    const answer = {
      version: 1,
      state: stateOfPhase(state.phase),
      muted: Boolean(state.muted),
      capture_active: !state.muted && state.phase === 'live',
      input_id: state.inputId ?? null,
      inputs: devices.inputs,
      issue,
      inventory_revision: null,
      publication: null,
      publishers: [],
      rosters,
      speaking,
      camera: Boolean(state.camera),
      screen: Boolean(state.screen),
      deafened: Boolean(state.deafened),
      volumes,
      cameras: devices.cameras,
      camera_id: state.cameraId ?? devices.cameras[0]?.id ?? null,
      // The pictures are decoded here, where the page draws its tiles: the view's call service
      // asks for none and collects none.
      pictures_local: true,
      revision,
    };
    if (revision !== told) {
      told = revision;
      const me = (state.members ?? []).find(member => member.self)?.user ?? null;
      try { onStatus(answer, me); } catch {}
    }
    return answer;
  }

  function answered(requestId) {
    send({request_id: requestId, answer: {answered: status()}});
  }

  /** Hold `requestId` until the call moves past `seen`, or its hold runs out. */
  function watch(requestId, seen) {
    if (held) clearTimeout(held.timer);
    if (seen < revision) {
      answered(requestId);
      return;
    }
    const timer = after(() => {
      if (held?.requestId !== requestId) return;
      held = null;
      answered(requestId);
    }, holdMs);
    held = {requestId, seen, timer};
  }

  /** Answer a held watch whose status has moved; every other change waits for its own watch. */
  function moved() {
    if (!held) return;
    if (revision <= held.seen) return;
    const {requestId, timer} = held;
    held = null;
    clearTimeout(timer);
    answered(requestId);
  }

  const unsubscribe = typeof call.subscribe === 'function'
    ? call.subscribe(() => { listDevices(); status(); moved(); })
    : null;
  const unsubscribeCalls = typeof peerVoice.onCalls === 'function'
    ? peerVoice.onCalls(() => { status(); moved(); })
    : null;
  /** Every picture the call decodes, drawn where the page's tiles stand. */
  const unsubscribePictures = typeof call.onPicture === 'function'
    ? call.onPicture((person, source, frame) => { if (!disposed) paint(person, source, frame); })
    : null;
  // The page wears what the call says now, before any step arrives.
  status();
  listDevices();

  return {
    /** Carry out the step `step` (`{request_id, command}`, as the page's stream carries it). A
     * `watch` is held; every other step is answered at once, with the status it leaves. */
    apply(step) {
      if (disposed) return;
      const command = step?.command?.command;
      const requestId = Number(step?.request_id);
      if (!command || !Number.isSafeInteger(requestId)) return;
      switch (command.op) {
        case 'join':
          // As on a Tilo device: joining is being in the call, muted and listening, and the
          // browser is asked for the microphone only when the person unmutes.
          call.join(command.channel_id, {muted: true});
          break;
        case 'leave':
          call.leave();
          break;
        case 'set_muted':
          void call.setMuted(Boolean(command.muted));
          break;
        case 'set_deafened':
          void call.setDeafened(Boolean(command.deafened));
          break;
        case 'set_camera':
          void call.setVideo('camera', Boolean(command.on));
          break;
        case 'set_screen':
          void call.setVideo('screen', Boolean(command.on));
          break;
        case 'select_input':
          void call.selectInput(command.input_id);
          break;
        case 'select_camera':
          if (typeof call.selectCamera !== 'function') {
            send({request_id: requestId, answer: {refused: 'This browser cannot choose a camera'}});
            return;
          }
          void call.selectCamera(command.camera_id);
          break;
        case 'set_volume':
          call.setVolume(command.member, Number(command.percent) / 100);
          break;
        case 'show':
          call.showPictures(command.pictures);
          break;
        case 'watch':
          watch(requestId, Number(command.revision) || 0);
          return;
        case 'status':
          break;
        default:
          send({request_id: requestId, answer: {refused: `The page has no step \`${command.op}\``}});
          return;
      }
      status();
      answered(requestId);
    },
    /** The status as it stands. */
    status,
    dispose() {
      disposed = true;
      if (held) clearTimeout(held.timer);
      held = null;
      unsubscribe?.();
      unsubscribeCalls?.();
      unsubscribePictures?.();
    },
  };
}

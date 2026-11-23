/**
 * `peer.voice`: the workspace's calls through the page's certified peer.
 *
 * Being in a call is presence: `WasmPeer.callJoin` says in this page's
 * presence which voice channel it is in, `callShow` what it shows there and
 * `callLeave` that it left, and `callChannels` lists who every device of the
 * workspace says is in which channel. No host keeps a call. The workspace's
 * live session carries the call's media, which only its members open:
 * `callAudio` publishes the microphone's frames as they are made,
 * `callPictures` the pictures its encoders make, `callShowing` what the page
 * draws of the call's devices and `callPictureRestart` a keyframe it needs;
 * `takeVoiceEvents` carries what the live session delivers. What each call
 * allows is the workspace's: `moderateCall` asks its `call` action to end,
 * restart or deny a call, and `callAllows` says what it lets this page's
 * person do, so a call that was ended or a person denied it drops out here as
 * it does on every other member's device. This adapts all of it to the
 * signalling web/voice.mjs follows, and lists and moderates the workspace's
 * calls for the desktop.
 */
import {createVoiceCall} from './voice.mjs';

/**
 * `peer()` is the current `WasmPeer` (it changes when the page certifies
 * again); `workspace` the joined workspace; `device` this page's own device,
 * which its frames carry as their sender; `person` the person this page acts
 * for when it moderates a call. `options` go to `createVoiceCall`.
 */
export function createPeerVoice({peer, workspace, device = null, person = '', createCall = createVoiceCall, ...options}) {
  const listeners = new Set();
  const callListeners = new Set();
  let calls = [];
  let listed = '[]';
  /** The channel this page is in, and the members it last told the call of. */
  let joined = null;
  let told = '';
  const emit = event => { for (const listener of [...listeners]) { try { listener(event); } catch {} } };

  /** The workspace's calls as its presence says them now: one per channel anyone is in. */
  function refresh() {
    let channels;
    try { channels = JSON.parse(peer().callChannels(workspace)); } catch { channels = null; }
    const now = JSON.stringify(channels ?? []);
    if (now !== listed) {
      listed = now;
      calls = (channels ?? []).map(channel => ({...channel, call: channel.channel, active: true}));
      for (const listener of [...callListeners]) { try { listener(calls); } catch {} }
    }
    if (joined === null) return calls;
    // What the workspace lets this page's person do in its call: a call that was ended, or a
    // person denied it, drops this page out of it, as every other member's device leaves it out.
    let allows;
    try { allows = JSON.parse(peer().callAllows(workspace, joined)); } catch { allows = null; }
    const directions = {publish: allows?.publish ?? true, subscribe: allows?.subscribe ?? true};
    if (!directions.publish && !directions.subscribe) {
      const call = joined;
      leave();
      emit({kind: 'ended', call, reason: allows?.ended ? 'the call was ended' : 'you were denied this call'});
      return calls;
    }
    // Who is in this page's call: everyone saying the channel, this page among them.
    const members = calls.find(call => call.channel === joined)?.members ?? [];
    const said = JSON.stringify({directions, members});
    if (said !== told) {
      told = said;
      emit({kind: 'granted', grant: {call: joined, directions, members}});
    }
    return calls;
  }

  function leave() {
    joined = null;
    told = '';
    try { peer().callLeave(workspace); } catch {}
  }

  /** Ask the workspace's `call` action to `operation` the call in `channel`, with what the
   * operation names; resolves to `{call, active, allowed}` as the call stands after it. */
  async function moderate(operation, channel, named = {}) {
    const answer = JSON.parse(await peer().moderateCall(workspace, person, JSON.stringify({operation, channel, ...named})));
    queueMicrotask(refresh);
    const rule = answer?.call ?? {};
    return {call: channel, active: !rule.ended, allowed: rule.allowed ?? {publish: true, subscribe: true}, denied: rule.denied ?? {}};
  }

  const signalling = {
    join: channel => {
      peer().callJoin(workspace, channel);
      joined = channel;
      told = '';
      queueMicrotask(refresh);
    },
    leave,
    show: (camera, screen, muted, deafened) => { try { peer().callShow(workspace, camera, screen, muted, deafened); } catch {} },
    // The microphone's frames, as they are made: published on the
    // workspace's live session as this page's voice.
    audio: frames => { try { peer().callAudio(workspace, JSON.stringify(frames)); } catch {} },
    // The pictures the page's encoders made, as they are made: published on
    // the live session as this page's camera and screen.
    picture: pictures => { try { peer().callPictures(workspace, JSON.stringify(pictures)); } catch {} },
    // What the page draws of each device: the live session reads exactly those
    // layers, and their owners encode only them.
    showing: shown => { try { peer().callShowing(workspace, JSON.stringify(shown)); } catch {} },
    // A device's next picture of `source` starts a group this page can decode
    // from.
    keyframe: (device, source) => { try { peer().callPictureRestart(workspace, device, source); } catch {} },
    onEvent(listener) { listeners.add(listener); return () => listeners.delete(listener); },
  };
  const call = createCall({signalling, device, ...options});

  function clear() {
    joined = null;
    told = '';
    calls = [];
    listed = '[]';
    for (const listener of [...callListeners]) { try { listener(calls); } catch {} }
  }

  return {
    /** The call this page takes part in (web/voice.mjs). */
    call,
    /** The workspace's calls as last listed. */
    get calls() { return calls; },
    /** Call `listener(calls)` whenever the calls are listed; returns its removal. */
    onCalls(listener) { callListeners.add(listener); return () => callListeners.delete(listener); },
    /** The workspace's calls, as its presence says them: `[{call, channel, name, active,
     * members, people, cameras, screens, muted, deafened}]`, `call` the channel. */
    list: () => Promise.resolve(refresh()),
    /** A voice channel's call to join: a channel's call is whoever says they are in it, so
     * every channel has one. Resolves to `{call, active}`. */
    open: ({channel}) => {
      let allows;
      try { allows = JSON.parse(peer().callAllows(workspace, channel)); } catch { allows = null; }
      return Promise.resolve({call: channel, active: !allows?.ended});
    },
    /** Start the call in a voice channel again, allowing everyone in it `allowed` (the workspace's
     * owner, and whoever it grants `call`). */
    start: ({channel, allowed = {publish: true, subscribe: true}}) =>
      moderate('start', channel, {publish: allowed.publish !== false, subscribe: allowed.subscribe !== false}),
    /** End a call for everyone in it (whoever the workspace grants `call`). */
    end: ({call}) => moderate('end', call),
    /** Restart an ended call (whoever the workspace grants `call`). */
    restart: ({call}) => moderate('restart', call),
    /** Take back from `person` (their root as hex) what `denied` names in the call (both
     * directions unless it says otherwise). */
    deny: ({call, person: denied, publish = true, subscribe = true}) => moderate('deny', call, {person: denied, publish, subscribe}),
    /** Give `person` back what the call allows everyone. */
    allow: ({call, person: allowed}) => moderate('allow', call, {person: allowed}),
    /** What everyone in the call may do. */
    setDirections: ({call, publish = true, subscribe = true}) => moderate('set-directions', call, {publish, subscribe}),
    /** Take the wasm peer's call events (`takeVoiceEvents` JSON), and what its presence now says
     * of the call this page is in. */
    pump(json) {
      let events;
      try { events = JSON.parse(json); } catch { events = []; }
      for (const event of events) if (event.workspace === workspace) emit(event);
      if (joined !== null) refresh();
    },
    /** Retire the old identity's media before replacing its certified peer. */
    reset() {
      call.leave();
      clear();
    },
    dispose() {
      call.dispose();
      listeners.clear();
      callListeners.clear();
      clear();
    },
  };
}

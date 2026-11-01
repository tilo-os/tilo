/**
 * `peer.voice`: the workspace's calls through the page's certified peer.
 *
 * The wasm peer (`WasmPeer.voiceCalls`, `voiceCall`, `voiceJoin`,
 * `voiceLeave`, `takeVoiceEvents`) carries the call envelopes over the
 * peer's HTTPS session and keeps the participation alive; this adapts it to
 * the signalling web/voice.mjs follows, and lists and governs the workspace's
 * calls for the desktop.
 */
import {createVoiceCall} from './voice.mjs';

const CALLS_TIMEOUT_MS = 10_000;
const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');

/**
 * `peer()` is the current `WasmPeer` (it changes when the page certifies
 * again); `workspace` the joined workspace. `options` go to `createVoiceCall`.
 */
export function createPeerVoice({peer, workspace, createCall = createVoiceCall, random = bytes => crypto.getRandomValues(bytes), ...options}) {
  const listeners = new Set();
  const listings = [];
  const requests = new Map();
  let calls = [];
  const callListeners = new Set();
  const signalling = {
    join: call => peer().voiceJoin(workspace, call),
    leave: () => { try { peer().voiceLeave(workspace); } catch {} },
    // The host just answered the carrier: a session waiting to reconnect
    // can now, and brings the fresh grant the call waits for.
    reachable: () => { try { peer().retryNow?.(); } catch {} },
    show: (camera, screen, muted, deafened) => { try { peer().voiceShow(workspace, camera, screen, muted, deafened); } catch {} },
    onEvent(listener) { listeners.add(listener); return () => listeners.delete(listener); },
  };
  const call = createCall({signalling, ...options});

  function listed(result) {
    if (result.calls) {
      calls = result.calls;
      for (const listener of [...callListeners]) { try { listener(calls); } catch {} }
    }
    for (const waiter of listings.splice(0)) {
      if (result.calls) waiter.resolve(result.calls);
      else waiter.reject(new Error(result.error ?? 'the host listed no calls'));
    }
  }

  function clearPending(reason) {
    calls = [];
    for (const listener of [...callListeners]) {try {listener(calls);} catch {}}
    for (const waiter of listings.splice(0)) waiter.reject(new Error(reason));
    for (const waiter of requests.values()) waiter.reject(new Error(reason));
    requests.clear();
  }

  function operate(operation) {
    const request = hex(random(new Uint8Array(16)));
    return new Promise((resolve, reject) => {
      requests.set(request, {resolve, reject});
      try { peer().voiceCall(workspace, JSON.stringify(operation), request); }
      catch (error) { requests.delete(request); reject(error); }
    });
  }

  return {
    /** The call this page takes part in (web/voice.mjs). */
    call,
    /** The workspace's calls as last listed. */
    get calls() { return calls; },
    /** Call `listener(calls)` whenever a listing arrives; returns its removal. */
    onCalls(listener) { callListeners.add(listener); return () => callListeners.delete(listener); },
    /** Ask the host for the workspace's calls: `[{call, epoch, active, allowed,
     * app, record, channel, participants}]`. */
    list() {
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          const index = listings.indexOf(waiter);
          if (index >= 0) listings.splice(index, 1);
          reject(new Error('the host did not list the calls'));
        }, CALLS_TIMEOUT_MS);
        const waiter = {
          resolve: value => { clearTimeout(timer); resolve(value); },
          reject: error => { clearTimeout(timer); reject(error); },
        };
        listings.push(waiter);
        try { peer().voiceCalls(workspace); } catch (error) { waiter.reject(error); listings.splice(listings.indexOf(waiter), 1); }
      });
    },
    /** Open a voice channel's call to join it, starting it when it has none
     * (any member): resolves to `{call, epoch, active, allowed}`. */
    open: ({app, record = '', channel}) => operate({kind: 'open', app, record, channel}),
    /** Start a call on a chat's voice channel (the workspace's owner only). */
    start: ({app, record = '', channel, allowed = {publish: true, subscribe: true}}) => operate({kind: 'start', app, record, channel, allowed}),
    /** End a call for everyone (the workspace's owner only). */
    end: ({call: id, epoch}) => operate({kind: 'end', call: id, epoch}),
    /** Restart an ended call (the workspace's owner only). */
    restart: ({call: id, epoch}) => operate({kind: 'restart', call: id, epoch}),
    /** Take the wasm peer's call events (`takeVoiceEvents` JSON). */
    pump(json) {
      let events;
      try { events = JSON.parse(json); } catch { return; }
      for (const event of events) {
        if (event.room !== workspace) continue;
        if (event.kind === 'calls') listed(event);
        else if (event.kind === 'callResult') {
          const waiter = requests.get(event.request);
          requests.delete(event.request);
          if (event.call) waiter?.resolve(event.call); else waiter?.reject(new Error(event.error ?? 'the host refused'));
        } else {
          for (const listener of [...listeners]) { try { listener(event); } catch {} }
        }
      }
    },
    /** Retire the old identity's media before replacing its certified peer. */
    reset() {
      call.leave();
      clearPending('voice identity changed');
    },
    dispose() {
      call.dispose();
      listeners.clear();
      callListeners.clear();
      clearPending('voice disposed');
    },
  };
}

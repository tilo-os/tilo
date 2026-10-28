import {localized} from './arrival.mjs';

/**
 * What the bar says about this desktop's connection. It is taken from the
 * peer session's own health (`PeerSnapshot.health`, `SessionHealth` v1)
 * instead of from the fact that the desktop once mounted.
 *
 * A page that derived "connected" from its mount kept saying so after the
 * transport dropped, after the host stopped admitting its device, after the
 * profile stopped saving and after the renderer stopped drawing: none of
 * those unmount anything, and the person went on editing on a promise
 * nothing backed. So the bar follows the session: the transport it is on,
 * the host's admission of this device, the store's persistence of changes
 * and the desktop's own drawing, each in the person's words, and it clears
 * to nothing once all of them are well again. Nothing here touches the
 * page's work: a change made while the transport is down waits in the
 * profile and goes when the transport is back.
 */

/** What each kind of trouble says, in English where a locale lacks the line
 * (`arrival-failed-<kind>` in the locale file, as the page's other failures). */
export const WORDS = {
  offline: 'You’re offline. Changes are kept on this device and sent once the connection is back.',
  admission: 'The workspace can’t take this browser’s changes right now. Changes are kept on this device.',
  saving: 'Your changes aren’t saved on this device yet. Keep this tab open and try again.',
  renderer: 'The desktop stopped drawing. Try again to start it, or reload the page.',
};

/** A desktop that presented a frame (the two phases that follow one). */
const presented = phase => phase === 'presented' || phase === 'gpu-pending';

/**
 * The bar's reading of the session and the desktop: `{state, sentence,
 * detail}`, where `state` is `online`, `connecting` or the kind of trouble
 * (`offline`, `admission`, `saving`, `renderer`); `sentence` is what the person
 * reads (`null`: the bar stays empty) and `detail` the cause, for the
 * console.
 *
 * `desktop` is the mount's last reported status, `mounted` marks that the
 * page mounted a desktop (its `phase` says whether one drew), `peer` the
 * peer's `PeerSnapshot` (`null` for a page with no peer: the person's own
 * desktop, which has no session to be cut off from), and `saved` whether the
 * store last committed every change (the page's store reports it, above the
 * mount's own `persistence`: a mount reports on the console it draws in).
 */
export function connectionStatus({peer = null, desktop = null, mounted = false, saved = true, messages = null} = {}) {
  const failed = failureOf({peer, desktop, saved, messages});
  if (failed) return {state: failed.kind, sentence: failed.sentence, detail: failed.detail};
  if (!mounted || !presented(desktop?.phase)) return {state: 'connecting', sentence: null, detail: null};
  // A session still finding its transport, or its host still admitting its
  // rooms, is on its way rather than cut off, and says so in its own line:
  // nothing to add here.
  if (pending(peer)) return {state: 'connecting', sentence: null, detail: null};
  return {state: 'online', sentence: null, detail: null};
}

/** Whether the session has not caught up with the host yet: it is still
 * authenticating or admitting, or commits wait for its acknowledgement. */
function pending(peer) {
  if (!peer) return false;
  if (['authenticating', 'admitting', 'incomplete'].includes(peer.phase)) return true;
  return Number(peer.outgoingPending) > 0;
}

/** The worst thing wrong with the desktop right now (`null` when nothing
 * is), in the order the person most needs to know it. */
function failureOf({peer, desktop, saved, messages}) {
  const said = kind => localized(messages, `arrival-failed-${kind}`, WORDS[kind]);
  const health = peer?.health ?? null;
  const transport = health?.transport?.phase ?? null;
  const cut = peer !== null
    && (peer.phase === 'closed' || peer.phase === 'failed' || ['offline', 'failed', 'closed', 'reconnecting'].includes(transport));
  if (cut) {
    return {
      kind: 'offline',
      detail: `session ${peer.phase ?? 'unknown'}${transport ? `, transport ${transport}` : ''}`,
      sentence: said('offline'),
    };
  }
  if (health?.admission?.phase === 'failed') {
    return {
      kind: 'admission',
      detail: 'the host did not admit this device’s room',
      sentence: said('admission'),
    };
  }
  if (!saved || health?.failures?.some(failure => failure.scope === 'storage')) {
    return {
      kind: 'saving',
      detail: 'the store could not commit every change',
      sentence: said('saving'),
    };
  }
  if (desktop?.phase === 'failed') {
    return {
      kind: 'renderer',
      detail: desktop?.error ? `the desktop stopped: ${desktop.error}` : 'the desktop stopped drawing',
      sentence: said('renderer'),
    };
  }
  return null;
}

/**
 * Follow a page's connection into `bar` (`show({state, sentence, detail,
 * cleared})`, and `showDebug(state, detail)` with the cause, for the
 * console).
 * `read()` gives what [`connectionStatus`] takes, `look()` re-reads after a
 * status arrives, and the function it returns stops the following.
 *
 * A line already on the bar is not replaced by a milder one, and it clears
 * (`cleared: true`) once nothing is wrong: a connection is a state, not an
 * event, so the bar says it until it is true again, not for a moment.
 * `read()` that throws (a peer disposed as it was read) reads as nothing to
 * say, so the bar cannot keep a line the page can no longer check.
 */
export function followConnection({read, bar = {}}) {
  let said = null;
  let stopped = false;
  /** The last line the bar took, for a page that says it in its own line. */
  let shown = null;
  const look = () => {
    if (stopped) return;
    let reading;
    try { reading = connectionStatus(read()); } catch { reading = {state: 'connecting', sentence: null, detail: null}; }
    // A milder trouble waits for the worse one to clear first, and the
    // same state again is nothing new.
    if (said !== null && troubles(reading.state, said)) return;
    if (reading.state === said) return;
    const was = said;
    // "Online" and "on its way" are not troubles: nothing stands on the bar.
    said = ['online', 'connecting'].includes(reading.state) ? null : reading.state;
    shown = reading;
    // The line goes up as it takes (or leaves) the bar; the cause goes to
    // the console beside it, once.
    if (said !== null) bar.show?.(reading);
    else if (was !== null) bar.show?.({...reading, cleared: true});
    if (reading.detail) bar.showDebug?.(reading.state, reading.detail);
  };
  look();
  return {get last() { return shown; }, look, stop() { stopped = true; }};
}

/** The kinds of trouble, worst first. */
const SEVERITY = ['offline', 'admission', 'saving', 'renderer'];

/** Whether `next` reads milder than the trouble `said`, and so waits for
 * the bar instead of taking it. */
function troubles(next, said) {
  const [rank, held] = [SEVERITY.indexOf(next), SEVERITY.indexOf(said)];
  if (rank === -1 || held === -1) return false;
  return rank > held;
}

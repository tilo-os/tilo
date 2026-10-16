/**
 * What an open page does when its workspace's host turns the session away:
 * the peer's snapshot is `failed`, its `errorCode` the host's own reason
 * (the typed handshake refusal, `SessionRefusal`), never a count.
 *
 * - `not-admitted`: the host no longer has the person (its state was reset,
 *   it pruned them after an absence, or a room took them out), and
 *   `host-changed`: another host answers at the origin now. A page that came
 *   in through the host's door (`canRejoin`) joins again through it, exactly
 *   as it does on arrival (`rejoin`, which loads the bare entry): the person
 *   sees the workspace loading again and nothing else. The door decides
 *   whether they may: one that turns them away (they were removed) is said on
 *   arrival, as any refusal is.
 * - `certificate-rejected`: the host did not take the device's certificate.
 *   The page certifies the device again with the identity it holds
 *   (`recover({kind: 'reauthenticate'})`), once.
 * - `device-revoked`, a page with no door to join through again, a second
 *   refusal soon after joining again, or a failed re-certification: `refuse`
 *   says what happened (`refusalText`); there is nothing to try again.
 *
 * Joining again is remembered for `REJOIN_WINDOW_MS` in the tab's session
 * storage (`rejoinedRecently`), with the name the person went by
 * (`rejoinedName`): a host that lets the person in at its door and turns
 * them away again is not looped, and the page it reloads into joins under
 * the same name (a guest's is made up per visit, and kept nowhere else).
 * Their cursor colour follows the device key, which stays. Nothing here asks
 * the person about hosts or certificates.
 */

/** Session storage key: when this tab last joined again by itself, and as
 * whom: `{at, name}` (ms since the epoch, the display name). */
export const REJOINED = 'tilo-rejoined';
/** How long a join again by itself keeps the next one from happening. */
export const REJOIN_WINDOW_MS = 2 * 60 * 1000;
/** The failure codes a host's refusal ends a session with. */
export const REFUSALS = new Set(['not-admitted', 'host-changed', 'device-revoked', 'certificate-rejected']);

function recentRejoin({storage = globalThis.sessionStorage, now = Date.now()} = {}) {
  let kept;
  try { kept = JSON.parse(storage?.getItem(REJOINED) ?? 'null'); } catch { return null; }
  const at = Number(kept?.at);
  return Number.isFinite(at) && at > 0 && now - at >= 0 && now - at < REJOIN_WINDOW_MS ? kept : null;
}

/** Whether this tab joined again by itself within `REJOIN_WINDOW_MS`. */
export function rejoinedRecently(options) {
  return recentRejoin(options) !== null;
}

/** The name the person went by when this tab last joined again by itself,
 * within `REJOIN_WINDOW_MS`, or `null`. */
export function rejoinedName(options) {
  const name = recentRejoin(options)?.name;
  return typeof name === 'string' && name ? name : null;
}

function noteRejoin({storage, now, name}) {
  try {
    storage?.setItem(REJOINED, JSON.stringify({at: now, ...(name ? {name} : {})}));
  } catch { /* No storage: the page cannot loop past a reload either way. */ }
}

/** Waits between tries at a door that did not answer yet, in ms. */
const DOOR_RETRIES_MS = [1000, 2000, 4000, 8000, 15_000, 15_000, 15_000, 15_000];

/**
 * After joining again by itself, the door did not answer (a host set up
 * again starts its relay before its door): how long to wait before the page
 * tries again, or `null` once it has tried enough or did not come here by
 * itself. Counts the try.
 */
export function retryRejoin({storage = globalThis.sessionStorage, now = Date.now()} = {}) {
  const kept = recentRejoin({storage, now});
  if (!kept) return null;
  const tries = Number.isInteger(kept.tries) ? kept.tries : 0;
  if (tries >= DOOR_RETRIES_MS.length) return null;
  try {
    storage?.setItem(REJOINED, JSON.stringify({...kept, tries: tries + 1}));
  } catch { return null; }
  return DOOR_RETRIES_MS[tries];
}

/** What the person is told when the host's refusal `code` cannot be undone
 * here, about the workspace `name` (or `null`). */
export function refusalText(code, name = null) {
  const workspace = name || 'this workspace';
  if (code === 'device-revoked') {
    return `This browser was removed from your devices, so it can’t open ${workspace} anymore.`;
  }
  if (code === 'host-changed') {
    return `${name || 'This workspace'} was reset, so this browser can’t open it anymore. Ask the person who shared it for a new invitation.`;
  }
  return `${name || 'This workspace'} doesn’t let this browser in anymore. You may have been removed from it, or it was closed.`;
}

/**
 * Follow `peer`'s session for a refusal. `canRejoin(code)` says whether this
 * page entered through a door it can enter through again; `rejoin(code)`
 * leaves the page to do so, as the person named `displayName()`;
 * `refuse(code)` tells the person. It stops following once it joins again or
 * tells; the function it returns stops it sooner.
 */
export function followRefusal({
  peer, canRejoin, rejoin, refuse, displayName = () => null, storage = globalThis.sessionStorage, now = () => Date.now(),
}) {
  let acting = false;
  let reauthenticated = false;
  let stopped = false;
  const act = async snapshot => {
    if (stopped || acting || snapshot?.phase !== 'failed' || !REFUSALS.has(snapshot.errorCode)) return;
    const code = snapshot.errorCode;
    acting = true;
    try {
      if (code === 'certificate-rejected' && !reauthenticated) {
        reauthenticated = true;
        try {
          await peer.recover({kind: 'reauthenticate'});
          return;
        } catch (error) {
          console.warn('session: certifying this browser again failed', error?.code ?? error?.name ?? 'error');
        }
      } else if (code !== 'device-revoked' && code !== 'certificate-rejected' && canRejoin(code)
        && !rejoinedRecently({storage, now: now()})) {
        stop();
        noteRejoin({storage, now: now(), name: displayName()});
        await rejoin(code);
        return;
      }
      stop();
      await refuse(code);
    } finally {
      acting = false;
    }
  };
  const unsubscribe = peer.subscribe?.(snapshot => { void act(snapshot); }) ?? (() => {});
  function stop() {
    stopped = true;
    unsubscribe();
  }
  void act(peer.getSnapshot?.());
  return stop;
}

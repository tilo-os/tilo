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

/** Session storage key: the tries of this tab at a host that did not
 * answer before it got in: `{since, last, tries}` (ms since the epoch of the
 * first and the latest, and how many). */
export const UNREACHED = 'tilo-unreached';
/** How long after the latest try the next one still counts with it: longer
 * than the longest backoff (`reconnect.mjs`). */
const UNREACHED_WINDOW_MS = 10 * 60 * 1000;

/** Whether `error` is a host not answering at all (restarting, or the
 * network between): nothing it refused. */
export const hostUnreachable = error => (error?.name === 'HostAdmissionError' && error.code === 'unavailable')
  || /^(fetch failed|Failed to fetch|NetworkError|Load failed)/.test(String(error?.message ?? ''));

/**
 * The host did not answer the page before it got in (a host restarting
 * starts its relay and door a moment apart): which try this was, counted
 * across the page's reloads (`attempt`, 0 for the first failure), and since
 * when it fails (`since`), for the reconnect screen's backoff. Counts it.
 */
export function retryUnreachable({storage = globalThis.sessionStorage, now = Date.now()} = {}) {
  let kept;
  try { kept = JSON.parse(storage?.getItem(UNREACHED) ?? 'null'); } catch { kept = null; }
  const last = Number(kept?.last);
  const recent = Number.isFinite(last) && last > 0 && now - last >= 0 && now - last < UNREACHED_WINDOW_MS;
  const attempt = recent && Number.isInteger(kept.tries) ? kept.tries : 0;
  const since = recent && Number.isFinite(Number(kept.since)) ? Number(kept.since) : now;
  try {
    storage?.setItem(UNREACHED, JSON.stringify({since, last: now, tries: attempt + 1}));
  } catch { /* No storage: each reload starts over, still waiting. */ }
  return {attempt, since};
}

/** The page got in: tries at a host that did not answer start over. */
export function reachedHost({storage = globalThis.sessionStorage} = {}) {
  try { storage?.removeItem(UNREACHED); } catch { /* Nothing kept. */ }
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

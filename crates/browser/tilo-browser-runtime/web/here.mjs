// Who is here in the desktop's workspace and what they are doing.
//
// The workspace's live layer says who is present now: each member's publisher,
// name, the record they focus, and for an agent what it is doing. Every few seconds the page asks the runtime what its own person is
// doing (`desktop_here_tick`: the window they focus and the titles of their
// own recent steps, never content) and posts it to the host's `/here`
// (`peer.here`), which answers with everyone's one-line status and recent
// summary. A host's workspace service also counts the people watching. All of it goes
// to the runtime as ambient presence; none of it is a notification.

/** How often the page checks what its person is doing. */
export const TICK_MS = 4_000;
/** The longest a page goes without posting, so the host keeps it listed. */
export const POST_EVERY_MS = 20_000;
/** How far back a person's own steps count as what they are doing. */
export const RECENT_MS = 10 * 60_000;
/** How often the watching count is read; the service caches it 5 s. */
export const WATCHING_EVERY_MS = 15_000;
/**
 * How long after someone arrives the page keeps asking the host, every
 * tick, for the line that says whose device theirs is: until it has it,
 * the desktop draws them neutral rather than guess a colour.
 */
export const PAIR_WAIT_MS = 30_000;

/**
 * Keep the runtime's view of who is here current while `active()`.
 *
 * @param options.runtime      the page runtime
 * @param options.mountId      the desktop mount's id
 * @param options.active       whether the mount is still shown
 * @param options.layer        the workspace's live layer, or undefined
 * @param options.peer         the certified peer: `here(window, actions)`, `participant`,
 *                             `liveNote()`, why this page shows no live presence
 * @param options.presenceUrl  the workspace service's watching count, if any
 * @param options.photos       follows people's profile pictures (`profile-photos.mjs` `followPhotos`), if any
 */
export function createHere({runtime, mountId, active, layer, peer, presenceUrl, photos = null,
  fetch = globalThis.fetch, now = () => Date.now(), setTimer = setTimeout, clearTimer = clearTimeout,
  visible = () => globalThis.document?.visibilityState !== 'hidden', onError = () => {}}) {
  let stopped = false;
  let timer;
  let postedKey;
  let postedAt = -Infinity;
  let posting = false;
  let watchedAt = -Infinity;
  let saidStatus = null; // the own words last put on the person's presence
  let saidNote = null; // the live note last logged, so a change is said once
  let lines = [];
  const joined = new Set(); // publishers here, as the last tick read them
  let again = false; // asked to check while busy: check once it is done
  const arrivedAt = new Map(); // publisher -> when it joined, while unpaired

  /** Why this page shows no live presence (a browser without WebTransport,
   * or a plane that could not be opened), said once here as it changes: a
   * page that retries quietly reads as an empty workspace. The page itself
   * shows it in the members' place. */
  function sayNote() {
    const unavailable = peer?.liveNote?.() ?? null;
    if (unavailable !== saidNote) {
      if (unavailable) console.warn(`live: ${unavailable}`);
      else if (saidNote) console.info('live: presence joined');
      saidNote = unavailable;
    }
  }

  /** Whether the host's last lines say whose device `publisher` is. */
  function paired(publisher) {
    return lines.some(line => line?.device === publisher && line.user);
  }
  /** Someone who arrived lately whose person the host has not named yet. */
  function awaitingPerson(at) {
    const members = layer()?.members?.() ?? [];
    for (const publisher of members) if (!joined.has(publisher)) { joined.add(publisher); arrivedAt.set(publisher, at); }
    for (const [publisher, since] of arrivedAt) {
      if (paired(publisher) || at - since >= PAIR_WAIT_MS || !members.includes(publisher)) arrivedAt.delete(publisher);
    }
    for (const publisher of [...joined]) if (!members.includes(publisher)) joined.delete(publisher);
    return arrivedAt.size > 0;
  }
  async function post(state, at) {
    posting = true;
    postedAt = at;
    try {
      const answer = await peer.here(state.window, state.actions);
      lines = Array.isArray(answer) ? answer : [];
      if (!stopped && active()) {
        runtime.desktop_set_here(mountId(), JSON.stringify(answer));
        photos?.seen(lines);
      }
    } catch (error) {
      // Not posted: try again on the next tick, not at the next refresh.
      postedKey = undefined;
      postedAt = -Infinity;
      throw error;
    } finally {
      posting = false;
    }
  }
  async function watch(at) {
    watchedAt = at;
    const response = await fetch(presenceUrl, {credentials: 'omit'});
    if (!response.ok) return;
    const {watching} = await response.json();
    if (!stopped && active() && Number.isInteger(watching) && watching >= 0) runtime.desktop_set_watching(mountId(), watching);
  }
  async function tick() {
    timer = undefined;
    if (stopped) return;
    try {
      sayNote();
      if (active() && visible()) {
        const at = now();
        const state = JSON.parse(runtime.desktop_here_tick(mountId(), at - RECENT_MS));
        // What they say they are doing, in their own words, travels on
        // their presence: everyone here reads it at once.
        const status = typeof state.status === 'string' && state.status ? state.status : null;
        if (status !== saidStatus && typeof peer?.setStatus === 'function') {
          saidStatus = status;
          peer.setStatus(status);
        }
        const key = JSON.stringify([state.window, state.actions.map(action => action.id)]);
        if (typeof peer?.here === 'function' && !posting && (key !== postedKey || at - postedAt >= POST_EVERY_MS || awaitingPerson(at))) {
          postedKey = key;
          await post(state, at);
        }
        if (presenceUrl && at - watchedAt >= WATCHING_EVERY_MS) await watch(at);
      }
    } catch (error) {
      onError(error);
    }
    if (!stopped) timer = setTimer(tick, again ? 0 : TICK_MS);
    again = false;
  }
  function wake() {
    if (stopped) return;
    if (!posting && timer !== undefined) { clearTimer(timer); void tick(); } else again = true;
  }
  void tick();

  return {
    /** Check now, e.g. after the person acted. */
    wake,
    stop() {
      stopped = true;
      if (timer !== undefined) clearTimer(timer);
    },
  };
}

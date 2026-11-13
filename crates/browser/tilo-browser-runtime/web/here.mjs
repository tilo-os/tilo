// What the page's own person is doing, said on their live presence, and the
// pictures of the people here.
//
// Who is here is the workspace's live layer: every device publishes its own
// presence there, a person's one-line status included, and every page reads
// everyone's from there. Every few seconds the page asks the runtime to say
// what its person is doing (`desktop_say_doing`: their own words, or what
// their own recent steps read as, titles only, never content); the runtime
// publishes it on their presence when it changed. The same tick follows the
// pictures of the people present (`photos`) and hands their names to the
// peer, which draws this person a name of their own when one here collides.
// None of it is a notification.

/** How often the page says what its person is doing. */
export const TICK_MS = 4_000;

/**
 * Keep this person's presence and the pictures of who is here current while `active()`.
 *
 * @param options.runtime      the page runtime
 * @param options.mountId      the desktop mount's id
 * @param options.active       whether the mount is still shown
 * @param options.layer        the workspace's live layer, or undefined
 * @param options.peer         the certified peer: `userPublicKey`, `liveNote()` (why this page
 *                             shows no live presence), `membersSeen(people)`
 * @param options.photos       follows people's profile pictures (`profile-photos.mjs` `followPhotos`), if any
 */
export function createHere({runtime, mountId, active, layer, peer, photos = null,
  now = () => Date.now(), setTimer = setTimeout, clearTimer = clearTimeout,
  visible = () => globalThis.document?.visibilityState !== 'hidden', onError = () => {}}) {
  let stopped = false;
  let timer;
  let saidNote = null; // the live note last logged, so a change is said once

  /** Why this page shows no live presence (a plane that could not be
   * reached), said once here as it changes: a
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

  /** The people here as their live presence names them, this person
   * included, one entry per person: `[{user, label, profile_ms}]`, the
   * newest profile stamp any of their devices said. */
  function people() {
    const byUser = new Map();
    const own = typeof peer?.userPublicKey === 'string' ? peer.userPublicKey : '';
    if (own) byUser.set(own, {user: own, label: '', profile_ms: 0});
    for (const member of layer()?.presenceMembers?.() ?? []) {
      if (member?.agent || typeof member?.user !== 'string' || !member.user) continue;
      const known = byUser.get(member.user);
      const stamp = Number(member.profileMs) || 0;
      if (!known) byUser.set(member.user, {user: member.user, label: member.label ?? '', profile_ms: stamp});
      else {
        known.profile_ms = Math.max(known.profile_ms, stamp);
        known.label ||= member.label ?? '';
      }
    }
    return [...byUser.values()];
  }

  function tick() {
    timer = undefined;
    if (stopped) return;
    try {
      sayNote();
      if (active() && visible()) {
        runtime.desktop_say_doing(mountId(), now());
        const here = people();
        photos?.seen(here);
        peer?.membersSeen?.(here);
      }
    } catch (error) {
      onError(error);
    }
    if (!stopped) timer = setTimer(tick, TICK_MS);
  }
  function wake() {
    if (stopped) return;
    if (timer !== undefined) clearTimer(timer);
    tick();
  }
  tick();

  return {
    /** Say it now, e.g. after the person acted. */
    wake,
    stop() {
      stopped = true;
      if (timer !== undefined) clearTimer(timer);
    },
  };
}

/**
 * Owns `location` and `history` for one entry page.
 *
 * The URL always says what is in front of the person, and the history stack
 * holds where they went in Tilo, so the browser's Back and Forward move
 * through Tilo rather than leaving it:
 * - push when this person opens something: a link they follow in the page
 *   (`open`), or a window they open on the desktop (a search hit, a launch);
 * - push when they move to another app's window already open, unless they
 *   left the last one within `FOCUS_SETTLE_MS`: then they were passing
 *   through it (a spun wheel, arrows held down), and the window they stop
 *   at replaces it, so Back goes where they were, not one step of the way;
 * - replace to write the canonical form of a link the page was opened with,
 *   or a deeper address within the app in front;
 * - never for anything else. Collaborators' and agents' windows open on
 *   their own strips, so they never move this person's focus or URL.
 * Back, Forward and a hand-edited fragment parse the URL and show its
 * target without writing history, once: the browser announces such a move
 * twice when the fragment changed (popstate, then hashchange), and showing
 * it again would run the target's view action a second time. The history state is only a page
 * generation key: the URL is the whole truth, so reload, Back and a pasted
 * link take the same path.
 *
 * The route is parsed and written by the runtime's Rust grammar (`link`,
 * `{parse(url), format(route)}`); this module never reads a route itself.
 * Routes here never hold secrets: the inline scrubber removed them before
 * any module ran.
 */

/** How long a window is in front before moving on from it is a step of its
 * own in the history, rather than passing through on the way elsewhere. */
export const FOCUS_SETTLE_MS = 1000;

const sameTarget = (a, b) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

/** The target a snapshot's focused window stands for, keeping `current`'s
 * deeper address while the same app stays in front. */
function focusedTarget(snapshot, current) {
  const active = snapshot?.active;
  const same = route => active && route.workspace === active.workspace && route.strip === active.strip
    && route.group === active.group && route.tab === active.tab;
  const app = snapshot?.entries?.find(entry => same(entry.route))?.app ?? null;
  const scope = current.target.scope;
  if (!app) return {scope};
  if (app === current.target.app) return current.target;
  return {scope, app};
}

export function startRouter({
  window = globalThis, link, initial, show, onError = () => {}, navigation,
  now = () => window.performance?.now?.() ?? Date.now(),
}) {
  const {history, location} = window;
  // When the window in front came there by a focus move; null when the
  // entry in front came some other way.
  let focusedAt = null;
  const generation = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  let current = initial;
  // The address this page last wrote or showed: the second announcement of
  // one move finds it there already.
  let followed = null;
  let stopped = false;
  let knownTabs = null;
  // While the router itself shows a route, the windows it opens are its
  // own doing: that history entry is written once, by the router.
  let showing = 0;
  async function quietly(route) {
    showing += 1;
    try {
      await show(route);
    } finally {
      showing -= 1;
    }
  }

  function write(route, how) {
    const url = link.format(route);
    if (how === 'push' && url !== location.href) history.pushState({tilo: generation}, '', url);
    else if (url !== location.href) history.replaceState({tilo: generation}, '', url);
    if (how !== 'replace') focusedAt = null;
    current = route;
    followed = location.href;
  }

  async function follow() {
    if (stopped || location.href === followed) return;
    followed = location.href;
    let route;
    try {
      route = link.parse(location.href);
    } catch (error) {
      onError(error);
      return;
    }
    if (route.mode !== current.mode) {
      // The other mode is another page: load it with the same fragment.
      location.reload();
      return;
    }
    current = route;
    focusedAt = null;
    try {
      await quietly(route);
    } catch (error) {
      onError(error);
    }
  }

  function onSnapshot(snapshot) {
    if (stopped) return;
    const tabs = new Set((snapshot.entries ?? []).map(entry => entry.route.tab));
    const opened = knownTabs !== null && snapshot.active && !knownTabs.has(snapshot.active.tab);
    knownTabs = tabs;
    if (showing > 0) return;
    const target = focusedTarget(snapshot, current);
    if (sameTarget(target, current.target)) return;
    const next = {mode: current.mode, target};
    if (opened) {
      write(next, 'push');
      return;
    }
    if (target.app === current.target.app) {
      // Deeper within the app in front: the same place, said more exactly.
      write(next, 'replace');
      return;
    }
    const at = now();
    const passing = focusedAt !== null && at - focusedAt < FOCUS_SETTLE_MS;
    write(next, passing ? 'replace' : 'push');
    focusedAt = at;
  }

  window.addEventListener('popstate', follow);
  window.addEventListener('hashchange', follow);
  const unsubscribe = navigation?.subscribe(onSnapshot);
  if (navigation) {
    knownTabs = new Set((navigation.getSnapshot().entries ?? []).map(entry => entry.route.tab));
  }
  // The page was opened with this link: write its canonical public form.
  write(current, 'replace');

  return {
    get route() { return current; },
    /** This person opens `route` in the page: show it, then push it. */
    async open(route) {
      await quietly(route);
      write(route, 'push');
    },
    /** Rewrite the URL without a new history entry. */
    replace(route) { write(route, 'replace'); },
    /** The same target in the other mode's page, for "Open in desktop" and "Open in its own tab". */
    url(mode) { return link.format({mode, target: current.target}); },
    stop() {
      stopped = true;
      window.removeEventListener('popstate', follow);
      window.removeEventListener('hashchange', follow);
      unsubscribe?.();
    },
  };
}

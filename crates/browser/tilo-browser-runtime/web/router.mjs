/**
 * Owns `location` and `history` for one entry page.
 *
 * The URL always says what is in front of the person, and the history stack
 * holds what they navigated to:
 * - push when this person opens something: a link they follow in the page
 *   (`open`), or a window they open on the desktop (a search hit, a launch);
 * - replace when focus moves between windows already open, and to write the
 *   canonical form of a link the page was opened with;
 * - never for anything else. Collaborators' and agents' windows open on
 *   their own strips, so they never move this person's focus or URL.
 * Back, Forward and a hand-edited fragment parse the URL and show its
 * target without writing history. The history state is only a page
 * generation key: the URL is the whole truth, so reload, Back and a pasted
 * link take the same path.
 *
 * The route is parsed and written by the runtime's Rust grammar (`link`,
 * `{parse(url), format(route)}`); this module never reads a route itself.
 * Routes here never hold secrets: the inline scrubber removed them before
 * any module ran.
 */

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
}) {
  const {history, location} = window;
  const generation = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  let current = initial;
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
    current = route;
  }

  async function follow() {
    if (stopped) return;
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
    write({mode: current.mode, target}, opened ? 'push' : 'replace');
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

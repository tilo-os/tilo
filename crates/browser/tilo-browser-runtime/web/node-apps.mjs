/**
 * The applications a node runs beside its relay, offered on the page's
 * desktop: its legacy applications (Chromium, LibreOffice, …) and the Tilo
 * applications that need a host (Terminal, …).
 *
 * The node's session, a device of the person, answers the room's
 * `workspace:<room>` catalogue with an `app.<id>/open` row per application
 * it can launch: `applications.host-app` for an unconfined legacy one,
 * `applications.app` for a Tilo package (`app` is its package id). They are
 * listed with the desktop's own (`desktop_set_node_apps`), which leaves out
 * the packages the page runs itself; opening one asks the session to launch
 * it in the room, and its window arrives as any member's legacy window does
 * (`legacy-windows.mjs`).
 *
 * - `nodeSession()`: resolves to the session's device (hex) or a falsy value.
 * - `actions(device)`: `{catalogue(query), invoke(target)}` addressed to it
 *   (`agents.actions`).
 * - `setApps([{id, title, app?}])`, `takeRequests()` → ids the person opened.
 * - `pending()`: whether the desktop has requests to take. `pump()` is the
 *   page's own frame (`host.mjs` `update`), which asks `pending()` first: a
 *   desktop that opened nothing costs one predicate, not a drained list on
 *   a timer that ran whether the tab was shown or hidden.
 */

const HOST_APP_KIND = 'applications.host-app';
const PACKAGE_KIND = 'applications.app';
const PACKAGE_PREFIX = 'app.';
const OPEN = 'open';
const MAX_PAGES = 8;
const REFRESH_MS = 30_000;

/**
 * The applications in a catalogue page's rows: `[{id, title, app?, target}]`,
 * `app` the package id of a Tilo application.
 */
export function nodeApplications(rows) {
  const apps = [];
  for (const row of rows ?? []) {
    const action = row?.action;
    const kind = action?.resource_kind;
    if ((kind !== HOST_APP_KIND && kind !== PACKAGE_KIND) || action.address?.action !== OPEN) continue;
    const id = action.address.resource;
    if (typeof id !== 'string' || !id || apps.some(app => app.id === id)) continue;
    let app;
    if (kind === PACKAGE_KIND) {
      if (!id.startsWith(PACKAGE_PREFIX) || id.length === PACKAGE_PREFIX.length) continue;
      app = id.slice(PACKAGE_PREFIX.length);
    }
    apps.push({id, title: action.resource_label || id, ...(app ? {app} : {}), target: {provider: row.provider, address: action.address}});
  }
  return apps.sort((a, b) => a.title.localeCompare(b.title));
}

/** What the desktop is told of an application. */
const listed = ({id, title, app}) => (app ? {id, title, app} : {id, title});

export function createNodeApps({
  room, nodeSession, actions, setApps, takeRequests, pending = () => true,
  setInterval: every = (callback, ms) => globalThis.setInterval(callback, ms),
  clearInterval: stopEvery = handle => globalThis.clearInterval(handle),
  onError = () => {},
}) {
  const report = error => { try { onError(error); } catch {} };
  let device;
  let apps = [];
  let closed = false;
  let refreshing;

  async function listApps(on) {
    const rows = [];
    let cursor;
    for (let page = 0; page < MAX_PAGES; page += 1) {
      const answer = await on.catalogue({provider: `workspace:${room}`, max_results: 200, ...(cursor ? {cursor} : {})});
      rows.push(...(answer?.rows ?? []));
      if (answer?.exhausted || !answer?.next_cursor?.length) break;
      cursor = answer.next_cursor;
    }
    return nodeApplications(rows);
  }

  function refresh() {
    refreshing ??= (async () => {
      try {
        device = (await nodeSession()) || undefined;
        apps = device ? await listApps(actions(device)) : [];
        if (!closed) setApps(apps.map(listed));
      } catch (error) {
        report(error);
      } finally {
        refreshing = undefined;
      }
    })();
    return refreshing;
  }

  /** Open what the person asked for, if the desktop has anything to take:
   * the page calls this from its own frame, so an idle desktop asks one
   * predicate and drains nothing. */
  function pump() {
    if (closed) return;
    let waiting;
    try { waiting = pending(); } catch (error) { report(error); return; }
    if (!waiting) return;
    let requested = [];
    try { requested = takeRequests() ?? []; } catch (error) { report(error); }
    for (const id of requested) {
      const app = apps.find(candidate => candidate.id === id);
      if (!app || !device) continue;
      actions(device).invoke(app.target).catch(report);
    }
  }

  const refreshTimer = every(() => { void refresh(); }, REFRESH_MS);
  void refresh();
  return {
    refresh,
    pump,
    /** The applications listed now. */
    apps: () => apps.map(listed),
    close() {
      if (closed) return;
      closed = true;
      stopEvery(refreshTimer);
      try { setApps([]); } catch {}
    },
  };
}

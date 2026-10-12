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
 */

const HOST_APP_KIND = 'applications.host-app';
const PACKAGE_KIND = 'applications.app';
const PACKAGE_PREFIX = 'app.';
const OPEN = 'open';
const MAX_PAGES = 8;
const REFRESH_MS = 30_000;
const REQUEST_POLL_MS = 250;

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
  room, nodeSession, actions, setApps, takeRequests,
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

  function open() {
    let requested = [];
    try { requested = takeRequests() ?? []; } catch (error) { report(error); }
    for (const id of requested) {
      const app = apps.find(candidate => candidate.id === id);
      if (!app || !device) continue;
      actions(device).invoke(app.target).catch(report);
    }
  }

  const refreshTimer = every(() => { void refresh(); }, REFRESH_MS);
  const requestTimer = every(open, REQUEST_POLL_MS);
  void refresh();
  return {
    refresh,
    /** The applications listed now. */
    apps: () => apps.map(listed),
    close() {
      if (closed) return;
      closed = true;
      stopEvery(refreshTimer);
      stopEvery(requestTimer);
      try { setApps([]); } catch {}
    },
  };
}

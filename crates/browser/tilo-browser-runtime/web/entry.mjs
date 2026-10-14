/**
 * Boot of the web entry pages: `index.html` (desktop mode) and
 * `app/index.html` (one app, full tab). The page:
 * 1. takes the secrets the inline scrubber moved out of the address bar;
 * 2. parses the link with the runtime's Rust grammar under its entry
 *    (`entry.json` beside the desktop page, written by the web host: `home`
 *    is the workspace a bare entry opens instead of the person's own desktop,
 *    `tickets` names where guest tickets are redeemed; absent, a bare entry
 *    is the person's own desktop and takes no tickets);
 * 3. gets into the room (`admission.mjs`);
 * 4. mounts the desktop or the one app, and shows the target through the
 *    view door (`open-target.mjs`);
 * 5. hands `location` and `history` to the router.
 * See docs/architecture/web-routing.md.
 */
import {admit} from './admission.mjs';
import {takeLinkSecrets} from './link-secrets.mjs';
import {openTarget} from './open-target.mjs';
import {startRouter} from './router.mjs';
import {listedWorkspaces, workspaceAt} from './workspaces.mjs';

const PRIMARY_RECORD = '@tilo:package-primary';
/** The mount's record option: `-` is the app's room document, no record
 * at all is the app's own default. */
const recordOption = record => (record === undefined ? {} : {record: record || PRIMARY_RECORD});

/** The entry this page is served under: `{base, home?, tickets?}`. */
export async function readEntry({mode, location, fetch = globalThis.fetch}) {
  const base = new URL(mode === 'app' ? '../' : './', location.href);
  base.search = '';
  base.hash = '';
  let config = {};
  try {
    const response = await fetch(new URL('entry.json', base), {cache: 'no-cache'});
    if (response.ok) config = await response.json();
  } catch {
    // No configuration: a plain bundle's entry.
  }
  const home = typeof config.home?.host?.origin === 'string' ? config.home : null;
  return {base: base.href, ...(home ? {home} : {}),
    ...(typeof config.tickets === 'string' ? {tickets: config.tickets} : {})};
}

/** `{parse, format}` over the runtime's Rust grammar under `entry`. */
export function grammar(runtime, entry) {
  // The grammar knows the entry's base; the rest is the page's.
  const json = JSON.stringify({base: entry.base});
  return {
    parse: url => JSON.parse(runtime.parseLink(url, json)),
    format: route => runtime.formatLink(JSON.stringify(route), json),
    native: route => runtime.nativeLink(JSON.stringify(route)),
  };
}

/** The route the page was opened with. A bare entry opens the web host's
 * home workspace (its room is learned on admission), or else, on the
 * desktop, the person's own desktop. */
export function initialRoute({link, location, mode, home = null}) {
  const bare = location.hash === '' || location.hash === '#';
  if (bare && home) {
    const scope = {kind: 'room', host: new URL(home.host.origin).origin, room: home.room ?? null};
    // `home.app` is the app in front: on the desktop among the workspace's
    // other apps, or full tab under an app entry. Without one, the desktop.
    const target = home.app ? {scope, app: home.app} : {scope};
    return {mode: home.app ? mode : 'desktop', target};
  }
  if (bare && mode === 'desktop') {
    try {
      return link.parse(location.href);
    } catch (error) {
      if (error?.code === 'no-route') return {mode, target: {scope: {kind: 'own'}}};
      throw error;
    }
  }
  return link.parse(location.href);
}

/** Mount what `route` shows, then show its target. */
export async function present({route, peer, surface, mountDesktop, diagnostics = {}, onStatus, onNavigation}) {
  const {target, mode} = route;
  const mount = peer ? (element, options) => peer.mountDesktop(element, options) : mountDesktop;
  let navigation;
  if (mode === 'app') {
    if (!target.app) throw new Error('This link opens a whole workspace, so it opens on the desktop');
    if (peer && target.record !== undefined) await peer.whenRecord?.(target.app, target.record);
    const mounted = await mount(surface, {...diagnostics, app: target.app, ...recordOption(target.record), onStatus});
    await openTarget({mount: mounted, target, mode});
    return {mount: mounted};
  }
  if (target.record) {
    // The desktop opens an app on its primary record; a document of its own
    // opens full tab, where the mount names the record.
    return {redirect: 'app'};
  }
  const mounted = await mount(surface, {...diagnostics, onStatus, onNavigation: controller => {
    navigation = controller;
    onNavigation?.(controller);
  }});
  navigation ??= mounted.navigation;
  await openTarget({mount: mounted, navigation, target, mode});
  return {mount: mounted, navigation};
}

/**
 * Boot an entry page. `deps` supplies the page host (`loadRuntime`,
 * `mountDesktop`) and the admission doors; the page wires the real ones.
 */
export async function boot({mode, window = globalThis, surface, status, deps, diagnostics = {}}) {
  const say = text => { if (status) status.textContent = text; };
  // The page's own loading note, which admission progress replaces only
  // while it lasts: once in, the note is back until the desktop draws.
  const loading = status?.textContent ?? '';
  const secrets = takeLinkSecrets(window);
  const runtime = await deps.loadRuntime();
  const entry = await readEntry({mode, location: window.location, fetch: deps.fetch});
  const link = grammar(runtime, entry);
  let route;
  try {
    route = initialRoute({link, location: window.location, mode, home: entry.home});
  } catch (error) {
    say(error?.message ?? String(error));
    throw error;
  }
  const workspaces = listedWorkspaces({home: entry.home, stored: deps.storedWorkspaces?.() ?? []});
  const {peer} = await admit({
    scope: route.target.scope, secrets, tickets: entry.tickets,
    deps: {...deps, workspaceAt: origin => workspaceAt(workspaces, origin)},
    onProgress: event => say(event.phase === 'pairing' ? 'Asking to pair this device…'
      : event.phase === 'waiting' ? 'Waiting for the workspace owner to let you in…'
      : event.phase === 'asking' ? 'Asking to join…' : event.phase === 'joining' ? 'Joining…' : 'Connecting…'),
  });
  say(loading);
  if (peer && route.target.scope.kind === 'room' && !route.target.scope.room) {
    // The home workspace's room is known now: show its canonical link.
    route = {...route, target: {...route.target, scope: {...route.target.scope, room: peer.room}}};
    window.history.replaceState(window.history.state, '', link.format(route));
  }
  // A listed workspace is shown under its listed name, not its room id.
  const scope = route.target.scope;
  const listed = scope.kind === 'room' ? workspaceAt(workspaces, scope.host) : null;
  const shown = await present({route, peer, surface, mountDesktop: deps.mountDesktop,
    diagnostics: {...(listed?.name ? {workspaceName: listed.name} : {}),
      // The room service's spectator count, for the desktop's "watching".
      ...(listed?.admission ? {presenceUrl: new URL('presence', listed.admission).href} : {}),
      ...diagnostics},
    onStatus: diagnostics.onStatus});
  if (shown.redirect) {
    window.location.replace(link.format({mode: shown.redirect, target: route.target}));
    return null;
  }
  const router = startRouter({window, link, initial: route, navigation: shown.navigation,
    show: next => {
      // One app full tab mounts one app and record; another is another mount.
      if (mode === 'app' && (next.target.app !== route.target.app || next.target.record !== route.target.record)) {
        window.location.reload();
        return Promise.resolve();
      }
      return openTarget({mount: shown.mount, navigation: shown.navigation, target: next.target, mode: next.mode});
    },
    onError: error => say(error?.message ?? String(error))});
  return {peer, mount: shown.mount, navigation: shown.navigation, router, link, entry};
}

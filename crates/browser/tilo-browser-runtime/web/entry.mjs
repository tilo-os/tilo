/**
 * Boot of the web entry pages: `index.html` (desktop mode) and
 * `app/index.html` (one app, full tab). The page:
 * 1. takes the secrets the inline scrubber moved out of the address bar
 *    (a machine to set up, `install`, is handed back for the page to offer);
 * 2. parses the link with the runtime's Rust grammar under its entry (a bare
 *    entry under a home workspace needs no grammar, so admission starts
 *    while the runtime still loads)
 *    (`entry.json` beside the desktop page, written by the web host: `home`
 *    is the workspace a bare entry opens instead of the person's own desktop,
 *    `tickets` names where guest tickets are redeemed; absent, a bare entry
 *    is the person's own desktop and takes no tickets; under a home, a bare
 *    desktop entry reopens the workspace the person last chose instead,
 *    `deps.chosenWorkspace`);
 * 3. gets into the room (`admission.mjs`);
 * 4. mounts the desktop or the one app, and shows the target through the
 *    view door (`open-target.mjs`);
 * 5. hands `location` and `history` to the router.
 * See docs/architecture/web-routing.md.
 */
import {admit} from './admission.mjs';
import {wireFetchProxy} from './fetch-proxy.mjs';
import {takeLinkSecrets} from './link-secrets.mjs';
import {openTarget} from './open-target.mjs';
import {startRouter} from './router.mjs';
import {listedWorkspaces, workspaceAt} from './workspaces.mjs';

const PRIMARY_RECORD = '@tilo:package-primary';
/** The mount's record option: `-` is the app's room document, no record
 * at all is the app's own default. */
const recordOption = record => (record === undefined ? {} : {record: record || PRIMARY_RECORD});

/** The web links among `about`'s `notify`, `source` and `community`, or
 * `null` without any: the desktop's About sheet offers only these. */
function aboutLinks(about) {
  const links = {};
  for (const key of ['notify', 'source', 'community']) {
    const url = about?.[key];
    if (typeof url !== 'string') continue;
    try {
      if (['https:', 'http:'].includes(new URL(url).protocol)) links[key] = url;
    } catch {
      // Not a URL: not offered.
    }
  }
  return Object.keys(links).length ? links : null;
}

/** Where a room scope is hosted (`rooms.example`), as the desktop says it
 * ("Hosted by …"); `null` on the person's own desktop. A room on the site the
 * page was opened from, or on one of its machines (`eu1.tilo.coffee` behind
 * `tilo.coffee`), is hosted by that site: the name people reached it by, not
 * the machine's. Ports are how a machine splits its services, not a name
 * anyone knows a host by, so a room on another port of the page's machine
 * (a local copy's room service beside its web entry) is that site too. */
export function scopeHost(scope, site = globalThis.location?.host) {
  if (scope.kind !== 'room') return null;
  let url;
  try {
    url = new URL(scope.host);
  } catch {
    return null;
  }
  const name = site ? hostname(site) : '';
  const host = url.hostname;
  return name && (host === name || host.endsWith(`.${name}`)) ? name : url.host;
}

/** `site`'s host name without its port (`localhost` for `localhost:8411`). */
function hostname(site) {
  try {
    return new URL(`http://${site}`).hostname;
  } catch {
    return site;
  }
}

/** `onStatus` that also titles the tab after the workspace in front. */
function titled(window, forward) {
  return status => {
    const name = status?.workspaceName;
    if (window.document && typeof name === 'string' && name) {
      const title = `Tilo · ${name}`;
      if (window.document.title !== title) window.document.title = title;
    }
    forward?.(status);
  };
}

/** The entry this page is served under: `{base, home?, tickets?, about?,
 * embedder?}`; `about` holds the web host's links for the desktop's About
 * sheet, `embedder` the origin of the site that embeds the page, if any. */
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
  const about = aboutLinks(config.about);
  const embedder = embedderOrigin(config.embedder);
  return {base: base.href, ...(home ? {home} : {}),
    ...(typeof config.tickets === 'string' ? {tickets: config.tickets} : {}),
    ...(about ? {about} : {}), ...(embedder ? {embedder} : {})};
}

/** Send problem reports from the home workspace to the channel its
 * entry names. The desktop does not wait for it: the owner's admission is
 * a round trip of its own, and the desktop takes reports up once it is in. */
export function configureFeedback(peer, home) {
  const channel = home?.feedback?.channel;
  if (!peer?.room || typeof channel !== 'string' || typeof peer.configureFeedback !== 'function') return;
  peer.configureFeedback({room: peer.room, provider: 'record:so.tilo.chat:', channel})
    .catch(error => console.warn('feedback:', error?.message ?? error));
}

/** The embedding site's origin, when `value` is exactly an https origin. */
function embedderOrigin(value) {
  if (typeof value !== 'string') return null;
  try {
    const url = new URL(value);
    return url.protocol === 'https:' && url.origin === value ? value : null;
  } catch {
    return null;
  }
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

/** A bare entry under a web host's home workspace: its route, read without
 * the runtime's grammar; `null` for any other link. */
export function homeRoute({location, mode, home = null}) {
  const bare = location.hash === '' || location.hash === '#';
  if (!bare || !home) return null;
  const scope = {kind: 'room', host: new URL(home.host.origin).origin, room: home.room ?? null};
  // `home.app` is the app in front: on the desktop among the workspace's
  // other apps, or full tab under an app entry. Without one, the desktop.
  const target = home.app ? {scope, app: home.app} : {scope};
  return {mode: home.app ? mode : 'desktop', target};
}

/** A bare desktop entry under a web host's home workspace, when the person
 * last chose another of their workspaces (`chosen`, `{origin, room}`):
 * that room's route; `null` otherwise, and always in an embed. */
export function chosenRoute({location, mode, home = null, chosen = null}) {
  const bare = location.hash === '' || location.hash === '#';
  if (!bare || !home || mode !== 'desktop' || !chosen?.room) return null;
  return {mode, target: {scope: {kind: 'room', host: new URL(chosen.origin).origin, room: chosen.room}}};
}

/** Whether `a` and `b` name the same room (or both the person's own desktop). */
function sameScope(a, b) {
  if (a?.kind !== b?.kind) return false;
  if (a.kind !== 'room') return true;
  const origin = host => { try { return new URL(host).origin; } catch { return host; } };
  return origin(a.host) === origin(b.host) && (a.room ?? null) === (b.room ?? null);
}

/** The route the page was opened with. A bare entry opens the web host's
 * home workspace (its room is learned on admission), or else, on the
 * desktop, the person's own desktop. */
export function initialRoute({link, location, mode, home = null}) {
  const toHome = homeRoute({location, mode, home});
  if (toHome) return toHome;
  const bare = location.hash === '' || location.hash === '#';
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
 * The desktop link that says what is in front of the person: the app's
 * window, and the view of its resource a link opened (`{scope, app,
 * record?, view?}`). Secrets left the address before any module ran.
 */
export function shownLink(target) {
  const {scope, app, record, view} = target ?? {};
  if (!scope || app === undefined || app === null) return {mode: 'desktop', target: {scope}};
  return {mode: 'desktop', target: {scope, app, ...(record ? {record} : {}), ...(view ? {view} : {})}};
}
/**
 * Boot an entry page. `deps` supplies the page host (`loadRuntime`,
 * `mountDesktop`) and the admission doors; the page wires the real ones.
 * `onStep` hears the steps of getting in: `join` while a host's door is
 * asked, `open` once admitted (until the desktop draws).
 */
export async function boot({mode, window = globalThis, surface, status, deps, diagnostics = {}, onStep = () => {}}) {
  const say = text => { if (status) status.textContent = text; };
  // The page's own loading note, which admission progress replaces only
  // while it lasts: once in, the note is back until the desktop draws.
  const loading = status?.textContent ?? '';
  // A machine to set up is not admission: the page offers it once open.
  // Nor is a Discord account to link: the desktop offers it once open.
  const {install, link: linkOffer, ...secrets} = takeLinkSecrets(window);
  // The runtime (most of the page's download) loads while the entry is read
  // and, for a bare entry under a home workspace, while admission starts:
  // a host's door is asked, and the person and device keys opened, before
  // anything needs the runtime. Each door waits for it where it does.
  const loaded = deps.loadRuntime();
  const entry = await readEntry({mode, location: window.location, fetch: deps.fetch});
  const linked = loaded.then(runtime => grammar(runtime, entry));
  // Awaited below; a failure is the first door's to report.
  void linked.catch(() => {});
  let route;
  try {
    route = chosenRoute({location: window.location, mode, home: entry.home, chosen: deps.chosenWorkspace?.() ?? null})
      ?? homeRoute({location: window.location, mode, home: entry.home})
      ?? initialRoute({link: await linked, location: window.location, mode, home: entry.home});
  } catch (error) {
    say(error?.message ?? String(error));
    throw error;
  }
  const workspaces = listedWorkspaces({home: entry.home, stored: deps.storedWorkspaces?.() ?? []});
  // The page hears which room it is getting into, under which entry, before admission.
  deps.onScope?.({scope: route.target.scope, entry});
  const {peer} = await admit({
    scope: route.target.scope, secrets, tickets: entry.tickets,
    home: entry.home ? workspaceAt(workspaces, entry.home.host?.origin) ?? entry.home : null,
    deps: {...deps, workspaceAt: origin => workspaceAt(workspaces, origin)},
    onProgress: event => {
      onStep('join');
      say(event.phase === 'pairing' ? 'Asking to pair this device…'
        : event.phase === 'waiting' ? 'Waiting for someone in the workspace to let you in…'
        : event.phase === 'asking' ? 'Asking to join…' : event.phase === 'joining' ? 'Joining…' : 'Connecting…');
    },
  });
  if (peer) deps.onAdmitted?.(peer);
  configureFeedback(peer, entry.home);
  // The app in front (a home workspace's, or the link's) downloads once the
  // page is in, while the desktop mounts, not when the desktop first opens
  // it. Not before: admission is a run of small round trips to the host,
  // and megabytes of app code on the same link held each of them up, so
  // the desktop drew only once the app had arrived. Its window holds the
  // app's place until then.
  if (route.target.app) deps.prefetchApplication?.(route.target.app);
  // The apps in the page read sites without CORS headers through the
  // host it got into, when that host serves a fetch proxy.
  wireFetchProxy(peer, window);
  say(loading);
  onStep('open');
  const link = await linked;
  if (peer && route.target.scope.kind === 'room' && !route.target.scope.room) {
    // The home workspace's room is known now: show its canonical link.
    route = {...route, target: {...route.target, scope: {...route.target.scope, room: peer.room}}};
    window.history.replaceState(window.history.state, '', link.format(route));
  }
  // A listed workspace is shown under its listed name, not its room id.
  const scope = route.target.scope;
  const listed = scope.kind === 'room' ? workspaceAt(workspaces, scope.host) : null;
  // The desktop offers Invite in a room the page can invite to.
  const invite = peer?.room && deps.onInvite ? {onInvite: deps.onInvite} : {};
  const host = scopeHost(scope, window.location?.host);
  const shown = await present({route, peer, surface, mountDesktop: deps.mountDesktop,
    diagnostics: {...(listed?.name ? {workspaceName: listed.name} : {}), ...(host ? {host} : {}),
      ...(entry.about ? {about: entry.about} : {}),
      ...(linkOffer ? {linkOffer} : {}),
      // The room service's spectator count, for the desktop's "watching".
      ...(listed?.admission ? {presenceUrl: new URL('presence', listed.admission).href} : {}),
      ...invite, ...diagnostics},
    onStatus: titled(window, diagnostics.onStatus)});
  if (shown.redirect) {
    window.location.replace(link.format({mode: shown.redirect, target: route.target}));
    return null;
  }
  // `show` reads the router it was given, so the address can name what a
  // link opened once it is shown.
  let router;
  router = startRouter({window, link, initial: route, navigation: shown.navigation,
    show: next => {
      // Another room is another peer and another desktop (Back or Forward
      // across a switch of workspace, a hand-edited fragment): load it anew,
      // never inside the mount of the room in front.
      if (!sameScope(next.target.scope, route.target.scope)) {
        window.location.reload();
        return Promise.resolve();
      }
      // One app full tab mounts one app and record; another is another mount.
      if (mode === 'app' && (next.target.app !== route.target.app || next.target.record !== route.target.record)) {
        window.location.reload();
        return Promise.resolve();
      }
      return openTarget({mount: shown.mount, navigation: shown.navigation, target: next.target, mode: next.mode})
        .then(() => {
          // The resource a link opened is in front now: the address names it,
          // as a link to that view would, until focus moves to another
          // window (the router then writes that window instead).
          if (!next.target.resource) return;
          router.replace(shownLink({...next.target, view: next.target.view ?? null}));
        });
    },
    onError: error => say(error?.message ?? String(error))});
  return {peer, mount: shown.mount, navigation: shown.navigation, router, link, entry, ...(install ? {install} : {})};
}

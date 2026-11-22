/**
 * Boot of the web entry pages: `index.html` and `app/index.html`. The page:
 * 1. takes the secrets the inline scrubber moved out of the address bar
 *    (a machine to set up, `install`, is handed back for the page to offer);
 * 2. parses the link with the runtime's Rust grammar under its entry (a bare
 *    entry under a home workspace needs no grammar, so admission starts
 *    while the runtime still loads)
 *    (`entry.json` beside the page, written by the web host: `home` is the
 *    workspace a bare entry opens, `tickets` names where guest tickets are
 *    redeemed; under a home, a bare entry reopens the workspace the person
 *    last chose instead, `deps.chosenWorkspace`);
 * 3. gets into the workspace (`admission.mjs`), which makes this browser a
 *    device of it;
 * 4. hands the browser to the page the server renders of that workspace
 *    (`/w/<workspace>/<app>`), unless a first visit's first screen holds it
 *    until its setup is done (`deps.holdFirstScreen`).
 */
import {admit} from './admission.mjs';
import {takeLinkSecrets} from './link-secrets.mjs';
import {nameSiteServer} from './site-host.mjs';
import {listedWorkspaces, workspaceAt} from './workspaces.mjs';

/** The entry this page is served under: `{base, home?, tickets?,
 * embedder?}`; `embedder` is the origin of the site that embeds the page, if
 * any. */
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
  const embedder = embedderOrigin(config.embedder);
  return {base: base.href, ...(home ? {home} : {}),
    ...(typeof config.tickets === 'string' ? {tickets: config.tickets} : {}),
    ...(embedder ? {embedder} : {})};
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
    /** The link to `route` that lets anyone who has it in with `invitation`. */
    invite: (route, invitation) => runtime.formatInviteLink(JSON.stringify(route), json, invitation),
    native: route => runtime.nativeLink(JSON.stringify(route)),
  };
}

/** Whether `location` is a bare entry: one whose fragment names no route. */
export const bareEntry = location => ['', '#', '#/'].includes(location.hash);

/** A bare entry's address (`bareEntry`) as the grammar reads one:
 * without its empty fragment, since `#/` alone reads there as a route with an
 * empty part. */
const bareHref = location => location.href.replace(/#\/?$/, '');

/** A bare entry under a web host's home workspace: its route, read without
 * the runtime's grammar; `null` for any other link. */
export function homeRoute({location, mode, home = null}) {
  const bare = bareEntry(location);
  if (!bare || !home) return null;
  const scope = {kind: 'workspace', host: new URL(home.host.origin).origin, workspace: home.workspace ?? null};
  // `home.app` is the app in front: on the desktop among the workspace's
  // other apps, or full tab under an app entry. Without one, the desktop.
  const target = home.app ? {scope, app: home.app} : {scope};
  return {mode: home.app ? mode : 'desktop', target};
}

/** A bare desktop entry under a web host's home workspace, when the person
 * last chose another of their workspaces (`chosen`, `{origin, workspace}`):
 * that workspace's route; `null` otherwise, and always in an embed. */
export function chosenRoute({location, mode, home = null, chosen = null}) {
  const bare = bareEntry(location);
  if (!bare || !home || mode !== 'desktop' || !chosen?.workspace) return null;
  return {mode, target: {scope: {kind: 'workspace', host: new URL(chosen.origin).origin, workspace: chosen.workspace}}};
}

/** `route` with a workspace its link names no server for at the server the site names
 * (`entry.home`): the link grammar reads a link without one as the page's own site, and a site
 * that names its home workspace's server at another origin keeps no workspace itself. A link
 * carrying an invitation, a ticket or a pairing is left as it reads: what it carries names its
 * server. */
export function atSiteServer(route, entry, secrets = {}) {
  const scope = route?.target?.scope;
  const home = originOf(entry?.home?.host?.origin);
  const site = originOf(entry?.base);
  if (scope?.kind !== 'workspace' || !home || !site || home === site || originOf(scope.host) !== site) return route;
  if (secrets.invite || secrets.ticket || secrets.pair) return route;
  return {...route, target: {...route.target, scope: {...scope, host: home}}};
}

/** `url`'s origin, or `null` for no URL. */
function originOf(url) {
  try {
    return new URL(url).origin;
  } catch {
    return null;
  }
}

/** The route the page was opened with. A bare entry opens the web host's
 * home workspace (its workspace is learned on admission), or else, on the
 * desktop, the person's own desktop. */
export function initialRoute({link, location, mode, home = null}) {
  const toHome = homeRoute({location, mode, home});
  if (toHome) return toHome;
  const bare = bareEntry(location);
  if (bare && mode === 'desktop') {
    try {
      return link.parse(bareHref(location));
    } catch (error) {
      if (error?.code === 'no-route') return {mode, target: {scope: {kind: 'own'}}};
      throw error;
    }
  }
  return link.parse(bare ? bareHref(location) : location.href);
}

/**
 * Boot an entry page. `deps` supplies the runtime (`loadRuntime`) and the
 * admission entries; the page wires the real ones. `onStep` hears the steps
 * of getting in: `join` while a host's entry is asked, `open` once admitted.
 * Resolves to `null` once the page the server renders took over (or, in an
 * embed, while nothing joined yet).
 */
export async function boot({mode, window = globalThis, status, deps, onStep = () => {}}) {
  const say = text => { if (status) status.textContent = text; };
  // The page's own loading note, which admission progress replaces only
  // while it lasts.
  const loading = status?.textContent ?? '';
  // A machine to set up, or an account to link, is not admission.
  const {install: _install, link: _link, ...secrets} = takeLinkSecrets(window);
  // The runtime (most of the page's download) loads while the entry is read
  // and the route chosen; admission waits for it, as the way in is its to say.
  const loaded = deps.loadRuntime();
  const entry = await readEntry({mode, location: window.location, fetch: deps.fetch});
  // The server the site names with its home workspace: the page dials it as named.
  nameSiteServer(entry.home);
  const linked = loaded.then(runtime => grammar(runtime, entry));
  // Awaited below; a failure is the first entry's to report.
  void linked.catch(() => {});
  let route;
  try {
    route = chosenRoute({location: window.location, mode, home: entry.home, chosen: deps.chosenWorkspace?.() ?? null})
        ?? homeRoute({location: window.location, mode, home: entry.home})
        ?? atSiteServer(initialRoute({link: await linked, location: window.location, mode, home: entry.home}), entry, secrets);
  } catch (error) {
    say(error?.message ?? String(error));
    throw error;
  }
  // A link whose workspace reads but whose document (or item, or view) is
  // damaged names that workspace: it opens without the damaged part.
  const {damaged: _damaged, ...named} = route;
  route = named;
  const workspaces = listedWorkspaces({home: entry.home, stored: deps.storedWorkspaces?.() ?? []});
  // The page hears which workspace it is getting into, under which entry,
  // and how its links read once the runtime is in, before admission.
  deps.onScope?.({scope: route.target.scope, entry, link: linked});
  const {peer, idle, own: ownFirst = false} = await admit({
    scope: route.target.scope, deep: route.target.app != null, secrets, tickets: entry.tickets, runtime: await loaded,
    home: entry.home ? workspaceAt(workspaces, entry.home.host?.origin) ?? entry.home : null,
    deps: {...deps, workspaceAt: origin => workspaceAt(workspaces, origin)},
    onProgress: event => {
      onStep('join');
      say(event.phase === 'pairing' ? 'Asking to pair this device…'
          : event.phase === 'asking' ? 'Asking to join…' : event.phase === 'joining' ? 'Joining…' : 'Connecting…');
    },
  });
  // Nothing to mount: an embed shows its site's fallback until the visitor
  // does something (`entry-page.mjs`).
  if (idle) return null;
  // The page hears it was let in before anything is handed over; it may ask
  // the person something first (the 24 recovery words, shown once), so its
  // answer is awaited.
  if (peer) await deps.onAdmitted?.(peer);
  // A wait-list invitation that opened the person's own workspace in place of
  // the one its link names: from here on the page is in that one, as a link
  // to it says.
  const ownAdmitted = Boolean(ownFirst && peer);
  if (ownAdmitted) {
    route = {mode: 'desktop', target: {scope: {kind: 'workspace', host: new URL(peer.origin).origin, workspace: peer.workspace}}};
  }
  // An invitation whose server only it names (one reached over iroh alone) admits at that
  // server: the link from here on names it, so the next visit comes back to it.
  const admittedElsewhere = !ownAdmitted && Boolean(peer?.origin) && route.target.scope.kind === 'workspace'
    && new URL(peer.origin).origin !== route.target.scope.host;
  if (admittedElsewhere) {
    route = {...route, target: {...route.target, scope: {...route.target.scope, host: new URL(peer.origin).origin}}};
  }
  say(loading);
  onStep('open');
  // A link that named no workspace is in the one its server's entry admitted to.
  if (peer && route.target.scope.kind === 'workspace' && (ownAdmitted || admittedElsewhere || !route.target.scope.workspace)) {
    route = {...route, target: {...route.target, scope: {...route.target.scope, workspace: peer.workspace}}};
  }
  // The web version is the page the server renders of a workspace: this
  // browser is a device of it now, and the page takes over from here. What
  // the link named of an app is the page it opens, and the thing it named
  // there is what that page opens first; a link naming none lands on the
  // workspace's front page, which lists its apps.
  if (peer && route.target.scope.kind === 'workspace' && route.target.scope.workspace) {
    deps.onPageFirst?.(peer);
    const app = route.target.app ? `/${String(route.target.app).split('.').pop()}` : '';
    const resource = typeof route.target.resource === 'string'
      ? route.target.resource : route.target.resource?.id;
    const show = app && resource ? `/${encodeURIComponent(resource)}` : '';
    const url = `${entry.base}w/${route.target.scope.workspace}${app}${show}`;
    // A first visit sets itself up before it is in a workspace of its own:
    // the entry keeps the browser on its first screen, holding the page it
    // would have handed over, until the person is done or skips
    // (`deps.holdFirstScreen`).
    if (deps.holdFirstScreen?.({url, workspace: route.target.scope.workspace, entry, peer})) return null;
    window.location.replace(url);
    return null;
  }
  // A link that opens no workspace (the person's own desktop, a peer that
  // names none) has no page to hand over to.
  throw new Error('This link opens no workspace. Open one of your workspaces to use Tilo on the web.');
}

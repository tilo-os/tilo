/**
 * The fetch proxy the page's apps read cross-origin sites through
 * (tilo-net's `proxy`, wire protocol v1; docs/architecture/network.md). The
 * workspace host the page is admitted to serves it at `/peer/v1/fetch` to
 * its members' sessions, and says so when the session starts.
 *
 * `globalThis.__tiloFetchProxy` names it, for tilo-net in the runtime and
 * for the page's image loader:
 * - a string is the endpoint of a host on the page's own origin that needs
 *   no session (the development server, `serve.mjs --fetch-proxy`);
 * - a function answers `{endpoint, session}` for each request, or nothing
 *   while there is no proxy: the session follows the page's reconnects.
 */

/** Where a workspace host serves the proxy. */
export const FETCH_PROXY_PATH = '/peer/v1/fetch';

/** Name `peer`'s host as the page's fetch proxy, on `target` (the page's
 * global object). A page whose server already named one keeps it. */
export function wireFetchProxy(peer, target = globalThis) {
  if (typeof target.__tiloFetchProxy === 'string') return;
  if (typeof peer?.fetchProxy !== 'function') return;
  target.__tiloFetchProxy = () => peer.fetchProxy();
}

/** The proxy a request to `url` (a `URL`) from the page at `pageHref` goes
 * through: `{endpoint, session?}` with an absolute endpoint, or `undefined`
 * for a same-origin request or a page without a proxy. */
export function fetchProxyFor(url, {target = globalThis, pageHref = globalThis.location?.href} = {}) {
  const page = new URL(pageHref);
  if (url.origin === page.origin) return undefined;
  const named = target.__tiloFetchProxy;
  let proxy;
  if (typeof named === 'string') {
    proxy = {endpoint: named};
  } else if (typeof named === 'function') {
    try {
      proxy = named() ?? undefined;
    } catch {
      proxy = undefined;
    }
  }
  if (typeof proxy?.endpoint !== 'string') return undefined;
  let endpoint;
  try {
    endpoint = new URL(proxy.endpoint, page);
  } catch {
    return undefined;
  }
  if (!['http:', 'https:'].includes(endpoint.protocol)) return undefined;
  return {endpoint: endpoint.href, ...(typeof proxy.session === 'string' ? {session: proxy.session} : {})};
}

/** What a peer request's `url` query parameter says ([`tilo_net::proxy::PEER_PARAM`]): the target is not a site on the web but a path on the machine that admitted the page's person. */
export const PEER_PARAM = 'tilo-peer';
/** What [`PEER_PARAM`] says. */
export const PEER_LOCAL = 'local';

/** `fetch` arguments that read `path` — a path on the machine that admitted
 * the page's person, `/home/ada/Notes.md` — through `proxy`. The host reads
 * it as the file it is on its own disk, so an app in a page sees the same
 * files its person sees in the desktop session. */
export function peerGet(path, proxy, {signal} = {}) {
  const endpoint = new URL(proxy.endpoint, globalThis.location?.href);
  endpoint.searchParams.set('url', `file://${path}`);
  endpoint.searchParams.set(PEER_PARAM, PEER_LOCAL);
  const headers = {'x-tilo-proxy': '1'};
  if (proxy.session) headers['x-tilo-session'] = proxy.session;
  return [endpoint.href, {signal, credentials: 'same-origin', headers}];
}

/** `fetch` arguments that send a GET of `url` through `proxy` (from
 * `fetchProxyFor`): the page's cookies never leave its own origin, and
 * the session rides in its header. */
export function proxiedGet(url, proxy, {signal} = {}) {
  const endpoint = new URL(proxy.endpoint);
  endpoint.searchParams.set('url', url.href);
  const headers = {'x-tilo-proxy': '1'};
  if (proxy.session) headers['x-tilo-session'] = proxy.session;
  return [endpoint.href, {signal, credentials: 'same-origin', headers}];
}

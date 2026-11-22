/**
 * The host this page's own site names (`entry.json` `home.host.info`, the web
 * entry's `TILO_WEB_HOME_HOST_INFO`): its certificate and where it is dialled,
 * as the runtime's `hostInfo()` answers them. Before it is in, the page trusts
 * nothing about a host but what its own origin serves it, and the site serves
 * this with the page, so it is as trustworthy as the page itself: the page
 * dials that host as named, without asking it who it is first.
 *
 * A host set up again (a new certificate or endpoint) answers as someone
 * else, and the site names it as it was until its deployment is updated. The
 * page never goes on with another host than the one named: a session that
 * cannot reach the named host asks the host who it is, only to say so
 * (`staleSiteServer`), and stops with `STALE_SITE_SERVER`.
 */

/** What a person is told when the site names its host as it was before it was set up again. */
export const STALE_SITE_SERVER = 'This site is out of date with its workspace’s host, so it cannot reach it. Please try again later.';

/** The hosts the site named, by origin: `{text, info}`, the JSON text as served and parsed. */
const named = new Map();

const originOf = origin => {
  try {
    return new URL(origin).origin;
  } catch {
    return null;
  }
};

/** Take the host `home` (`entry.json` `home`) names, when it names one. Throws for one
 * the page cannot read: the site's deployment is wrong, and the page says so rather than
 * ask the host. */
export function nameSiteServer(home) {
  const text = home?.host?.info;
  if (text === undefined) return;
  const origin = originOf(home.host.origin);
  let info;
  try {
    info = JSON.parse(text);
  } catch {
    info = null;
  }
  if (!origin || typeof text !== 'string' || !info?.hostCert || !info.address?.endpointAddr) {
    throw new Error('This site names its workspace’s host in a form this page cannot read (entry.json home.host.info, '
      + 'the web entry’s TILO_WEB_HOME_HOST_INFO: the host’s `web-host.json`).');
  }
  named.set(origin, {text, info});
}

/** Where this browser keeps the servers invitations named ([`nameInvitedServer`]), by origin. */
const INVITED_SERVERS = 'tilo-invited-servers';

/** This browser's storage, or `null` where it holds none (a sandboxed frame throws on reading it). */
const storage = () => {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
};

/** The servers invitations named, as this browser keeps them: `{origin: text}`. */
function keptInvitedServers(store) {
  try {
    const kept = JSON.parse(store?.getItem(INVITED_SERVERS) ?? '{}');
    return kept && typeof kept === 'object' ? kept : {};
  } catch {
    return {};
  }
}

/** The origin a page names a server by that an invitation alone names: the server's endpoint
 * id under a name that never resolves (`.invalid`), so nothing is ever fetched from it and the
 * page reaches it only as the invitation says (`nameInvitedServer`). */
export function invitedOrigin(endpoint) {
  const id = String(endpoint ?? '').toLowerCase().replace(/[^a-z0-9]/g, '').slice(0, 32);
  return id ? `https://${id}.iroh.invalid` : null;
}

/** Whether `origin` is one `invitedOrigin` made: a server with no web origin of its own. */
export function isInvitedServer(origin) {
  return /\.iroh\.invalid$/.test(originOf(origin) ?? '');
}

/**
 * Name the server an invitation names (`info`, as its `/host` answers) at `origin`
 * (`invitedOrigin`): a server reached only over iroh, behind a NAT and through a packet relay,
 * has no web origin a page could ask who it is, and its own signed address in the invitation is
 * what the page dials, as it dials a server its site names. Kept in `store` too, so the next visit
 * reaches it the same way.
 */
export function nameInvitedServer(origin, info, store = storage()) {
  const at = originOf(origin);
  if (!at || !info?.hostCert || !info.address?.endpointAddr) return;
  const text = JSON.stringify(info);
  named.set(at, {text, info});
  try {
    store?.setItem(INVITED_SERVERS, JSON.stringify({...keptInvitedServers(store), [at]: text}));
  } catch {
    // A browser that keeps nothing reaches it this visit, and asks for a new link next time.
  }
}

/** The server the site named at `origin`, or one an invitation named there:
 * `{text, info}`, else `null`. */
export function siteServer(origin, store = storage()) {
  const at = originOf(origin);
  const known = named.get(at);
  if (known || !at) return known ?? null;
  const text = keptInvitedServers(store)[at];
  if (typeof text !== 'string') return null;
  try {
    const info = JSON.parse(text);
    named.set(at, {text, info});
    return {text, info};
  } catch {
    return null;
  }
}

/** The origin the site names the server `info` at (`info` as the server answers who it is, or
 * as an invitation names it), or `null` for a server the site does not name: one only an
 * invitation named is not the site's, wherever this browser keeps it. A server is who its
 * certificate says, not where it is dialled: the same server names itself by its endpoint in an
 * invitation and is named by its web origin on its site. */
export function siteNames(info) {
  const device = JSON.stringify(info?.hostCert?.device_public_key ?? null);
  if (device === 'null') return null;
  for (const [origin, kept] of named.entries()) {
    if (!isInvitedServer(origin) && JSON.stringify(kept.info?.hostCert?.device_public_key ?? null) === device) return origin;
  }
  return null;
}

/** Forget every host the site named (tests). */
export function forgetSiteServers() {
  named.clear();
}

/** Who a host is and where it is dialled: its root and device, its endpoint, its packet relay,
 * and its WebTransport listener with the certificate a page pins there. */
const identity = info => JSON.stringify([
  info?.hostCert?.user_public_key ?? null, info?.hostCert?.device_public_key ?? null,
  info?.address?.endpointAddr?.id ?? null, info?.address?.relayUrl ?? null,
  info?.address?.web?.origin ?? null, info?.address?.web?.certificate_sha256 ?? null,
]);

/** Whether the host answering at its origin (`answering`, as `hostInfo()` reads it) is
 * another than the one the site named (`info`): set up again since the site's deployment
 * named it, reached through another packet relay, or listening for pages elsewhere or under
 * a renewed pinned certificate. */
export function staleSiteServer(info, answering) {
  return identity(info) !== identity(answering);
}

/** What the site's operator is told (the page's console, and the error log of the site's
 * entry) when the site names `origin`'s host as it was before it was set up again. */
export function staleSiteServerReport(origin) {
  return `entry.json names the host at ${origin} as it was before it was set up again, and the host answers as `
    + `another now. Set the web entry's TILO_WEB_HOME_HOST_INFO to the host's web-host.json.`;
}

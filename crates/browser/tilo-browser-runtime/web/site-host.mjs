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
 * (`staleSiteHost`), and stops with `STALE_SITE_HOST`.
 */

/** What a person is told when the site names its host as it was before it was set up again. */
export const STALE_SITE_HOST = 'This site is out of date with its workspace’s host, so it cannot reach it. Please try again later.';

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
export function nameSiteHost(home) {
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
      + 'the web entry’s TILO_WEB_HOME_HOST_INFO: what `tilo-relay page-host <host>` prints).');
  }
  named.set(origin, {text, info});
}

/** The host the site named at `origin`: `{text, info}`, else `null`. */
export function siteHost(origin) {
  return named.get(originOf(origin)) ?? null;
}

/** Forget every host the site named (tests). */
export function forgetSiteHosts() {
  named.clear();
}

/** Who a host is and where it is dialled: its root and device, its endpoint and its packet relay. */
const identity = info => JSON.stringify([
  info?.hostCert?.user_public_key ?? null, info?.hostCert?.device_public_key ?? null,
  info?.address?.endpointAddr?.id ?? null, info?.address?.relayUrl ?? null,
]);

/** Whether the host answering at its origin (`answering`, as `hostInfo()` reads it) is
 * another than the one the site named (`info`): set up again since the site's deployment
 * named it, or reached through another packet relay. */
export function staleSiteHost(info, answering) {
  return identity(info) !== identity(answering);
}

/** What the site's operator is told (the page's console, and the error log of the site's
 * door) when the site names `origin`'s host as it was before it was set up again. */
export function staleSiteHostReport(origin) {
  return `entry.json names the host at ${origin} as it was before it was set up again, and the host answers as `
    + `another now. Set the web entry's TILO_WEB_HOME_HOST_INFO to what \`tilo-relay page-host ${origin}\` prints.`;
}

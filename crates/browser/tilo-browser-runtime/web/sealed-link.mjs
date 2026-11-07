/**
 * The address a sealed invitation's link opens, as the desk hands it over and
 * a page takes it: `https://<site>/i/<handle>#<key>`
 * (crates/collab/workspace-join/src/sealed.rs). The site keeps the invitation
 * under the handle, sealed; the key that opens it travels after the `#`, which
 * a browser never sends to a server, and the scrubber took it out of the
 * address bar before any module ran (`scrub.inline.js`, `link-secrets.mjs`).
 * A page that opens such a link asks the site for the invitation and its host
 * for the workspace it admits into, which the page learns on the way in.
 */

/** Where a site serves a sealed invitation. */
const ROUTE = '/i/';
/** A handle as the route spells it (16 characters of base64url). */
const HANDLE = /^[A-Za-z0-9_-]{16}$/;
/** The key that opens a sealed invitation, as the fragment spells it. */
const KEY = /^[A-Za-z0-9_-]{43}$/;

/** The handle `pathname` names, below a site's `/i/`; `null` elsewhere. */
export function sealedHandle(pathname) {
  if (typeof pathname !== 'string' || !pathname.startsWith(ROUTE)) return null;
  const handle = pathname.slice(ROUTE.length);
  return HANDLE.test(handle) ? handle : null;
}

/** The sealed invitation's link `location` is, with the `secrets` the
 * scrubber took out of it (`{site, handle, key}`), or `null` for a page
 * opened at any other address. `site` is the site the page is served from:
 * the one that keeps the invitation. */
export function sealedLink({location, secrets = {}}) {
  const handle = sealedHandle(location?.pathname);
  const key = secrets.invite;
  if (!handle || typeof key !== 'string' || !KEY.test(key)) return null;
  return {site: new URL('/', location.href).origin, handle, key};
}

/**
 * A page that outlived its bundle. The web entry serves one version of the
 * bundle at a time (`<entry>v/<version>/…`, container/layout.py), and a page
 * fetches some of it only when first needed: an app's module when its window
 * first opens (`pkg/__wasm_split.js`). Once a newer version is deployed, a
 * page opened before keeps running the old one, and its next such fetch
 * finds nothing there. Never loading the new version's files into the old
 * page (each page runs exactly one version), this tells the page that it
 * was left behind, so the person can reload into the new version: the
 * desktop and its records are kept in the profile's storage, so a reload
 * reopens the same windows on the same documents, and the window that did
 * not open now opens.
 *
 * A loader that could not fetch a file says so with an `UNAVAILABLE` event
 * on the page's global (`{detail: {url}}`); `watchForUpdate` asks the entry
 * for the version it serves now (`<entry>version.json`, revalidated) and
 * offers the reload when it is another one. A failure under the version the
 * entry still serves (the network, a broken file) is not an update and is
 * left to the loader's caller.
 */

/** The event a loader dispatches on the page's global when it could not fetch a bundle file. */
export const UNAVAILABLE = 'tilo-module-unavailable';

const VERSIONED = /^(.*?\/)v\/([0-9a-f]{20})\//;

/** The entry (`base`, ending in `/`) and content version a bundle file's URL is
 * served under, or `null` for a bundle not laid out in versions (a
 * development bundle). */
export function servedVersion(url) {
  let href;
  try { href = new URL(url).href; } catch { return null; }
  const match = VERSIONED.exec(href);
  return match ? {base: match[1], version: match[2]} : null;
}

/** The version the entry at `base` serves now, or `null` when it cannot be
 * read (offline, or an entry without `version.json`). */
export async function deployedVersion(base, {fetch = globalThis.fetch} = {}) {
  try {
    const response = await fetch(new URL('version.json', base), {cache: 'no-store'});
    if (!response.ok) return null;
    const {version} = await response.json();
    return typeof version === 'string' && /^[0-9a-f]{20}$/.test(version) ? version : null;
  } catch {
    return null;
  }
}

/**
 * Listen on `target` for loaders that could not fetch a file, and when the
 * entry now serves another version than the file's, call `offer({running,
 * deployed})` (the person's answer may take as long as it likes; one offer
 * is open at a time, and another failure after it closes offers again).
 * Resolves each check through `onChecked` for tests and tools. Returns a
 * function that stops listening.
 */
export function watchForUpdate({target = globalThis, fetch = globalThis.fetch, offer, onChecked = () => {}}) {
  let busy = false;
  const heard = async event => {
    const served = servedVersion(event?.detail?.url);
    if (!served || busy) return onChecked(null);
    busy = true;
    try {
      const deployed = await deployedVersion(served.base, {fetch});
      const update = deployed && deployed !== served.version ? {running: served.version, deployed} : null;
      onChecked(update);
      if (update) await offer(update);
    } finally {
      busy = false;
    }
  };
  const listener = event => { void heard(event).catch(error => console.warn('update check:', error)); };
  target.addEventListener(UNAVAILABLE, listener);
  return () => target.removeEventListener(UNAVAILABLE, listener);
}

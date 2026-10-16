/**
 * The secrets the entry page's inline scrubber (`scrub.inline.js`) moved
 * out of the address bar. Admission takes them once; after that the page
 * holds them nowhere, so nothing later (a share-link, a crash report) can
 * pick them up.
 */
export function takeLinkSecrets(scope = globalThis) {
  const secrets = scope.__tiloLinkSecrets ?? {};
  delete scope.__tiloLinkSecrets;
  return {...secrets};
}

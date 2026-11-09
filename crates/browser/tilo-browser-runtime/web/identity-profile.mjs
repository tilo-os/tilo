/**
 * Where a person's state lives for one host: the profile names the device
 * keys, the certificate and the documents a browser keeps for that person at
 * that host (`peer.mjs`, `device-custody.mjs`).
 *
 * It lives in its own module so a page that needs the name alone (the
 * sign-in route, which holds no peer) reads this instead of the whole peer.
 */

/** The profile an identity keeps for one host. */
export const identityProfile = (identity, origin) => `tilo-identity:${identity.userPublicKey}@${new URL(origin).host}`;

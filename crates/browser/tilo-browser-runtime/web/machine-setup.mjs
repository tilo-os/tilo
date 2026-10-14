/**
 * Setting up a machine: making a Tilo node that waits for its owner
 * (`tilo-relay run --web … --install-token-file PATH`) the person's own,
 * from a page that holds their root.
 *
 * The node prints its device key (`{"event": "awaiting_certificate",
 * "devicePublicKey": …}`) and was provisioned with an install token; with
 * its HTTPS origin those are what the person hands this page, typed into
 * the desktop's "Set up a machine" or carried by an `install` link secret
 * (`#/me?install=…`, `installSecret`), which the entry page's scrubber
 * moves out of the address bar like every other secret. Nothing happens
 * until the person confirms: the root then certifies the node and the
 * session beside it, and the page installs both (`installNode`). From then
 * on the node hosts and governs the person's workspaces, so this browser
 * remembers it as a host the person owns.
 */
import {IdentityError, installNode} from './root-identity.mjs';

export const SET_UP = Object.freeze({id: 'set-up-machine', title: 'Set up a machine',
  subtitle: 'Make a Tilo node yours, with its address, key and install token'});

/** The node's shortest install token (`tilo-relay` refuses shorter ones). */
const MIN_TOKEN = 16;
const LOOPBACK = new Set(['localhost', '127.0.0.1', '[::1]']);

const base64url = text => {
  let binary = '';
  for (const byte of new TextEncoder().encode(text)) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
};
const unbase64url = value => {
  const binary = atob(value.replace(/-/g, '+').replace(/_/g, '/'));
  return new TextDecoder('utf-8', {fatal: true}).decode(Uint8Array.from(binary, char => char.charCodeAt(0)));
};

/**
 * `{origin, devicePublicKey, installToken}` as the install takes them: an
 * HTTPS origin (plain HTTP only on loopback), the node's 32-byte key in hex
 * and its token. Throws `IdentityError` `malformed` naming what is wrong.
 */
export function machineDetails({origin, devicePublicKey, installToken} = {}) {
  let url;
  try { url = new URL(String(origin ?? '').trim()); } catch { url = null; }
  if (!url || !(url.protocol === 'https:' || (url.protocol === 'http:' && LOOPBACK.has(url.hostname)))) {
    throw new IdentityError('malformed', 'The address is the machine’s HTTPS address, like https://machine.example:8443');
  }
  const key = String(devicePublicKey ?? '').trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(key)) {
    throw new IdentityError('malformed', 'The machine key is the 64-character devicePublicKey the machine printed');
  }
  const token = String(installToken ?? '').trim();
  if (token.length < MIN_TOKEN) {
    throw new IdentityError('malformed', `The install token is the text in the machine’s install token file (at least ${MIN_TOKEN} characters)`);
  }
  return {origin: url.origin, devicePublicKey: key, installToken: token};
}

/** The `install` link secret for a waiting machine. */
export function installSecret(details) {
  return base64url(JSON.stringify(machineDetails(details)));
}

/** The machine an `install` link secret names, checked as `machineDetails`. */
export function readInstallSecret(secret) {
  let parsed;
  try { parsed = JSON.parse(unbase64url(String(secret))); } catch { parsed = null; }
  if (!parsed || typeof parsed !== 'object') throw new IdentityError('malformed', 'This setup link is damaged, so Tilo did not use it');
  return machineDetails(parsed);
}

/** What each step of `setUpMachine` is said as while it runs. */
export const SETUP_PHASES = Object.freeze({
  checking: 'Asking the machine who it is…',
  certifying: 'Signing its certificate with your identity…',
  installing: 'Installing the certificate on the machine…',
});

/** What a person reads when setting up `origin` failed with `error`. */
export function setUpFailure(error, origin = 'the machine') {
  const host = (() => { try { return new URL(origin).host; } catch { return origin; } })();
  switch (error?.code) {
    case 'malformed': return error.message;
    case 'locked': return 'This browser does not hold your identity, so it cannot make a machine yours. Set it up from a browser where you are signed in.';
    case 'install-token-refused': return `${host} refused the install token. Use the text in its install token file.`;
    case 'not-waiting': return `${host} is already set up, or is not a machine waiting for its owner.`;
    case 'install-refused': return /another key/.test(error.message)
      ? `The machine at ${host} has another key than the one given.`
      : `${host} refused: ${error.message}`;
    case 'other-owner': return error.message;
    default: break;
  }
  // `fetch` rejects without a status: unreachable, or this page's origin
  // is not one the machine allows (`--allowed-origin`).
  if (error instanceof TypeError) return `Could not reach ${host}. Check the address, and that the machine allows this page (--allowed-origin).`;
  return `Setting up ${host} failed: ${error?.message ?? error}`;
}

/**
 * Make the machine `details` names the person's: `person()` resolves to the
 * unlocked root (`IdentityError` `locked` when this browser holds none for
 * the person it is), `install` is `installNode`. Resolves to `{origin,
 * devicePublicKey, userPublicKey}` once the machine took its certificate;
 * `onProgress({phase})` hears each step (`SETUP_PHASES`).
 */
export async function setUpMachine({details, person, runtime, install = installNode, fetch, onProgress = () => {}}) {
  const machine = machineDetails(details);
  const identity = await person();
  if (!identity?.sign || identity.locked) throw new IdentityError('locked', 'Unlock the identity first');
  const answer = await install({...machine, identity, runtime, fetch, onProgress,
    label: new URL(machine.origin).hostname.slice(0, 64)});
  // The node answers with the identity it now serves (hex).
  if (String(answer?.user_public_key ?? '').toLowerCase() !== identity.userPublicKey) {
    throw new IdentityError('other-owner', 'The machine took a certificate for someone else');
  }
  return {origin: machine.origin, devicePublicKey: machine.devicePublicKey, userPublicKey: identity.userPublicKey};
}

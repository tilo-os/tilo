/**
 * The workspaces this browser can open, and joining one through its host's
 * own admission.
 *
 * A workspace is data that names its host:
 * `{name, host: {origin, key?}, room?, admission?}`. `host.origin` is the
 * HTTPS origin of the relay that hosts the room, `host.key` the host's root
 * key (hex) once known, `room` its `RoomId` once known (a listed public
 * workspace learns it when admitted), and `admission` the base URL of the
 * host's admission door, present only when that host lets people in without
 * an invitation. Opening a workspace is the same admission everywhere
 * (`admission.mjs`): an invitation, a member's account, or, for a host that
 * says so, its admission door. Nothing here is special to any one workspace:
 * which ones a page offers comes from its web host's entry configuration.
 *
 * Joining through the host takes the person's identity from the caller,
 * which owns creating and keeping it: a person who already joined that host
 * reconnects; otherwise the host pre-admits the person
 * (`POST <admission>join`) and the page joins with the invitation and
 * one-time token at once, with no pairing code.
 *
 * Pairing (`pairViaHost`): a person's new device becomes theirs without
 * their root. It asks the host for a certificate for that person, shows its
 * pairing code, and one of the person's devices that holds the root
 * approves it; the host only relays. The membership kept for that host is
 * marked `paired`, so the next visit reconnects as that person rather than
 * joining as this browser's own.
 *
 * Spectating (the homepage embed): the page makes a device key in memory
 * only and asks the host to certify it as the room's one read-only
 * spectator person (`POST <admission>spectate`), then connects through the
 * ordinary `connectPeer` path with that certificate. Nothing is stored and
 * no person is created, so a visitor who only scrolls past leaves no member
 * behind.
 */
import {connectPeer, fitDisplayName, httpsPost, identityProfile, joinWorkspace, pairDevice} from './peer.mjs';

const MEMBERSHIPS = 'tilo-host-memberships';
const ADJECTIVES = ['Brave', 'Calm', 'Clever', 'Curious', 'Eager', 'Gentle', 'Happy', 'Jolly', 'Kind', 'Lively', 'Lucky', 'Merry', 'Nimble', 'Quiet', 'Swift', 'Witty'];
const ANIMALS = ['Badger', 'Beaver', 'Crane', 'Dolphin', 'Falcon', 'Fox', 'Hedgehog', 'Heron', 'Koala', 'Lynx', 'Otter', 'Owl', 'Panda', 'Puffin', 'Seal', 'Wren'];

/** The origin a workspace's host is reached at, normalized for comparison. */
const originOf = value => new URL(value).origin;

/**
 * The workspaces to offer: the page's home workspace (from its web host),
 * then the person's own, each host once (the first naming wins).
 */
export function listedWorkspaces({home = null, stored = []} = {}) {
  const seen = new Set();
  const listed = [];
  for (const workspace of [home, ...stored]) {
    if (!workspace?.host?.origin) continue;
    const origin = originOf(workspace.host.origin);
    if (seen.has(origin)) continue;
    seen.add(origin);
    listed.push(workspace);
  }
  return listed;
}

/** The listed workspace hosted at `origin`, if any. */
export function workspaceAt(workspaces, origin) {
  const wanted = originOf(origin);
  return workspaces.find(workspace => originOf(workspace.host.origin) === wanted) ?? null;
}

export class HostAdmissionError extends Error {
  constructor(code, message, retryAfter = null) {
    super(message);
    this.name = 'HostAdmissionError';
    this.code = code;
    this.retryAfter = retryAfter;
  }
}

export function randomDisplayName(random = Math.random) {
  const pick = list => list[Math.floor(random() * list.length) % list.length];
  return `${pick(ADJECTIVES)} ${pick(ANIMALS)}`;
}

const NAMES = 'tilo-display-names';
const readNames = store => {
  try { return JSON.parse(store?.getItem(NAMES) ?? '{}') ?? {}; } catch { return {}; }
};

/** The name the person `userPublicKey` chose in this browser, or `null`. */
export function chosenDisplayName(userPublicKey, store = globalThis.localStorage) {
  const name = userPublicKey ? readNames(store)[userPublicKey] : null;
  return typeof name === 'string' && name ? name : null;
}

/** Keep `name` as the person's chosen name here (empty forgets it). */
export function chooseDisplayName(userPublicKey, name, store = globalThis.localStorage) {
  const names = readNames(store);
  const chosen = fitDisplayName(name);
  if (chosen) names[userPublicKey] = chosen; else delete names[userPublicKey];
  store?.setItem(NAMES, JSON.stringify(names));
  return chosen;
}

const hex = bytes => Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
const CODES = {400: 'invalid', 403: 'cooling-down', 429: 'too-many', 503: 'full'};

function admissionDoor(workspace) {
  if (!workspace?.admission) throw new HostAdmissionError('invitation-only', `${workspace?.name ?? 'This workspace'} needs an invitation`);
  return workspace.admission;
}

async function ask({workspace, path, body, fetch}) {
  const response = await fetch(new URL(path, admissionDoor(workspace)), {
    method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(body),
  });
  const reply = await response.json().catch(() => ({}));
  if (response.ok) return reply;
  const retry = Number(response.headers.get('retry-after'));
  const hasRetry = response.headers.get('retry-after') != null && Number.isFinite(retry);
  // A 503 without Retry-After is the host failing, not the room being full.
  const code = response.status === 503 && !hasRetry ? 'unavailable' : (CODES[response.status] ?? 'unavailable');
  throw new HostAdmissionError(code, reply.error ?? `${path}: HTTP ${response.status}`, hasRetry && retry > 0 ? retry : null);
}

const readMemberships = store => {
  try { return JSON.parse(store?.getItem(MEMBERSHIPS) ?? '{}') ?? {}; } catch { return {}; }
};

/** Who this browser joined `workspace`'s host as, and in which room, or `null`. */
export function keptMembership(workspace, memberships = globalThis.localStorage) {
  return readMemberships(memberships)[originOf(workspace.host.origin)] ?? null;
}

/** The origins of the hosts this browser joined through, as `userPublicKey`
 * when given: the hosts that keep that person's envelope. */
export function membershipOrigins(userPublicKey, memberships = globalThis.localStorage) {
  return [...new Set(Object.values(readMemberships(memberships))
    .filter(kept => kept?.origin && (!userPublicKey || kept.userPublicKey === userPublicKey))
    .map(kept => kept.origin))];
}

function keepMembership(memberships, workspace, value) {
  const all = readMemberships(memberships);
  const key = originOf(workspace.host.origin);
  if (value) all[key] = value; else delete all[key];
  memberships?.setItem(MEMBERSHIPS, JSON.stringify(all));
}

/**
 * Remember the host at `origin` as one `userPublicKey` owns: a machine this
 * browser set up for that person (`machine-setup.mjs`). Like a joined host,
 * it keeps the person's envelope and profile.
 */
export function keepOwnedHost({origin, userPublicKey}, memberships = globalThis.localStorage) {
  const host = {host: {origin}};
  const kept = keptMembership(host, memberships);
  keepMembership(memberships, host, {
    origin: originOf(origin), room: kept?.userPublicKey === userPublicKey ? kept.room ?? null : null, userPublicKey, owner: true,
  });
}

/**
 * Remember that this browser is in `room` at the host at `origin` as
 * `userPublicKey`, whose root it holds (joined through an invitation, or a
 * workspace it created on a host it owns), so a later visit reconnects
 * (`hostToReconnect`).
 */
export function keepJoinedHost({origin, room, userPublicKey}, memberships = globalThis.localStorage) {
  const host = {host: {origin}};
  const kept = keptMembership(host, memberships);
  const owner = Boolean(kept?.owner && kept.userPublicKey === userPublicKey);
  keepMembership(memberships, host, {origin: originOf(origin), room, userPublicKey, ...(owner ? {owner} : {})});
}

/** The origins of the hosts this browser set up for `userPublicKey`. */
export function ownedHosts(userPublicKey, memberships = globalThis.localStorage) {
  return Object.values(readMemberships(memberships))
    .filter(kept => kept?.owner && kept.origin && kept.userPublicKey === userPublicKey)
    .map(kept => kept.origin);
}

/**
 * What this browser kept for the host at `origin` that the root of
 * `userPublicKey` it holds can reconnect with, or `null`: a host it set up
 * or joined as that person. Never a device paired to a person kept
 * elsewhere, which holds no root of theirs.
 */
export function hostToReconnect(origin, userPublicKey, memberships = globalThis.localStorage) {
  const kept = keptMembership({host: {origin}}, memberships);
  return kept && !kept.paired && kept.userPublicKey === userPublicKey ? kept : null;
}

/**
 * Join `workspace` as `identity` (the person, created and kept by the
 * caller) through its host's admission door; resolves to the peer.
 */
export async function joinViaHost({
  workspace, identity, displayName = randomDisplayName(), fetch = globalThis.fetch, memberships = globalThis.localStorage,
  join = joinWorkspace, connect = connectPeer, onProgress, ...connectOptions
}) {
  admissionDoor(workspace);
  const known = keptMembership(workspace, memberships);
  if (known?.paired) {
    // A device paired to a person kept elsewhere: only that person's
    // devices can let it in again, so a failure is the page's to show.
    return connect({...connectOptions, origin: known.origin, room: known.room, userPublicKey: known.userPublicKey,
      profile: identityProfile(known, known.origin), displayName});
  }
  if (!identity?.userPublicKey) throw new TypeError("joining through a host needs the person's identity");
  if (known?.userPublicKey === identity.userPublicKey) {
    try {
      return await connect({...connectOptions, origin: known.origin, room: known.room, identity, displayName});
    } catch (error) {
      // The membership is gone (pruned after a long absence): ask the host
      // to admit the same person again rather than stop at an error.
      console.warn('host admission: reconnecting failed, asking to join again', error);
      keepMembership(memberships, workspace, null);
    }
  }
  const {invitation, admissionToken, origin} = await ask({
    workspace, path: 'join', fetch, body: {userPublicKey: identity.userPublicKey, displayName: fitDisplayName(displayName)},
  });
  let room = null;
  const peer = await join({
    ...connectOptions, origin, invitation, admissionToken, identity, displayName,
    onProgress: event => { if (event.phase === 'admitted') room = event.room; onProgress?.(event); },
  });
  keepMembership(memberships, workspace, {origin, room: room ?? peer.room, userPublicKey: identity.userPublicKey});
  return peer;
}

/**
 * Become a new device of `userPublicKey` (hex), the person whose root
 * approves it on another of their devices, in `room` at the host at
 * `origin`. `onPairingCode(code)` is called with the code that device
 * shows the person; resolves to the peer once approved. Where that host
 * lets people in through its door (`workspace.admission`), the new device
 * joins the person's workspace there as theirs. The host is remembered as
 * joined, paired, for the next visit (`joinViaHost`).
 */
export async function pairViaHost({
  origin, room, userPublicKey, workspace = null, label = 'Browser', displayName = randomDisplayName(),
  memberships = globalThis.localStorage, fetch = globalThis.fetch, pair = pairDevice, ...connectOptions
}) {
  if (!/^[0-9a-f]{64}$/.test(userPublicKey ?? '')) throw new HostAdmissionError('invalid', 'This pairing link names no person');
  const admission = workspace?.admission
    ? () => ask({workspace, path: 'join', fetch, body: {userPublicKey, displayName: fitDisplayName(displayName)}})
    : undefined;
  const peer = await pair({...connectOptions, origin, room, userPublicKey, label, displayName, fetch, admission,
    profile: identityProfile({userPublicKey}, origin)});
  keepMembership(memberships, {host: {origin}}, {origin, room: peer.room ?? room, userPublicKey, paired: true});
  return peer;
}

/** A device key that lives only in this page: never stored, gone on reload. */
async function memoryCustody(crypto) {
  let pair;
  try {
    pair = await crypto.subtle.generateKey({name: 'Ed25519'}, false, ['sign', 'verify']);
  } catch (error) {
    throw new HostAdmissionError('unavailable', `this browser cannot watch: ${error?.message ?? error}`);
  }
  const publicKey = new Uint8Array(await crypto.subtle.exportKey('raw', pair.publicKey));
  return {
    kind: 'webcrypto-ed25519',
    publicKey,
    sign: async message => new Uint8Array(await crypto.subtle.sign({name: 'Ed25519'}, pair.privateKey, message)),
    close: async () => {},
    destroy: async () => {},
  };
}

/** `{get, put, delete}` peer state that is gone with the page. */
function memoryStore() {
  const map = new Map();
  return {get: async key => map.get(key), put: async (key, value) => { map.set(key, value); }, delete: async key => { map.delete(key); }};
}

/**
 * Watch `workspace` read-only as its spectator person. Rejects with
 * `HostAdmissionError` code `full` above the host's spectator cap, so an
 * embedding site can show its static fallback.
 */
export async function spectate({
  workspace, fetch = globalThis.fetch, crypto = globalThis.crypto, connect = connectPeer,
  runtime, loadRuntime = async () => (await import('./host.mjs')).loadRuntime(), ...connectOptions
}) {
  const custody = await memoryCustody(crypto);
  const {certificate, origin, room} = await ask({
    workspace, path: 'spectate', fetch, body: {devicePublicKey: hex(custody.publicKey)},
  });
  runtime ??= await loadRuntime();
  const {hostCert} = JSON.parse(await runtime.hostInfo(httpsPost(origin, fetch)));
  return connect({
    ...connectOptions, origin, room, runtime, custody, store: memoryStore(), fetch,
    userPublicKey: hex(certificate.user_public_key), label: 'Spectator',
    deviceCertificate: JSON.stringify({cert: certificate, hostCert}),
  });
}

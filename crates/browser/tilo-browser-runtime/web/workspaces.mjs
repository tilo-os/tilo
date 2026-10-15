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
 * A host set up again (a new device key behind the same origin) is not the
 * one this browser kept the person's state for. Nothing kept is reused or
 * handed to it: the page asks the person (`confirmRejoin`) and, if they
 * agree, the person joins it through its door once, like anyone new; the
 * peer then keeps that host's state in a namespace of its own
 * (`peer.mjs`), and what this browser kept for the previous host stays on
 * this device.
 *
 * Pairing (`pairViaHost`): a person's new device becomes theirs without
 * their root. It asks the host for a certificate for that person, shows its
 * pairing code, and one of the person's devices that holds the root
 * approves it; the host only relays. The membership kept for that host is
 * marked `paired`, so the next visit reconnects as that person rather than
 * joining as this browser's own.
 *
 * Returning (`enteredWorkspaces`): the rooms this browser entered as a
 * member are remembered, so a person invited to several workspaces, on
 * several hosts, can switch between them (`workspaceChoices`, offered as
 * host actions in desktop search) and a bare entry reopens the one they
 * last chose (`chosenWorkspace`). Remembering grants nothing: opening one
 * is the room's own admission again, and one that refuses this browser is
 * marked as such rather than reopened.
 *
 * Spectating (the homepage embed): the page makes a device key in memory
 * only and asks the host to certify it as the room's one read-only
 * spectator person (`POST <admission>spectate`), then connects through the
 * ordinary `connectPeer` path with that certificate, watching only (it
 * writes nothing, and takes the keys of what members seal from the room's
 * steward). Nothing is stored and
 * no person is created, so a visitor who only scrolls past leaves no member
 * behind.
 */
import {AdmissionError} from './admission.mjs';
import {connectPeer, fitDisplayName, httpsPost, identityProfile, joinWorkspace, NOT_REJOINED, pairDevice} from './peer.mjs';
import {memberText} from './json-text.mjs';

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

const ENTERED = 'tilo-entered-workspaces';
const CHOSEN = 'tilo-chosen-workspace';
/** The most rooms remembered as entered; the least recently opened go first. */
const MAX_ENTERED = 32;
/** Host action ids that open one of the person's workspaces. */
export const OPEN_WORKSPACE = 'open-workspace:';

const readJson = (store, key, fallback) => {
  try { return JSON.parse(store?.getItem(key) ?? 'null') ?? fallback; } catch { return fallback; }
};
const sameRoom = (a, b) => originOf(a.origin) === originOf(b.origin) && a.room === b.room;
const validEntry = entry => {
  try { return typeof entry?.room === 'string' && entry.room && Boolean(originOf(entry.origin)); } catch { return false; }
};
const readEntered = store => {
  const list = readJson(store, ENTERED, []);
  return Array.isArray(list) ? list.filter(validEntry) : [];
};
const writeEntered = (store, list) => store?.setItem(ENTERED, JSON.stringify(list.slice(0, MAX_ENTERED)));

/**
 * The rooms this browser entered as a member (an invitation it redeemed, an
 * account it holds, a device it paired), most recently opened first:
 * `{origin, room, name, unavailable}`. `unavailable` says the last attempt
 * to open it failed: the host refused this browser, or could not be reached.
 */
export function enteredWorkspaces(store = globalThis.localStorage) {
  return readEntered(store).map(({origin, room, name = null, unavailable = false}) => ({
    origin: originOf(origin), room, name: typeof name === 'string' && name ? name : null, unavailable: unavailable === true,
  }));
}

/** Remember that this browser opened the room `{origin, room}` as a member,
 * under `name` when one is known (an earlier name stays otherwise). */
export function rememberEntered({origin, room, name = null}, store = globalThis.localStorage) {
  const entry = {origin: originOf(origin), room};
  if (!validEntry(entry)) return;
  const list = readEntered(store);
  const earlier = list.find(kept => sameRoom(kept, entry));
  const kept = name || earlier?.name || null;
  writeEntered(store, [{...entry, ...(kept ? {name: kept} : {})}, ...list.filter(other => !sameRoom(other, entry))]);
}

/** Name a remembered room after the workspace it showed, if it has no name yet. */
export function nameEntered({origin, room, name}, store = globalThis.localStorage) {
  if (typeof name !== 'string' || !name) return;
  const list = readEntered(store);
  const entry = list.find(kept => sameRoom(kept, {origin, room}));
  if (!entry || entry.name) return;
  entry.name = name;
  writeEntered(store, list);
}

/** The last attempt to open the remembered room `{origin, room}` failed: say
 * so where it is offered, and stop reopening it on a bare entry. */
export function markUnavailable({origin, room}, store = globalThis.localStorage) {
  const list = readEntered(store);
  const entry = list.find(kept => sameRoom(kept, {origin, room}));
  if (!entry) return;
  entry.unavailable = true;
  writeEntered(store, list);
  const chosen = readJson(store, CHOSEN, null);
  if (chosen && validEntry(chosen) && sameRoom(chosen, entry)) store?.removeItem(CHOSEN);
}

/** Keep `{origin, room}` as the workspace a bare entry reopens; `null` goes
 * back to the web host's home workspace. */
export function chooseWorkspace(choice, store = globalThis.localStorage) {
  if (choice && validEntry(choice)) store?.setItem(CHOSEN, JSON.stringify({origin: originOf(choice.origin), room: choice.room}));
  else store?.removeItem(CHOSEN);
}

/** The workspace the person last chose to open, while this browser still
 * remembers entering it and nothing refused it since; else `null`. */
export function chosenWorkspace(store = globalThis.localStorage) {
  const chosen = readJson(store, CHOSEN, null);
  if (!chosen || !validEntry(chosen)) return null;
  const entry = enteredWorkspaces(store).find(kept => sameRoom(kept, chosen));
  return entry && !entry.unavailable ? {origin: entry.origin, room: entry.room} : null;
}

/**
 * The workspaces the person can switch to from `current` (a route scope, or
 * `null`): the web host's `home` workspace, then the rooms they `entered`,
 * never the one in front. `homeRoom` is the room this browser was admitted
 * to through the home's door, so that room is offered once, as the home.
 * Each choice is `{id, name, origin, room, home, unavailable}`; `id` is its
 * host action id.
 */
export function workspaceChoices({home = null, homeRoom = null, entered = [], current = null}) {
  const here = current?.kind === 'room' ? current : null;
  const homeOrigin = home?.host?.origin ? originOf(home.host.origin) : null;
  const isHome = scope => homeOrigin !== null && originOf(scope.host ?? scope.origin) === homeOrigin
    && (scope.room == null || scope.room === (home.room ?? homeRoom));
  const choices = [];
  if (homeOrigin && !(here && isHome(here))) {
    choices.push({id: `${OPEN_WORKSPACE}home`, name: home.name || new URL(homeOrigin).host, origin: homeOrigin,
      room: home.room ?? null, home: true, unavailable: false});
  }
  for (const entry of entered) {
    if (isHome(entry) || (here && originOf(here.host) === entry.origin && here.room === entry.room)) continue;
    choices.push({id: `${OPEN_WORKSPACE}${entry.origin}/${entry.room}`, name: entry.name || new URL(entry.origin).host,
      origin: entry.origin, room: entry.room, home: false, unavailable: entry.unavailable});
  }
  return choices;
}

/** The host actions that open `choices` (`workspaceChoices`), for desktop search. */
export function workspaceActions(choices) {
  return choices.map(choice => {
    const host = new URL(choice.origin).host;
    return {id: choice.id, title: `Open ${choice.name}`,
      subtitle: choice.unavailable ? `Workspace · ${host} · did not open last time` : `Workspace · ${host}`};
  });
}

/** What a person the host took out of a workspace is told. */
export const removedFrom = name => `You were removed from ${name || 'this workspace'}. To come back, ask someone in it for a new invitation.`;

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
 * caller) through its host's admission door; resolves to the peer. When
 * the host was set up again since this browser joined it,
 * `confirmRejoin()` asks the person whether to join it again (resolving to
 * whether they do); without it, or when they decline, the `host-changed`
 * refusal stands and no join is made.
 */
export async function joinViaHost({
  workspace, identity, displayName = randomDisplayName(), fetch = globalThis.fetch, memberships = globalThis.localStorage,
  join = joinWorkspace, connect = connectPeer, onProgress, confirmRejoin, ...connectOptions
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
  /** Whether the host refused the membership this browser kept. */
  let refused = false;
  if (known?.userPublicKey === identity.userPublicKey) {
    let peer;
    try {
      peer = await connect({...connectOptions, origin: known.origin, room: known.room, identity, displayName});
      // A host refuses a membership it no longer has once the session
      // starts, not when connecting: wait for its answer here.
      await peer?.whenReady?.();
      return peer;
    } catch (error) {
      await peer?.dispose?.().catch(() => {});
      // This browser could not use its own device key (another tab holds
      // it), or the host has not answered yet: nothing the host refused,
      // so the membership stays, and joining again would spend a seat and
      // a join to fail the same way.
      if (error?.name === 'CustodyError' || error?.name === 'TimeoutError') throw error;
      if (error?.code === 'host-changed') {
        // Another host answers at this origin now. Joining it is the
        // person's choice: it is a new admission, and what this browser kept
        // stays pinned to the previous host.
        if (!confirmRejoin) throw error;
        if (!await confirmRejoin()) throw new AdmissionError('host-changed', NOT_REJOINED);
      } else {
        // The membership is gone (pruned after a long absence, or the host
        // started its room anew): ask the host to admit the same person
        // again rather than stop at an error.
        console.warn('host admission: reconnecting failed, asking to join again', error);
        refused = true;
      }
      keepMembership(memberships, workspace, null);
    }
  }
  let admitted;
  try {
    admitted = await ask({
      workspace, path: 'join', fetch, body: {userPublicKey: identity.userPublicKey, displayName: fitDisplayName(displayName)},
    });
  } catch (error) {
    // The host no longer has the person, and its door turns away the
    // address they joined from because someone was removed: they were taken
    // out of the room, not pruned after an absence. Only an invitation
    // brings them back.
    if (refused && error?.code === 'cooling-down') throw new AdmissionError('removed', removedFrom(workspace.name));
    throw error;
  }
  const {invitation, admissionToken, origin} = admitted;
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
  const hostText = await runtime.hostInfo(httpsPost(origin, fetch));
  // Both certificates are passed on as the JSON text they arrived as: their
  // nanosecond timestamps exceed what a JavaScript number holds exactly, and
  // a rounded copy no longer matches its signature (json-text.mjs).
  return connect({
    ...connectOptions, origin, room, runtime, custody, store: memoryStore(), fetch,
    userPublicKey: hex(JSON.parse(certificate).user_public_key), label: 'Spectator', watching: true,
    deviceCertificate: `{"cert":${certificate},"hostCert":${memberText(hostText, 'hostCert')}}`,
  });
}

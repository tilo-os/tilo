/**
 * The workspaces this browser can open, and joining one through its host's
 * own admission.
 *
 * A workspace is data that names its host:
 * `{name, host: {origin, key?}, workspace?, admission?}`. `host.origin` is the
 * HTTPS origin of the relay that hosts the workspace, `host.key` the host's root
 * key (hex) once known, `workspace` its id once known (a listed public
 * workspace learns it when admitted), and `admission` the base URL of the
 * host's admission door, present only when that host lets people in without
 * an invitation. Opening a workspace is the same admission everywhere
 * (`admission.mjs`): an invitation, a member's account, or, for a host that
 * says so, its admission door. Nothing here is special to any one workspace:
 * which ones a page offers comes from its web host's entry configuration.
 *
 * Joining through the host takes the person's identity from the caller,
 * which owns creating and keeping it: a person who already joined that host
 * reconnects; otherwise the door admits the person (`POST <admission>join`)
 * and the page joins with the invitation and one-time token at once, with no
 * pairing code. A workspace that is full answers `503` with `Retry-After`:
 * the page waits that long and asks again, until there is a seat.
 *
 * A host set up again (a new device key behind the same origin) is not the
 * one this browser kept the person's state for. Nothing kept is reused or
 * handed to it: the person joins it through its door once, like anyone
 * new, without being asked (hosts are nothing a person decides about); the
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
 * Returning (`enteredWorkspaces`): the workspaces this browser entered as a
 * member are remembered, so a person invited to several workspaces, on
 * several hosts, can switch between them (`workspaceChoices`, offered as
 * host actions in desktop search) and a bare entry reopens the one they
 * last chose (`chosenWorkspace`). Remembering grants nothing: opening one
 * is the workspace's own admission again, and one that refuses this browser is
 * marked as such rather than reopened.
 *
 * Each remembered workspace is pinned to the host that held it (its device key).
 * A workspace lives in its host's state, so once another host answers at that
 * origin (it was set up again on fresh state) the workspace is gone, and this
 * browser forgets it (`forgetGoneWorkspaces`) rather than offer a workspace that
 * can never open. Only that answer, from the origin itself, forgets one: a
 * host that cannot be reached, or fails, says nothing about its workspaces, so
 * they stay, marked as not opening last time (`settleFailedWorkspace`).
 */
import {AdmissionError} from './admission.mjs';
import {connectPeer, fitDisplayName, identityProfile, joinWorkspace, pairDevice, renewDevice, withCustody} from './peer.mjs';
import {idbStorage, openDeviceCustody} from './device-custody.mjs';
import {originOf} from './entered-workspaces.mjs';
import {backoffDelay} from './reconnect.mjs';
import {hostUnreachable} from './session-refusal.mjs';

// The workspaces this browser entered are its own store (`entered-workspaces.mjs`):
// offered here as the choices a person switches between.
export {
  chosenWorkspace, chooseWorkspace, enteredWorkspaces, forgetGoneWorkspaces, hostDeviceOf, markUnavailable, nameEntered,
  rememberEntered, settleFailedWorkspace,
} from './entered-workspaces.mjs';

const MEMBERSHIPS = 'tilo-host-memberships';
const ADJECTIVES = ['Brave', 'Calm', 'Clever', 'Curious', 'Eager', 'Gentle', 'Happy', 'Jolly', 'Kind', 'Lively', 'Lucky', 'Merry', 'Nimble', 'Quiet', 'Swift', 'Witty'];
const ANIMALS = ['Badger', 'Beaver', 'Crane', 'Dolphin', 'Falcon', 'Fox', 'Hedgehog', 'Heron', 'Koala', 'Lynx', 'Otter', 'Owl', 'Panda', 'Puffin', 'Seal', 'Wren'];

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

/**
 * The listed workspace the workspace `scope` names, when it is one: its host
 * lists a workspace and the workspace is that workspace's, as the listing names
 * it, else the workspace this browser was let into through its door
 * (`admittedWorkspace` on this visit, or the membership it kept). Another workspace
 * on the same host is not the listed one, and goes by no name or count of
 * the listed one's.
 */
export function listedAt(workspaces, scope, admittedWorkspace = null, memberships = globalThis.localStorage) {
  if (scope?.kind !== 'room') return null;
  const listed = workspaceAt(workspaces, scope.host);
  if (!listed) return null;
  const workspace = listed.room ?? admittedWorkspace ?? keptMembership(listed, memberships)?.room ?? null;
  return scope.room == null || scope.room === workspace ? listed : null;
}

/** Host action ids that open one of the person's workspaces. */
export const OPEN_WORKSPACE = 'open-workspace:';

/** Whether `scope` (`{host | origin, workspace}`) is the web host's `home`
 * workspace; `homeWorkspace` is the workspace this browser was admitted to through its
 * door. */
function isHomeWorkspace({home, homeWorkspace = null, scope}) {
  const homeOrigin = home?.host?.origin ? originOf(home.host.origin) : null;
  return homeOrigin !== null && Boolean(scope?.host ?? scope?.origin)
    && originOf(scope.host ?? scope.origin) === homeOrigin
    && (scope.room == null || scope.room === (home.room ?? homeWorkspace));
}

/**
 * The workspaces the person can switch to from `current` (a route scope, or
 * `null`): the web host's `home` workspace, then the workspaces they `entered`,
 * never the one in front. `homeWorkspace` is the workspace this browser was admitted
 * to through the home's door, so that workspace is offered once, as the home.
 * Where the person may have a desktop of their own (`own`), it comes first,
 * so a workspace is never a dead end. Each choice is
 * `{id, name, origin, workspace, home, own, unavailable}`; `id` is its host
 * action id.
 */
export function workspaceChoices({home = null, homeWorkspace = null, entered = [], current = null, own = false}) {
  const here = current?.kind === 'room' ? current : null;
  const homeOrigin = home?.host?.origin ? originOf(home.host.origin) : null;
  const isHome = scope => isHomeWorkspace({home, homeWorkspace, scope});
  const choices = [];
  if (own && current?.kind !== 'own') {
    choices.push({id: `${OPEN_WORKSPACE}own`, name: 'Your desktop', origin: null, room: null, home: false, own: true, unavailable: false});
  }
  if (homeOrigin && !(here && isHome(here))) {
    choices.push({id: `${OPEN_WORKSPACE}home`, name: home.name || new URL(homeOrigin).host, origin: homeOrigin,
      room: home.room ?? null, home: true, own: false, unavailable: false});
  }
  for (const entry of entered) {
    if (isHome(entry) || (here && originOf(here.host) === entry.origin && here.room === entry.room)) continue;
    choices.push({id: `${OPEN_WORKSPACE}${entry.origin}/${entry.room}`, name: entry.name || new URL(entry.origin).host,
      origin: entry.origin, room: entry.room, home: false, own: false, unavailable: entry.unavailable});
  }
  return choices;
}

/**
 * The host actions that open `choices` (`workspaceChoices`), for desktop
 * search. The workspace menu lists each by name beside the one in front
 * (named `current`), so two that share a name are told apart: by their
 * host, and where one host holds both, by their workspace too.
 */
export function workspaceActions(choices, {current = null} = {}) {
  const names = [current, ...choices.map(choice => choice.name)].filter(Boolean);
  const shared = name => names.filter(other => other === name).length > 1;
  return choices.map(choice => {
    if (choice.own) {
      return {id: choice.id, title: 'Open your desktop', label: choice.name, source: 'workspaces', subtitle: 'Your own desktop, in this browser'};
    }
    const host = new URL(choice.origin).host;
    const sameHost = choices.filter(other => other.name === choice.name && other.origin === choice.origin).length > 1;
    const label = !shared(choice.name) ? choice.name
      : sameHost && choice.room ? `${choice.name} · ${host} · ${choice.room.slice(0, 8)}` : `${choice.name} · ${host}`;
    // `label` and `source`: the desktop's workspace menu lists it by name.
    return {id: choice.id, title: `Open ${label}`, label, source: 'workspaces',
      subtitle: choice.unavailable ? `Workspace · ${host} · did not open last time` : `Workspace · ${host}`};
  });
}

/**
 * The workspaces this page holds, as the directory sheet shows them
 * (`mountDesktop({directory: {held}})`, `Held` in the runtime): each by the
 * workspace id a site lists it as, and by the host action id that opens it.
 * `current` (the scope in front) comes too, so the workspace the page is in
 * says Open rather than Join.
 */
export function heldWorkspaces({entered = [], current = null} = {}) {
  const held = [];
  const seen = new Set();
  const add = (origin, room, name) => {
    if (!origin || !room || seen.has(room)) return;
    seen.add(room);
    const at = originOf(origin);
    held.push({workspace: room, key: `${OPEN_WORKSPACE}${at}/${room}`, name: name || new URL(at).host});
  };
  if (current?.kind === 'room') add(current.host, current.room, current.name ?? null);
  for (const entry of entered) add(entry?.origin, entry?.room, entry?.name ?? null);
  return held;
}

/**
 * Join the workspace a site's directory let this person into
 * (`mountDesktop({onJoinGranted})`): the sheet's Join already asked the
 * door, so the page joins with what it answered — the invitation, and the
 * one-time token only this page can redeem — through the same join a link's
 * invitation takes (`join`, the page's `joinWorkspace`), then shows it.
 * Resolves to `{peer, origin, room, name}`.
 */
export async function joinGranted({origin, invitation, admissionToken = '', workspace = null, name = null}, {join, displayName = null, ...connectOptions} = {}) {
  if (!origin || !invitation) throw new TypeError('a granted join names the origin that granted it, and its invitation');
  if (typeof join !== 'function') throw new TypeError('a granted join needs the page\'s join');
  const peer = await join({...connectOptions, origin, invitation, admissionToken, ...(displayName ? {displayName} : {})});
  const room = peer?.workspace ?? workspace;
  return {peer, origin: originOf(origin), room, name: name || room};
}

/** What a person the host took out of a workspace is told. */
export const removedFrom = name => `You were removed from ${name || 'this workspace'}. To come back, ask someone in it for a new invitation.`;

export class HostAdmissionError extends Error {
  /** `door`: what the door said beside its refusal: `{waitList, contact}`
   * from a workspace taking people by invitation that keeps a wait list. */
  constructor(code, message, retryAfter = null, door = null) {
    super(message);
    this.name = 'HostAdmissionError';
    this.code = code;
    this.retryAfter = retryAfter;
    this.door = door;
  }
}

export function randomDisplayName(random = Math.random) {
  const pick = list => list[Math.floor(random() * list.length) % list.length];
  return `${pick(ADJECTIVES)} ${pick(ANIMALS)}`;
}

/**
 * The name a person goes by until they choose one: drawn from their key, so
 * every visit and every way in names them alike. A name drawn afresh per
 * visit showed one person under two names, to the workspace and to what it is
 * bridged with. Random only for a page that has no key yet.
 */
export function defaultDisplayName(userPublicKey, random = Math.random, taken = []) {
  if (!/^[0-9a-f]{4}/i.test(userPublicKey ?? '')) return randomDisplayName(random);
  const used = new Set(taken.map(name => String(name).toLowerCase()));
  const free = name => !used.has(name.toLowerCase());
  // The key's bytes, read two at a time: the first pair is the person's
  // name; the next ones stand in when someone in the workspace already goes by
  // it, so two people are never told apart only by "(1)" and "(2)" on each
  // screen while what they are bridged with shows one name for both.
  const bytes = userPublicKey.match(/[0-9a-f]{2}/gi).map(byte => parseInt(byte, 16));
  let first = null;
  for (let at = 0; at + 2 < bytes.length || at === 0; at += 4) {
    const adjective = bytes[at] ?? 0;
    const animal = bytes[at + 2] ?? bytes[at + 1] ?? 0;
    const name = `${ADJECTIVES[adjective % ADJECTIVES.length]} ${ANIMALS[animal % ANIMALS.length]}`;
    first ??= name;
    if (free(name)) return name;
  }
  // Every name the key draws is taken: the first, numbered.
  for (let n = 2; ; n += 1) if (free(`${first} ${n}`)) return `${first} ${n}`;
}

/**
 * The name the person `userPublicKey`, who has not chosen one, takes when the
 * workspace's `lines` (from `/here`: `[{user, label}]`) show someone else already
 * going by `current`; `null` when they keep it. Of two people who drew the
 * same name the one with the greater key draws again, so both never do.
 */
export function redrawnDisplayName(userPublicKey, current, lines) {
  if (!userPublicKey || !current || !Array.isArray(lines)) return null;
  const others = lines.filter(line => line?.user && line.user !== userPublicKey && typeof line.label === 'string');
  const same = others.filter(line => line.label.toLowerCase() === current.toLowerCase());
  if (!same.length || same.every(line => line.user > userPublicKey)) return null;
  const name = defaultDisplayName(userPublicKey, Math.random, others.map(line => line.label));
  return name === current ? null : name;
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
  // A 503 without Retry-After is the host failing, not the workspace being full.
  // A 403 naming the person (`removed`) rather than their address, or a
  // workspace that takes people by invitation (`invitation-only`).
  const code = response.status === 403 && ['removed', 'invitation-only'].includes(reply.code) ? reply.code
    : response.status === 503 && !hasRetry ? 'unavailable' : (CODES[response.status] ?? 'unavailable');
  // A workspace taking people by invitation may keep a wait list, and name where
  // to ask to be taken off it.
  const door = code === 'invitation-only' && reply.waitList === true
    ? {waitList: true, contact: typeof reply.contact === 'string' && reply.contact ? reply.contact : null, mail: reply.mail === true} : null;
  throw new HostAdmissionError(code, reply.error ?? `${path}: HTTP ${response.status}`, hasRetry && retry > 0 ? retry : null, door);
}

/** Why the door kept no address, by its `code`: the cases a door page says. */
const WAIT_CODES = new Set(['invalid-email', 'invalid-secret', 'too-many', 'wait-list-full', 'no-wait-list']);

/** Where this browser keeps what it left on hosts' wait lists: per door
 * origin, the address and the secret that takes it off again. */
const WAIT_LISTINGS = 'tilo-wait-listings';

const readListings = store => {
  try { return JSON.parse(store?.getItem(WAIT_LISTINGS) ?? '{}') ?? {}; } catch { return {}; }
};

const writeListings = (store, listings) => {
  try {
    if (Object.keys(listings).length) store?.setItem(WAIT_LISTINGS, JSON.stringify(listings));
    else store?.removeItem(WAIT_LISTINGS);
  } catch {
    // Storage refused (private mode, full): the address stays listed, and
    // the contact the door names still takes it off.
  }
};

/** The door `POST`ed to, as JSON, without credentials; `HostAdmissionError`
 * `unavailable` when it cannot be reached. */
async function postToDoor(fetch, url, body, what) {
  try {
    return await fetch(url, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(body), credentials: 'omit'});
  } catch (error) {
    throw new HostAdmissionError('unavailable', `${what}: ${error?.message ?? error}`);
  }
}

async function doorRefusal(response, what) {
  const reply = await response.json().catch(() => ({}));
  const retry = Number(response.headers.get('retry-after'));
  const code = WAIT_CODES.has(reply.code) ? reply.code : 'unavailable';
  return new HostAdmissionError(code, reply.error ?? `${what}: HTTP ${response.status}`,
    Number.isFinite(retry) && retry > 0 ? retry : null);
}

/**
 * Leave `email` at `workspace`'s door, to be invited at (`POST
 * <admission>wait-list`), in the reader's `language`: a workspace taking people
 * by invitation keeps it for whoever may invite. A door that mails sends a
 * link that confirms it, and lists it only then. The secret the door hands
 * back, which takes the address off again (`leaveWaitList`), is kept in
 * `storage` for that door. Resolves to `{email, mail}`: the address as the
 * door keeps it (as typed, trimmed), and whether it mailed a link to
 * confirm; rejects with `HostAdmissionError` code `invalid-email`,
 * `too-many` (with `retryAfter`), `wait-list-full`, `no-wait-list` or
 * `unavailable`.
 */
export async function leaveOnWaitList({workspace, email, language = globalThis.navigator?.language ?? '',
  fetch = globalThis.fetch, storage = globalThis.localStorage}) {
  const door = new URL('wait-list', admissionDoor(workspace));
  const response = await postToDoor(fetch, door, {email: String(email ?? ''), language: String(language)}, 'wait list');
  if (!response.ok) throw await doorRefusal(response, 'wait list');
  const reply = await response.json().catch(() => ({}));
  if (typeof reply.email !== 'string') throw new HostAdmissionError('unavailable', 'wait list: the door named no address');
  const mail = reply.mail === true;
  if (typeof reply.removal === 'string' && SECRET.test(reply.removal)) {
    writeListings(storage, {...readListings(storage), [door.origin]: {email: reply.email, secret: reply.removal, mail}});
  }
  return {email: reply.email, mail};
}

/** A wait-list secret, as the door hands it out and its links carry it. */
const SECRET = /^[0-9a-f]{64}$/;

/** What this browser left on `workspace`'s wait list and can take off
 * again: `{email, pending}` (`pending`: the door mailed a link to confirm
 * it, not known to be opened yet), or `null`. */
export function waitListing(workspace, storage = globalThis.localStorage) {
  if (!workspace?.admission) return null;
  const kept = readListings(storage)[new URL(workspace.admission).origin];
  return typeof kept?.email === 'string' && typeof kept?.secret === 'string' ? {email: kept.email, pending: kept.mail === true && kept.confirmed !== true} : null;
}

const forgetListing = (storage, origin) => {
  const now = readListings(storage);
  delete now[origin];
  writeListings(storage, now);
};

/**
 * Take an address off `workspace`'s wait list (`POST
 * <admission>wait-list/remove`): the one this browser left, or the one
 * whose removal link carried `secret`; this browser's listing is forgotten
 * either way. The door answers alike whether or not it still listed the
 * address, so this resolves either way; it rejects with
 * `HostAdmissionError` code `too-many` (with `retryAfter`) or
 * `unavailable`, and keeps the secret to try again. A secret the door
 * calls none is forgotten.
 */
export async function leaveWaitList({workspace, secret = null, fetch = globalThis.fetch, storage = globalThis.localStorage}) {
  const door = new URL('wait-list/remove', admissionDoor(workspace));
  const sent = secret ?? readListings(storage)[door.origin]?.secret;
  if (typeof sent !== 'string' || !SECRET.test(sent)) {
    if (secret === null) return;
    throw new HostAdmissionError('invalid-secret', 'leaving the wait list: the link carries no secret');
  }
  const response = await postToDoor(fetch, door, {secret: sent}, 'leaving the wait list');
  const refusal = response.ok ? null : await doorRefusal(response, 'leaving the wait list');
  if (!refusal || refusal.code === 'invalid-secret' || refusal.code === 'no-wait-list') {
    forgetListing(storage, door.origin);
    return;
  }
  throw refusal;
}

/**
 * Confirm the address whose confirmation link carried `token` (`POST
 * <admission>wait-list/confirm`): whether the door still had it waiting
 * (`false`: it was taken off, or not confirmed within two days). Rejects
 * with `HostAdmissionError` as `leaveWaitList` does.
 */
export async function confirmWaitList({workspace, token, fetch = globalThis.fetch, storage = globalThis.localStorage}) {
  const door = new URL('wait-list/confirm', admissionDoor(workspace));
  if (typeof token !== 'string' || !SECRET.test(token)) return false;
  const response = await postToDoor(fetch, door, {token}, 'confirming the wait list');
  if (!response.ok) {
    const refusal = await doorRefusal(response, 'confirming the wait list');
    if (refusal.code === 'invalid-secret') return false;
    throw refusal;
  }
  const reply = await response.json().catch(() => ({}));
  const listings = readListings(storage);
  if (reply.confirmed === true && listings[door.origin]) {
    // This browser left it: it no longer waits to be confirmed.
    writeListings(storage, {...listings, [door.origin]: {...listings[door.origin], confirmed: true}});
  }
  return reply.confirmed === true;
}

/**
 * Where what this browser left on `workspace`'s wait list stands (`POST
 * <admission>wait-list/standing`): `pending` (to be confirmed), `listed`,
 * `invited` or `gone` (then forgotten here); `null` when nothing is kept
 * here or the door cannot say now.
 */
export async function waitListStanding({workspace, fetch = globalThis.fetch, storage = globalThis.localStorage}) {
  const door = new URL('wait-list/standing', admissionDoor(workspace));
  const kept = readListings(storage)[door.origin];
  if (typeof kept?.secret !== 'string') return null;
  let response;
  try {
    response = await postToDoor(fetch, door, {secret: kept.secret}, 'the wait list');
  } catch {
    return null;
  }
  if (!response.ok) return null;
  const {standing} = await response.json().catch(() => ({}));
  if (!['pending', 'listed', 'invited', 'gone'].includes(standing)) return null;
  if (standing === 'gone') forgetListing(storage, door.origin);
  return standing;
}

const readMemberships = store => {
  try { return JSON.parse(store?.getItem(MEMBERSHIPS) ?? '{}') ?? {}; } catch { return {}; }
};

/** Who this browser joined `workspace`'s host as, and in which workspace, or `null`. */
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
 * Remember that this browser is in `workspace` at the host at `origin` as
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

/** Resolves after the backoff before try `attempt` (`reconnect.mjs`), or the
 * least wait the door asked for (`retryAfter`, seconds), whichever is longer;
 * rejects once `signal` aborts. */
const backOff = ({attempt, retryAfter = null, signal}) => new Promise((resolve, reject) => {
  signal?.throwIfAborted();
  const asked = Number.isFinite(retryAfter) && retryAfter > 0 ? retryAfter * 1000 : 0;
  const timer = setTimeout(resolve, Math.max(backoffDelay(attempt), asked));
  signal?.addEventListener('abort', () => { clearTimeout(timer); reject(signal.reason); }, {once: true});
});

/** This browser's device key for `profile`: the first one no other tab
 * holds, a new one when they all are (`device-custody.mjs`). */
const browserCustody = profile => openDeviceCustody({profile, storage: idbStorage()});

/**
 * Join `workspace` as `identity` (the person, created and kept by the
 * caller) through its host's admission door; resolves to the peer. When
 * the host was set up again since this browser joined it, the person joins
 * the new one as themselves: nothing about hosts is theirs to decide, and
 * what this browser kept for the previous host stays on this device in a
 * namespace of its own.
 *
 * Each tab is a device of its own (`openCustody`, one device key per tab
 * open at once): a tab that takes a key no host has seen yet is a new
 * device of the person. The workspace admits each device once, so a new device
 * of a person whose root this page holds is admitted through the door; one
 * of a person paired from elsewhere pairs first, approved on one of their
 * devices, as on any new browser.
 */
export async function joinViaHost({
  workspace, identity, displayName = defaultDisplayName(identity?.userPublicKey), fetch = globalThis.fetch, memberships = globalThis.localStorage,
  join = joinWorkspace, connect = connectPeer, pair = pairDevice, openCustody = browserCustody, renew = renewDevice,
  wait = backOff, onProgress, ...connectOptions
}) {
  admissionDoor(workspace);
  const known = keptMembership(workspace, memberships);
  /** The host's door, for the person `userPublicKey`. */
  const door = userPublicKey => ask({workspace, path: 'join', fetch, body: {userPublicKey, displayName: fitDisplayName(displayName)}});
  if (known?.paired) {
    // A device paired to a person kept elsewhere: only that person's
    // devices can let it in again, so a failure is the page's to show.
    const profile = identityProfile(known, known.origin);
    const custody = await openCustody(profile);
    const common = {...connectOptions, origin: known.origin, workspace: known.room, userPublicKey: known.userPublicKey, profile, custody, displayName};
    return withCustody(custody, () => (custody.created
      // A tab with a key of its own: a new device of that person, which
      // only their root can certify. It pairs as a new browser would.
      ? pair({...common, admission: () => door(known.userPublicKey)})
      : connect(common)), connectOptions.signal);
  }
  if (!identity?.userPublicKey) throw new TypeError("joining through a host needs the person's identity");
  /** Ask the door to admit the person, then join with what it hands out.
   * `custody` (this tab's device key for `known`'s host) goes on where the
   * door names the same host. `refused`: the host refused the membership
   * this browser kept. `fresh`: join as a new device of the person. */
  const throughDoor = async (custody, refused, fresh) => {
    let admitted;
    try {
      admitted = await door(identity.userPublicKey);
    } catch (error) {
      // The host no longer has the person, and its door turns away the
      // address they joined from because someone was removed: they were taken
      // out of the workspace, not pruned after an absence. Only an invitation
      // brings them back. A door that names the person removed says so
      // whatever this browser kept.
      if (error?.code === 'removed' || (refused && error?.code === 'cooling-down')) {
        throw new AdmissionError('removed', removedFrom(workspace.name));
      }
      throw error;
    }
    // A door that names no origin is its workspace's own (`directory.rs`
    // in the relay's protocol): that is the host this page asked.
    const {invitation, admissionToken, origin = workspace.host.origin} = admitted;
    const sameHost = custody && known && originOf(origin) === originOf(known.origin);
    if (custody && !sameHost) await custody.close();
    let device = sameHost ? custody : null;
    if (fresh) {
      const profile = identityProfile(identity, origin);
      device = await renew(device ?? await openCustody(profile), profile);
    }
    let admittedAt = null;
    const joining = () => join({
      ...connectOptions, origin, invitation, admissionToken, identity, displayName, ...(device ? {custody: device} : {}),
      onProgress: event => { if (event.phase === 'admitted') admittedAt = event.room; onProgress?.(event); },
    });
    const peer = fresh ? await withCustody(device, joining, connectOptions.signal) : await joining();
    keepMembership(memberships, workspace, {origin, room: admittedAt ?? peer.workspace, userPublicKey: identity.userPublicKey});
    return peer;
  };
  /** Join through the door until the host lets the person in. The door
   * handed out the invitation a moment before, so a refusal is about what
   * this browser brings: a device key the host knows under a certificate
   * this browser no longer holds (its storage was cleared, or a join the
   * host admitted never finished here) is never admitted under another, so
   * the person comes again as a new device of theirs. A person the workspace
   * bans is told so; a crowd at the door (the workspace is full, or this
   * address asks too often) is waited out, as long as the door asked. Both
   * pass through `wait` (the reconnect screen, with the backoff and the
   * door's own `Retry-After`), so the page says where it stands and tries
   * again rather than stopping at an error. */
  const admitAnew = async (custody, refused = false) => {
    let held = custody;
    let since = null;
    /** How many times this tab has come again as a new device. */
    let renewals = 0;
    /** How many times the page has waited. */
    let waits = 0;
    for (;;) {
      try {
        return await throughDoor(held, refused, renewals > 0);
      } catch (error) {
        if (error?.code === 'refused') {
          if (/banned/.test(error.reason ?? '')) {
            throw Object.assign(new AdmissionError('removed', removedFrom(workspace.name)), {reason: error.reason});
          }
          // The first new device took this tab's key in place of the old one.
          if (renewals > 0) held = null;
          renewals += 1;
          console.warn('host admission: the host refused the door\'s invitation, joining as a new device', error.reason);
          // The first new device goes at once; later ones after a wait.
          if (renewals === 1) continue;
        } else if (!['full', 'too-many'].includes(error?.code)) {
          throw error;
        }
        since ??= Date.now();
        await wait({attempt: waits, error, retryAfter: error.retryAfter ?? null, since, signal: connectOptions.signal});
        waits += 1;
      }
    }
  };
  if (known?.userPublicKey !== identity.userPublicKey) return admitAnew();
  // Other tabs of this browser may hold every device key it has for this
  // host (`custody-busy`): nothing the host refused, so the membership stays
  // and the door is not asked, which would spend a seat to fail the same way.
  const custody = await openCustody(identityProfile(identity, known.origin));
  return withCustody(custody, async () => {
    // A key no host has seen yet is a new device of the person: the workspace
    // admits it through the door, as it admitted the person's first.
    if (custody.created) return admitAnew(custody);
    let peer;
    try {
      peer = await connect({...connectOptions, origin: known.origin, workspace: known.room, identity, custody, displayName});
      // A host refuses a membership it no longer has once the session
      // starts, not when connecting: wait for its answer here.
      await peer?.whenReady?.();
      return peer;
    } catch (error) {
      await peer?.dispose?.().catch(() => {});
      // The host has not answered yet, or is not answering at all (it is
      // restarting): nothing the host refused, so the membership stays, and
      // joining again would spend a seat and a join to fail the same way.
      if (error?.name === 'CustodyError' || error?.name === 'TimeoutError' || hostUnreachable(error)) throw error;
      if (error?.code === 'host-changed') {
        // Another host answers at this origin now: join it as the same
        // person. What this browser kept stays pinned to the previous host.
        console.info('host admission: the host was set up again, joining it anew');
        keepMembership(memberships, workspace, null);
        return admitAnew(custody);
      }
      // The membership is gone (pruned after a long absence, or the host
      // started its workspace anew), or this device was never admitted: ask the
      // host to admit the same person again rather than stop at an error.
      console.warn('host admission: reconnecting failed, asking to join again', error);
      keepMembership(memberships, workspace, null);
      return admitAnew(custody, true);
    }
  }, connectOptions.signal);
}

/**
 * Become a new device of `userPublicKey` (hex), the person whose root
 * approves it on another of their devices, in `workspace` at the host at
 * `origin`, answering the pairing link's `invite` (hex, or `null`).
 * `onPairingCode(code)` is called with the code that device shows the
 * person; resolves to the peer once approved. Where that host
 * lets people in through its door (`workspace.admission`), the new device
 * joins the person's workspace there as theirs. The host is remembered as
 * joined, paired, for the next visit (`joinViaHost`).
 */
export async function pairViaHost({
  origin, room, userPublicKey, workspace = null, label = 'Browser', displayName = defaultDisplayName(userPublicKey),
  memberships = globalThis.localStorage, fetch = globalThis.fetch, pair = pairDevice, ...connectOptions
}) {
  if (!/^[0-9a-f]{64}$/.test(userPublicKey ?? '')) throw new HostAdmissionError('invalid', 'This pairing link names no person');
  if (connectOptions.invite != null && !/^[0-9a-f]{32}$/.test(connectOptions.invite)) {
    throw new HostAdmissionError('invalid', 'This pairing link is damaged');
  }
  const admission = workspace?.admission
    ? () => ask({workspace, path: 'join', fetch, body: {userPublicKey, displayName: fitDisplayName(displayName)}})
    : undefined;
  const peer = await pair({...connectOptions, origin, workspace: room, userPublicKey, label, displayName, fetch, admission,
    profile: identityProfile({userPublicKey}, origin)});
  keepMembership(memberships, {host: {origin}}, {origin, room: peer.workspace ?? room, userPublicKey, paired: true});
  return peer;
}

/**
 * Whether this browser opens the person's own desktop under an entry whose
 * home workspace is `home`. Whom the home's door lets in by itself gets a
 * desktop of their own too; while it takes people by invitation only
 * (`admission` in its presence), or does not say, only a browser some host
 * let in (a membership it keeps: joined, paired or set up) does. Anyone
 * else has nothing here until invited.
 */
export async function ownDesktopOpen(home, {memberships = globalThis.localStorage, fetch = globalThis.fetch} = {}) {
  if (!home?.admission) return true;
  let kept = [];
  try { kept = membershipOrigins(undefined, memberships); } catch { /* No storage: nothing kept. */ }
  if (kept.length) return true;
  const admission = await doorAdmission(home, fetch);
  return typeof admission === 'string' && admission !== 'invite';
}

/** Who `home`'s door admits now, as its presence says it (`open` or
 * `invite`), or `null` when it does not say. */
async function doorAdmission(home, fetch) {
  try {
    const response = await fetch(new URL('presence', home.admission), {credentials: 'omit'});
    return response.ok ? (await response.json())?.admission ?? null : null;
  } catch {
    return null;
  }
}

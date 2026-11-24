/**
 * The workspaces this browser can open, and joining one through its host's
 * own admission.
 *
 * A workspace is data that names its host:
 * `{name, host: {origin, key?}, workspace?, admission?}`. `host.origin` is the
 * HTTPS origin of the server that hosts the workspace, `host.key` the host's root
 * key (hex) once known, `workspace` its id once known (a listed public
 * workspace learns it when admitted), and `admission` the base URL of the
 * host's admission entry, present only when that host lets people in without
 * an invitation. Opening a workspace is the same admission everywhere
 * (`admission.mjs`): an invitation, a member's account, or, for a host that
 * says so, its admission entry. Nothing here is special to any one workspace:
 * which ones a page offers comes from its web host's entry configuration.
 *
 * Joining through the host takes the person's identity from the caller,
 * which owns creating and keeping it: a person who already joined that host
 * reconnects; otherwise the person goes through the entry, by the entry every
 * device takes, on the session itself (`entryRequest` in wasm, `device::entry_request`),
 * with no pairing code. An entry that says no says why in its own code: a
 * workspace that is full is waited out as long as it asked, until it has
 * capacity.
 *
 * A host set up again (a new device key behind the same origin) is not the
 * one this browser kept the person's state for. Nothing kept is reused or
 * handed to it: the person joins it through its entry once, like anyone
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
 * can never open. The host that held one says, too, whether its owner deleted
 * it, to any browser that names it: one it no longer lets in, as a person
 * invited to only that workspace, learns so there and forgets it. Only an
 * answer from the origin itself forgets one: a host that cannot be reached, or
 * fails, says nothing about its workspaces, so they stay, marked as not
 * opening last time (`settleFailedWorkspace`).
 */
import {AdmissionError} from './admission.mjs';
import {connectPeer, fitDisplayName, identityProfile, joinWorkspace, pairDevice, renewDevice, withDeviceKey} from './peer.mjs';
import {idbStorage, openDeviceKey} from './device-key.mjs';
import {forgetEnteredAt, originOf} from './entered-workspaces.mjs';
import {fetchProfile} from './identity.mjs';
import {PAIRING_SAME_PERSON, PAIRING_STOPPED, shownName} from './pairing.mjs';
import {backoffDelay} from './reconnect.mjs';
import {hostUnreachable} from './session-refusal.mjs';

// The workspaces this browser entered are its own store (`entered-workspaces.mjs`):
// offered here as the choices a person switches between.
export {
  chosenWorkspace, chooseWorkspace, enteredWorkspaces, forgetDeletedWorkspace, forgetGoneWorkspaces, hostDeviceOf, markUnavailable,
  nameEntered, rememberEntered, settleFailedWorkspace,
} from './entered-workspaces.mjs';
export {forgetEnteredAt} from './entered-workspaces.mjs';

const MEMBERSHIPS = 'tilo-host-memberships';

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
 * it, else the workspace this browser was let into through its entry
 * (`admittedWorkspace` on this visit, or the membership it kept). Another workspace
 * on the same host is not the listed one, and goes by no name or count of
 * the listed one's.
 */
export function listedAt(workspaces, scope, admittedWorkspace = null, memberships = globalThis.localStorage) {
  if (scope?.kind !== 'workspace') return null;
  const listed = workspaceAt(workspaces, scope.host);
  if (!listed) return null;
  const workspace = listed.workspace ?? admittedWorkspace ?? keptMembership(listed, memberships)?.workspace ?? null;
  return scope.workspace == null || scope.workspace === workspace ? listed : null;
}

/** Host action ids that open one of the person's workspaces. */
export const OPEN_WORKSPACE = 'open-workspace:';

/** Action ids that say the person left the workspace named after the colon: another of theirs
 * opens in its place. */
export const LEFT_WORKSPACE = 'left-workspace:';

/** Whether `scope` (`{host | origin, workspace}`) is the web host's `home`
 * workspace; `homeWorkspace` is the workspace this browser was admitted to through its
 * entry. */
function isHomeWorkspace({home, homeWorkspace = null, scope}) {
  const homeOrigin = home?.host?.origin ? originOf(home.host.origin) : null;
  return homeOrigin !== null && Boolean(scope?.host ?? scope?.origin)
    && originOf(scope.host ?? scope.origin) === homeOrigin
    && (scope.workspace == null || scope.workspace === (home.workspace ?? homeWorkspace));
}

/**
 * The workspaces of the person this browser knows: what it remembers entering
 * (`entered`, in `enteredWorkspaces`'s shape) with the ones their host `listed`
 * for them filled in. The host is the source of truth for which workspaces of
 * theirs exist and what they are called; this browser's memory says where those
 * on other hosts are reached, which a host's name alone does not. A listed
 * workspace names the host that keeps it: one with no signed `hint` is the host
 * that answered (`answering`, its origin), the rest are reached where this
 * browser reached them before. One entry per workspace, the host's name
 * winning and one it lists no longer marked as not opening. A listing also says
 * the host's own name (`place`: a site's name, or the label of the person's
 * computer that keeps it) and the part the person takes in the workspace
 * (`role`) and when it was made (`madeAt`, ms since the epoch), which are what
 * the switcher tells two workspaces of one name apart by: never the address
 * this browser dialled.
 */
export function knownWorkspaces({entered = [], listed = [], answering = null} = {}) {
  const known = entered.map(entry => ({...entry}));
  if (!Array.isArray(listed)) return known;
  for (const workspace of listed) {
    if (typeof workspace?.workspace !== 'string' || !workspace.workspace) continue;
    const at = known.findIndex(entry => entry.workspace === workspace.workspace);
    // A host that answered names no address for a workspace it keeps itself:
    // the page reaches it where it is. Another host's name opens nothing, so
    // only one this browser reached before is offered.
    const here = Boolean(workspace.host) && !workspace.host.hint;
    const origin = here ? answering : (at >= 0 ? known[at].origin : null);
    if (!origin) continue;
    const name = typeof workspace.name === 'string' && workspace.name ? workspace.name : null;
    const place = typeof workspace.host?.name === 'string' && workspace.host.name ? workspace.host.name : null;
    const role = typeof workspace.role === 'string' && workspace.role ? workspace.role : null;
    // When the server that keeps it made it, ms since the epoch: a workspace
    // whose server did not say keeps none it was told before.
    const madeAt = Number.isFinite(workspace.made_at_ms) ? workspace.made_at_ms : (at >= 0 ? known[at].madeAt ?? null : null);
    if (at >= 0) known[at] = {...known[at], origin: originOf(origin), ...(name ? {name} : {}), place, role, madeAt, unavailable: false};
    else known.push({origin: originOf(origin), workspace: workspace.workspace, name, place, role, madeAt, unavailable: false});
  }
  return known;
}

/**
 * The workspaces the person can switch to from `current` (a route scope, or
 * `null`): the web host's `home` workspace, then the workspaces they `entered`
 * or their host `listed` (`knownWorkspaces`), never the one in front.
 * `homeWorkspace` is the workspace this browser was admitted to through the
 * home's entry, so that workspace is offered once, as the home.
 * Where the person may have a desktop of their own (`own`), it comes first,
 * so a workspace is never a dead end. It is no workspace of theirs: the
 * desktop's menu offers it above them, as the person's own place, and never
 * in the list of workspaces it switches between.
 * Each choice is
 * `{id, name, origin, workspace, home, own, unavailable, place, role, madeAt}`;
 * `id` is its host action id, `place` the host's name as people know it,
 * `role` the part the person takes in it and `madeAt` when it was made (ms
 * since the epoch), where the host said either.
 */
export function workspaceChoices({home = null, homeWorkspace = null, entered = [], listed = [], answering = null, current = null, own = false}) {
  const here = current?.kind === 'workspace' ? current : null;
  const homeOrigin = home?.host?.origin ? originOf(home.host.origin) : null;
  const isHome = scope => isHomeWorkspace({home, homeWorkspace, scope});
  const choices = [];
  if (own && current?.kind !== 'own') {
    choices.push({id: `${OPEN_WORKSPACE}own`, name: 'Your desktop', origin: null, workspace: null, home: false, own: true, unavailable: false, place: null, role: null, madeAt: null});
  }
  if (homeOrigin && !(here && isHome(here))) {
    choices.push({id: `${OPEN_WORKSPACE}home`, name: home.name || new URL(homeOrigin).host, origin: homeOrigin,
      workspace: home.workspace ?? null, home: true, own: false, unavailable: false, place: null, role: null, madeAt: null});
  }
  for (const entry of knownWorkspaces({entered, listed, answering})) {
    if (isHome({host: entry.origin, workspace: entry.workspace}) || (here && originOf(here.host) === entry.origin && here.workspace === entry.workspace)) continue;
    choices.push({id: `${OPEN_WORKSPACE}${entry.origin}/${entry.workspace}`, name: entry.name || new URL(entry.origin).host,
      origin: entry.origin, workspace: entry.workspace, home: false, own: false, unavailable: entry.unavailable,
      place: entry.place ?? null, role: entry.role ?? null, madeAt: entry.madeAt ?? null});
  }
  return choices;
}

/**
 * The name a workspace is made under when the person asks for `name`: the name
 * itself, or, when it is already taken, numbered past the names `held` (the
 * switcher's own workspaces, the one in front included) — the rule the shell
 * gives the workspaces it makes (`workspace_names`), so two workspaces of one
 * name never stand side by side in the bar and the menu, where a person picks
 * one by its name.
 */
export function freeName(name, held = []) {
  const wanted = String(name ?? '').trim();
  const taken = new Set(held.filter(other => typeof other === 'string' && other));
  if (!taken.has(wanted)) return wanted;
  for (let number = 2; ; number += 1) {
    const candidate = `${wanted} ${number}`;
    if (!taken.has(candidate)) return candidate;
  }
}

/** A place as a person reads it: a site's name (`tilo.sh`) or the label of their
 * computer, without the port or the address this browser dialled — an address
 * (`127.0.0.1`, `[::1]`, `localhost`) is nobody's name for their workspace. */
function asPlace(text) {
  const value = typeof text === 'string' ? text.trim().replace(/:\d+$/, '') : '';
  if (!value || value === 'localhost' || value.includes(':') || /^[0-9.]+$/.test(value)) return null;
  return value;
}

const namedOrigin = origin => {
  try { return asPlace(new URL(origin).hostname); } catch { return null; }
};

/** What a workspace is told apart by when another of its person's is named the
 * same: where the host keeps it, as people know that host (`tilo.sh`, the label
 * of their computer), else whose it is to the person reading. Never the
 * address this browser dialled. */
function placeOf(choice) {
  return asPlace(choice.place) ?? (choice.home ? 'this site' : namedOrigin(choice.origin));
}

/** Whose a workspace is, as a person reads it: theirs, or the part they hold. */
const roleWord = role => ({owner: 'yours', editor: 'editor', commenter: 'commenter',
  moderator: 'moderator', viewer: 'viewer'}[role] ?? null);

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

const sameDay = (at, now) => at.getFullYear() === now.getFullYear()
  && at.getMonth() === now.getMonth() && at.getDate() === now.getDate();

/** The day `ms` was, as a person reads it: "today", "3 Nov", or "3 Nov 2024"
 * when it was not this year. */
function dayOf(ms, now) {
  const at = new Date(ms);
  if (sameDay(at, now)) return 'today';
  const day = `${at.getDate()} ${MONTHS[at.getMonth()]}`;
  return at.getFullYear() === now.getFullYear() ? day : `${day} ${at.getFullYear()}`;
}

/** The clock time `ms` was, as a person reads it: "14:02". */
const clockOf = ms => {
  const at = new Date(ms);
  return `${String(at.getHours()).padStart(2, '0')}:${String(at.getMinutes()).padStart(2, '0')}`;
};

/**
 * The host actions that open `choices` (`workspaceChoices`), for desktop
 * search. The workspace menu lists each by name beside the one in front
 * (named `current`), so two that share a name are told apart by something a
 * person knows: the host it is on as people name it, whose it is to them, or
 * when it was made. The person's own desktop is no workspace: its action is
 * of its own kind (`desktop`), which the desktop offers apart from them.
 */
export function workspaceActions(choices, {current = null, now = Date.now()} = {}) {
  const today = new Date(now);
  const names = [current, ...choices.map(choice => choice.name)].filter(Boolean);
  const shared = name => names.filter(other => other === name).length > 1;
  const named = choice => choices.filter(other => other.name === choice.name);
  /** A place that only this one of the same-named workspaces is on: a place
   * two of them share tells them apart from nobody. */
  const ownPlace = choice => {
    const place = placeOf(choice);
    return place && named(choice).filter(other => placeOf(other) === place).length === 1 ? place : null;
  };
  /** Whose it is, told only where no other of the same name is whose in the
   * same way: two workspaces of one person's are both "yours". */
  const ownRole = choice => {
    const word = roleWord(choice.role);
    return word && named(choice).filter(other => roleWord(other.role) === word).length === 1 ? word : null;
  };
  /** When it was made, told only where no other of the same name was made
   * alike: those made the same day read by the time of day too. */
  const madeTell = choice => {
    if (!Number.isFinite(choice.madeAt)) return null;
    const day = dayOf(choice.madeAt, today);
    const others = named(choice).filter(other => other !== choice && Number.isFinite(other.madeAt));
    const withTime = others.some(other => dayOf(other.madeAt, today) === day);
    const when = other => `${dayOf(other, today)}${withTime ? ` ${clockOf(other)}` : ''}`;
    return others.some(other => when(other.madeAt) === when(choice.madeAt)) ? null : `made ${when(choice.madeAt)}`;
  };
  return choices.map(choice => {
    if (choice.own) {
      return {id: choice.id, title: 'Open your desktop', label: choice.name, source: 'desktop', icon: 'user',
        subtitle: 'Your own desktop, in this browser'};
    }
    const place = placeOf(choice);
    const tell = ownPlace(choice) ?? ownRole(choice) ?? madeTell(choice);
    const label = !shared(choice.name) || !tell ? choice.name : `${choice.name} · ${tell}`;
    // `label` and `source`: the desktop's workspace menu lists it by name; the
    // source its own action kind, so the person's desktop is offered apart.
    const made = Number.isFinite(choice.madeAt) ? `made ${dayOf(choice.madeAt, today)}` : null;
    const about = ['Workspace', place, made, ...(choice.unavailable ? ['did not open last time'] : [])].filter(Boolean).join(' · ');
    return {id: choice.id, title: `Open ${label}`, label, source: 'workspaces', subtitle: about};
  });
}

/**
 * The workspaces this page holds, as the directory sheet shows them
 * (`mountDesktop({directory: {held}})`, `Held` in the runtime): each by the
 * workspace id a site lists it as, and by the host action id that opens it.
 * `current` (the scope in front) comes too, so the workspace the page is in
 * says Open rather than Join.
 */
export function heldWorkspaces({entered = [], listed = [], answering = null, current = null} = {}) {
  const held = [];
  const seen = new Set();
  const add = (origin, workspace, name) => {
    if (!origin || !workspace || seen.has(workspace)) return;
    seen.add(workspace);
    const at = originOf(origin);
    held.push({workspace, key: `${OPEN_WORKSPACE}${at}/${workspace}`, name: name || new URL(at).host});
  };
  if (current?.kind === 'workspace') add(current.host, current.workspace, current.name ?? null);
  for (const entry of knownWorkspaces({entered, listed, answering})) add(entry?.origin, entry?.workspace, entry?.name ?? null);
  return held;
}

/**
 * Join the workspace the person chose in a site's directory
 * (`mountDesktop({onJoinGranted})`): through its `entry` (`…/directory/<workspace>/`),
 * which the page enters on the session it opens with the entry's host at
 * `origin`, the same entry the page's own entry takes; or with the
 * `invitation` a pasted link brought. Either goes through the page's join
 * (`join`, its `joinWorkspace`), then the workspace shows. Resolves to
 * `{peer, origin, workspace, name}`.
 */
export async function joinGranted({origin, entry = null, invitation = null, workspace = null, name = null}, {join, displayName = null, ...connectOptions} = {}) {
  if (!origin || !(entry || invitation)) throw new TypeError('a granted join names the origin of its host, and its entry or its invitation');
  if (typeof join !== 'function') throw new TypeError('a granted join needs the page\'s join');
  const way = entry ? {entry} : {invitation};
  const peer = await join({...connectOptions, origin, ...way, ...(displayName ? {displayName} : {})});
  const id = peer?.workspace ?? workspace;
  return {peer, origin: originOf(origin), workspace: id, name: name || id};
}

/** What a person the host took out of a workspace is told. */
export const removedFrom = name => `You were removed from ${name || 'this workspace'}. To come back, ask someone in it for a new invitation.`;

export class HostAdmissionError extends Error {
  /** `entry`: what the entry said beside its refusal: `{waitList, contact}`
   * from a workspace taking people by invitation that keeps a wait list. */
  constructor(code, message, retryAfter = null, entry = null) {
    super(message);
    this.name = 'HostAdmissionError';
    this.code = code;
    this.retryAfter = retryAfter;
    this.entry = entry;
  }
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

function admissionEntry(workspace) {
  if (!workspace?.admission) throw new HostAdmissionError('invitation-only', `${workspace?.name ?? 'This workspace'} needs an invitation`);
  return workspace.admission;
}

/**
 * The public workspaces the server at `origin` lists in its directory
 * (`<origin>/peer/v1/directory`, `DirectoryPage`): each as
 * `{workspace, name, entry, invitationOnly, waitList}`, its `entry` the
 * address of its own entry. Whether a workspace lets anyone in, and the wait
 * list an invitation-only one offers (`{contact, mail}`), as its entry would
 * answer, before anyone asks it. Rejects when the directory is not read.
 */
export async function publicListings(origin, {fetch = globalThis.fetch} = {}) {
  const directory = new URL('/peer/v1/directory', origin).href;
  const response = await fetch(directory, {method: 'POST', credentials: 'omit'});
  if (!response.ok) throw new Error(`the directory answered ${response.status}`);
  const page = await response.json();
  return (Array.isArray(page?.workspaces) ? page.workspaces : [])
    .filter(listed => typeof listed?.workspace === 'string' && typeof listed?.entry === 'string')
    .map(listed => ({
      workspace: listed.workspace,
      name: typeof listed.name === 'string' && listed.name ? listed.name : null,
      entry: new URL(listed.entry, directory).href,
      invitationOnly: listed.invitationOnly === true,
      waitList: listed.waitList && typeof listed.waitList === 'object'
        ? {contact: typeof listed.waitList.contact === 'string' && listed.waitList.contact ? listed.waitList.contact : null,
          mail: listed.waitList.mail === true}
        : null,
    }));
}

/** The workspace a listing's entry admits into, when it names one, or its
 * entry's address does (`<server>/peer/v1/directory/<workspace>/`, a site's
 * home); else `null`. */
export function entryWorkspace(workspace) {
  if (typeof workspace?.workspace === 'string' && workspace.workspace) return workspace.workspace;
  return (typeof workspace?.admission === 'string' && workspace.admission.match(/\/directory\/([^/]+)\/?$/)?.[1]) || null;
}

/** An entry's refusal as the shared entry read it (`DeviceRefusal`: the entry's
 * code and the wait it asked for), as this page's screens tell them apart: a
 * ban and a removal alike are `removed`; an entry not reached is `unavailable`.
 * Anything else stands. */
function entryRefused(error) {
  if (error?.name !== 'DeviceRefusal') return error;
  const code = {banned: 'removed', removed: 'removed', 'invitation-only': 'invitation-only', full: 'full', unanswered: 'unavailable'}[error.code] ?? 'refused';
  // A workspace taking people by invitation may keep a wait list, and name
  // where to ask to be taken off it: the entry's own words, as the shared
  // entry read them (`DeviceRefusal.entry`).
  const entry = code === 'invitation-only' && error.entry?.waitList === true
    ? {waitList: true, contact: typeof error.entry.contact === 'string' && error.entry.contact ? error.entry.contact : null, mail: error.entry.mail === true} : null;
  return new HostAdmissionError(code, error.message, Number.isFinite(error.retryAfter) && error.retryAfter > 0 ? error.retryAfter : null, entry);
}

/** Why the entry kept no address, by its `code`: the cases an entry page says. */
const WAIT_CODES = new Set(['invalid-email', 'invalid-secret', 'too-many', 'wait-list-full', 'no-wait-list']);

/** Where this browser keeps what it left on hosts' wait lists: per entry
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
    // the contact the entry names still takes it off.
  }
};

/** The entry `POST`ed to, as JSON, without credentials; `HostAdmissionError`
 * `unavailable` when it cannot be reached. */
async function postToEntry(fetch, url, body, what) {
  try {
    return await fetch(url, {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(body), credentials: 'omit'});
  } catch (error) {
    throw new HostAdmissionError('unavailable', `${what}: ${error?.message ?? error}`);
  }
}

async function entryRefusal(response, what) {
  const reply = await response.json().catch(() => ({}));
  const retry = Number(response.headers.get('retry-after'));
  const code = WAIT_CODES.has(reply.code) ? reply.code : 'unavailable';
  return new HostAdmissionError(code, reply.error ?? `${what}: HTTP ${response.status}`,
    Number.isFinite(retry) && retry > 0 ? retry : null);
}

/**
 * Leave `email` at `workspace`'s entry, to be invited at (`POST
 * <admission>wait-list`), in the reader's `language`: a workspace taking people
 * by invitation keeps it for whoever may invite. An entry that mails sends a
 * link that confirms it, and lists it only then. The secret the entry hands
 * back, which takes the address off again (`leaveWaitList`), is kept in
 * `storage` for that entry. Resolves to `{email, mail}`: the address as the
 * entry keeps it (as typed, trimmed), and whether it mailed a link to
 * confirm; rejects with `HostAdmissionError` code `invalid-email`,
 * `too-many` (with `retryAfter`), `wait-list-full`, `no-wait-list` or
 * `unavailable`.
 */
export async function leaveOnWaitList({workspace, email, language = globalThis.navigator?.language ?? '',
  fetch = globalThis.fetch, storage = globalThis.localStorage}) {
  const entry = new URL('wait-list', admissionEntry(workspace));
  const response = await postToEntry(fetch, entry, {email: String(email ?? ''), language: String(language)}, 'wait list');
  if (!response.ok) throw await entryRefusal(response, 'wait list');
  const reply = await response.json().catch(() => ({}));
  if (typeof reply.email !== 'string') throw new HostAdmissionError('unavailable', 'wait list: the entry named no address');
  const mail = reply.mail === true;
  if (typeof reply.removal === 'string' && SECRET.test(reply.removal)) {
    writeListings(storage, {...readListings(storage), [entry.origin]: {email: reply.email, secret: reply.removal, mail}});
  }
  return {email: reply.email, mail};
}

/** A wait-list secret, as the entry hands it out and its links carry it. */
const SECRET = /^[0-9a-f]{64}$/;

/** What this browser left on `workspace`'s wait list and can take off
 * again: `{email, pending}` (`pending`: the entry mailed a link to confirm
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
 * either way. The entry answers alike whether or not it still listed the
 * address, so this resolves either way; it rejects with
 * `HostAdmissionError` code `too-many` (with `retryAfter`) or
 * `unavailable`, and keeps the secret to try again. A secret the entry
 * calls none is forgotten.
 */
export async function leaveWaitList({workspace, secret = null, fetch = globalThis.fetch, storage = globalThis.localStorage}) {
  const entry = new URL('wait-list/remove', admissionEntry(workspace));
  const sent = secret ?? readListings(storage)[entry.origin]?.secret;
  if (typeof sent !== 'string' || !SECRET.test(sent)) {
    if (secret === null) return;
    throw new HostAdmissionError('invalid-secret', 'leaving the wait list: the link carries no secret');
  }
  const response = await postToEntry(fetch, entry, {secret: sent}, 'leaving the wait list');
  const refusal = response.ok ? null : await entryRefusal(response, 'leaving the wait list');
  if (!refusal || refusal.code === 'invalid-secret' || refusal.code === 'no-wait-list') {
    forgetListing(storage, entry.origin);
    return;
  }
  throw refusal;
}

/**
 * Confirm the address whose confirmation link carried `token` (`POST
 * <admission>wait-list/confirm`): whether the entry still had it waiting
 * (`false`: it was taken off, or not confirmed within two days). Rejects
 * with `HostAdmissionError` as `leaveWaitList` does.
 */
export async function confirmWaitList({workspace, token, fetch = globalThis.fetch, storage = globalThis.localStorage}) {
  const entry = new URL('wait-list/confirm', admissionEntry(workspace));
  if (typeof token !== 'string' || !SECRET.test(token)) return false;
  const response = await postToEntry(fetch, entry, {token}, 'confirming the wait list');
  if (!response.ok) {
    const refusal = await entryRefusal(response, 'confirming the wait list');
    if (refusal.code === 'invalid-secret') return false;
    throw refusal;
  }
  const reply = await response.json().catch(() => ({}));
  const listings = readListings(storage);
  if (reply.confirmed === true && listings[entry.origin]) {
    // This browser left it: it no longer waits to be confirmed.
    writeListings(storage, {...listings, [entry.origin]: {...listings[entry.origin], confirmed: true}});
  }
  return reply.confirmed === true;
}

/**
 * Where what this browser left on `workspace`'s wait list stands (`POST
 * <admission>wait-list/status`): `pending` (to be confirmed), `listed`,
 * `invited` or `gone` (then forgotten here); `null` when nothing is kept
 * here or the entry cannot say now.
 */
export async function waitListStatus({workspace, fetch = globalThis.fetch, storage = globalThis.localStorage}) {
  const entry = new URL('wait-list/status', admissionEntry(workspace));
  const kept = readListings(storage)[entry.origin];
  if (typeof kept?.secret !== 'string') return null;
  let response;
  try {
    response = await postToEntry(fetch, entry, {secret: kept.secret}, 'the wait list');
  } catch {
    return null;
  }
  if (!response.ok) return null;
  const {status} = await response.json().catch(() => ({}));
  if (!['pending', 'listed', 'invited', 'gone'].includes(status)) return null;
  if (status === 'gone') forgetListing(storage, entry.origin);
  return status;
}

/** The memberships this browser keeps, by host origin. One that names no workspace field at all
 * (not even `null`) is not one this page keeps, and is left out rather than opened half-read. */
const readMemberships = store => {
  let all;
  try { all = JSON.parse(store?.getItem(MEMBERSHIPS) ?? '{}') ?? {}; } catch { return {}; }
  if (typeof all !== 'object' || Array.isArray(all)) return {};
  return Object.fromEntries(Object.entries(all)
    .filter(([, kept]) => kept && typeof kept === 'object' && 'workspace' in kept
      && (kept.workspace === null || typeof kept.workspace === 'string')));
};

/** Who this browser joined `workspace`'s host as, and in which workspace, or `null`. */
export function keptMembership(workspace, memberships = globalThis.localStorage) {
  return readMemberships(memberships)[originOf(workspace.host.origin)] ?? null;
}

/** The origins of the hosts this browser joined through, as `userPublicKey`
 * when given. */
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
 * it keeps the person's profile.
 */
export function keepOwnedHost({origin, userPublicKey}, memberships = globalThis.localStorage) {
  const host = {host: {origin}};
  const kept = keptMembership(host, memberships);
  keepMembership(memberships, host, {
    origin: originOf(origin), workspace: kept?.userPublicKey === userPublicKey ? kept.workspace ?? null : null, userPublicKey, owner: true,
  });
}

/**
 * Remember that this browser is in `workspace` at the host at `origin` as
 * `userPublicKey`, whose root it holds (joined through an invitation, or a
 * workspace it created on a host it owns), so a later visit reconnects.
 * The workspace kept is the first this person entered
 * at that host, the one its entry let them into (`listedAt` names the host's
 * listed workspace by it): joining another of the host's workspaces, by an
 * invitation or by making one, adds that one to the entered workspaces
 * (`rememberEntered`) and leaves it as it is.
 */
export function keepJoinedHost({origin, workspace, userPublicKey}, memberships = globalThis.localStorage) {
  const host = {host: {origin}};
  const kept = keptMembership(host, memberships);
  const same = kept?.userPublicKey === userPublicKey;
  const owner = Boolean(kept?.owner && same);
  keepMembership(memberships, host, {origin: originOf(origin), workspace: (same && kept.workspace) || workspace, userPublicKey, ...(owner ? {owner} : {})});
}

/** The origins of the hosts this browser set up for `userPublicKey`. */
export function ownedHosts(userPublicKey, memberships = globalThis.localStorage) {
  return Object.values(readMemberships(memberships))
    .filter(kept => kept?.owner && kept.origin && kept.userPublicKey === userPublicKey)
    .map(kept => kept.origin);
}

/**
 * Undo the mark a browser was left with when it was paired to the person it
 * already is: a pairing link opened in the browser that made it, before
 * `pairViaHost` refused that. Every host kept as `paired` for the person
 * whose root this browser keeps (`kept()`: `{rootPublicKey}` or `null`,
 * `keptIdentity`) is kept as joined again; resolves to the origins changed.
 *
 * A browser that keeps a person's root is no device of theirs whose root is
 * elsewhere. Marked as one it left that root unused, so it could let no
 * further device in. A device paired to anyone else stays as it is, and a
 * browser with no paired host is not asked whom it keeps.
 */
export async function unmarkOwnPairing(kept, memberships = globalThis.localStorage) {
  const marked = Object.values(readMemberships(memberships)).some(host => host?.paired);
  if (!marked) return [];
  const holds = (await kept())?.rootPublicKey ?? null;
  if (!/^[0-9a-f]{64}$/.test(holds ?? '')) return [];
  // Read again: the person was asked for in between.
  const all = readMemberships(memberships);
  const own = Object.keys(all).filter(origin => all[origin]?.paired && all[origin].userPublicKey === holds);
  for (const origin of own) {
    const joined = {...all[origin]};
    delete joined.paired;
    all[origin] = joined;
  }
  if (own.length) memberships?.setItem(MEMBERSHIPS, JSON.stringify(all));
  return own;
}

/** Resolves after the backoff before try `attempt` (`reconnect.mjs`), or the
 * least wait the entry asked for (`retryAfter`, seconds), whichever is longer;
 * rejects once `signal` aborts. */
const backOff = ({attempt, retryAfter = null, signal}) => new Promise((resolve, reject) => {
  signal?.throwIfAborted();
  const asked = Number.isFinite(retryAfter) && retryAfter > 0 ? retryAfter * 1000 : 0;
  const timer = setTimeout(resolve, Math.max(backoffDelay(attempt), asked));
  signal?.addEventListener('abort', () => { clearTimeout(timer); reject(signal.reason); }, {once: true});
});

/** This tab's device key for `profile`: the one it held before a reload,
 * else a free one this browser has, a new one when other tabs hold them all
 * (`device-key.mjs`). */
const browserDeviceKey = profile => openDeviceKey({profile, storage: idbStorage()});

/**
 * Join `workspace` as `identity` (the person, created and kept by the
 * caller) through its host's admission entry; resolves to the peer. When
 * the host was set up again since this browser joined it, the person joins
 * the new one as themselves: nothing about hosts is theirs to decide, and
 * what this browser kept for the previous host stays on this device in a
 * namespace of its own.
 *
 * Each tab is a device of its own (`openDeviceKey`, one device key per tab
 * open at once): a tab that takes a key no host has seen yet is a new
 * device of the person. The workspace admits each device once, so a new device
 * of a person whose root this page holds is admitted through the entry; one
 * of a person paired from elsewhere pairs first, approved on one of their
 * devices, as on any new browser.
 */
export async function joinViaHost({
  workspace, identity, displayName = null, memberships = globalThis.localStorage,
  join = joinWorkspace, connect = connectPeer, pair = pairDevice, openDeviceKey = browserDeviceKey, renew = renewDevice,
  wait = backOff, onProgress, ...connectOptions
}) {
  const entry = admissionEntry(workspace);
  const known = keptMembership(workspace, memberships);
  // The workspace this listing's entry admits into (`entryWorkspace`). A
  // membership kept for another of the host's workspaces is not this one's:
  // a person invited into that one comes to the home through its entry. Only
  // an entry that names none is the membership's workspace.
  const asking = entryWorkspace({...workspace, admission: entry});
  const forThisWorkspace = !asking || known?.workspace === asking;
  if (known?.paired) {
    // A device paired to a person kept elsewhere: only that person's
    // devices can let it in again, so a failure is the page's to show.
    const profile = identityProfile(known, known.origin);
    const deviceKey = await openDeviceKey(profile);
    const common = {...connectOptions, origin: known.origin, workspace: asking ?? known.workspace, userPublicKey: known.userPublicKey, profile, deviceKey, displayName};
    return withDeviceKey(deviceKey, () => (deviceKey.created
      // A tab with a key of its own: a new device of that person, which
      // only their root can certify. It pairs as a new browser would.
      ? pair({...common, entry}).catch(error => { throw entryRefused(error); })
      : connect(common)), connectOptions.signal);
  }
  if (!identity?.userPublicKey) throw new TypeError("joining through a host needs the person's identity");
  /** Ask the entry to admit the person, then join with what it hands out.
   * `device key` (this tab's device key for `known`'s host) goes on where the
   * entry names the same host. `refused`: the host refused the membership
   * this browser kept. `fresh`: join as a new device of the person. */
  const throughEntry = async (deviceKey, refused, fresh) => {
    // The entry admits into its workspace's own host (`directory.rs` in the
    // server's protocol).
    const origin = workspace.host.origin;
    const sameHost = deviceKey && known && originOf(origin) === originOf(known.origin);
    if (deviceKey && !sameHost) await deviceKey.close();
    let device = sameHost ? deviceKey : null;
    if (fresh) {
      const profile = identityProfile(identity, origin);
      device = await renew(device ?? await openDeviceKey(profile), profile);
    }
    let admittedAt = null;
    const joining = () => join({
      ...connectOptions, origin, entry, identity, displayName, ...(device ? {deviceKey: device} : {}),
      onProgress: event => { if (event.phase === 'admitted') admittedAt = event.workspace; onProgress?.(event); },
    });
    let peer;
    try {
      peer = fresh ? await withDeviceKey(device, joining, connectOptions.signal) : await joining();
    } catch (error) {
      const said = entryRefused(error);
      // The workspace's people took the person out (a ban, or a removal
      // within the day): only an invitation brings them back. An entry that
      // turns away a membership this browser kept says the same.
      if (said?.code === 'removed') throw Object.assign(new AdmissionError('removed', removedFrom(workspace.name)), {reason: said.message});
      throw said;
    }
    // The membership names the workspace the host's entry first let this
    // browser into: entering another of the host's workspaces leaves it.
    const held = keptMembership(workspace, memberships);
    const same = held?.userPublicKey === identity.userPublicKey;
    keepMembership(memberships, workspace, {origin, workspace: (same && held.workspace) || admittedAt || peer.workspace,
      userPublicKey: identity.userPublicKey});
    return peer;
  };
  /** Join through the entry until the host lets the person in. The entry
   * handed out the invitation a moment before, so a refusal is about what
   * this browser brings: a device key the host knows under a certificate
   * this browser no longer holds (its storage was cleared, or a join the
   * host admitted never finished here) is never admitted under another, so
   * the person comes again as a new device of theirs. A person the workspace
   * bans is told so; a crowd at the entry (the workspace is full, or this
   * address asks too often) is waited out, as long as the entry asked. Both
   * pass through `wait` (the reconnect screen, with the backoff and the
   * entry's own `Retry-After`), so the page says where it stands and tries
   * again rather than stopping at an error. */
  const admitAnew = async (deviceKey, refused = false) => {
    let held = deviceKey;
    let since = null;
    /** How many times this tab has come again as a new device. */
    let renewals = 0;
    /** How many times the page has waited. */
    let waits = 0;
    for (;;) {
      try {
        return await throughEntry(held, refused, renewals > 0);
      } catch (error) {
        if (error?.name === 'AdmissionError' && error.code === 'refused') {
          // The first new device took this tab's key in place of the old one.
          if (renewals > 0) held = null;
          renewals += 1;
          console.warn('host admission: the host refused the entry\'s invitation, joining as a new device', error.reason);
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
  if (!forThisWorkspace || known?.userPublicKey !== identity.userPublicKey) return admitAnew();
  // Other tabs of this browser may hold every device key it has for this
  // host (`device-key-busy`): nothing the host refused, so the membership stays
  // and the entry is not asked, which would spend an admission to fail the same way.
  const deviceKey = await openDeviceKey(identityProfile(identity, known.origin));
  return withDeviceKey(deviceKey, async () => {
    // A key no host has seen yet is a new device of the person: the workspace
    // admits it through the entry, as it admitted the person's first.
    if (deviceKey.created) return admitAnew(deviceKey);
    let peer;
    try {
      peer = await connect({...connectOptions, origin: known.origin, workspace: known.workspace, identity, deviceKey, displayName});
      // A host refuses a membership it no longer has once the session
      // starts, not when connecting: wait for its answer here.
      await peer?.whenReady?.();
      return peer;
    } catch (error) {
      await peer?.dispose?.().catch(() => {});
      // The host has not answered yet, or is not answering at all (it is
      // restarting): nothing the host refused, so the membership stays, and
      // joining again would spend an admission and a join to fail the same way.
      if (error?.name === 'DeviceKeyError' || error?.name === 'TimeoutError' || hostUnreachable(error)) throw error;
      if (error?.code === 'host-changed') {
        // Another host answers at this origin now: join it as the same
        // person. What this browser kept stays pinned to the previous host.
        console.info('host admission: the host was set up again, joining it anew');
        keepMembership(memberships, workspace, null);
        return admitAnew(deviceKey);
      }
      // The membership is gone (pruned after a long absence, or the host
      // started its workspace anew), or this device was never admitted: ask the
      // host to admit the same person again rather than stop at an error.
      console.warn('host admission: reconnecting failed, asking to join again', error);
      keepMembership(memberships, workspace, null);
      return admitAnew(deviceKey, true);
    }
  }, connectOptions.signal);
}

/**
 * Become a new device of `userPublicKey` (hex), the person whose root
 * approves it on another of their devices, in `workspace` at the host at
 * `origin`, answering the pairing link's `invite` (hex, or `null`).
 * `onPairingCode(code)` is called with the code that device shows the
 * person; resolves to the peer once approved. Where that host
 * lets people in through its entry (`workspace.admission`), the new device
 * joins the person's workspace there as theirs. The host is remembered as
 * joined, paired, for the next visit (`joinViaHost`).
 *
 * A browser is never paired to the person it already is. `holds` is the
 * person whose root this browser keeps (hex, or `null`): a pairing link that
 * names them, opened in the browser that made it, is refused here
 * (`same-person`) before the host is asked anything. The link is not spent,
 * nobody is asked to approve, and nothing kept changes. Paired, the browser
 * would be remembered as a device whose root is elsewhere and leave the root
 * it holds unused: it could let no further device in.
 *
 * A device says whose it becomes before it asks. A link is written by
 * whoever made it, and following one makes this device theirs: so, before
 * the host is asked to let it in, `agree({userPublicKey, name, holds})` puts
 * that to the person here and resolves to whether they go on. `name` is the
 * host's word for the key the link names (the profile it keeps of them,
 * `findProfile`; empty when it keeps none or does not answer), never
 * anything the link says. The link wrote which server that is, so only one
 * this site names as its own is asked and taken at its word (`ofSite`: said
 * by the page, `ofThisSite` in pairing.mjs; `agree` hears it with `origin`).
 * Any other is asked nothing before the person goes on, and names nobody:
 * its keeper may be the link's maker, who would answer the reader's own
 * name. Without their yes the host is asked nothing
 * (`stopped`), and there is no pairing without `agree`. `stopped()` says
 * whether they stopped while the device waited: one approved meanwhile is
 * let go and nothing is kept of it.
 */
export async function pairViaHost({
  origin, workspace, userPublicKey, listed = null, label = 'Browser', displayName = null,
  memberships = globalThis.localStorage, fetch = globalThis.fetch, pair = pairDevice, holds = null,
  agree, findProfile = fetchProfile, ofSite = false, stopped = () => false, ...connectOptions
}) {
  if (!/^[0-9a-f]{64}$/.test(userPublicKey ?? '')) throw new HostAdmissionError('invalid', 'This pairing link names no person');
  if (connectOptions.ticket != null && !/^tilo1[0-9a-f]+$/.test(connectOptions.ticket)) {
    throw new HostAdmissionError('invalid', 'This pairing link is damaged');
  }
  if (holds && holds === userPublicKey) throw new HostAdmissionError('same-person', PAIRING_SAME_PERSON);
  if (typeof agree !== 'function') throw new TypeError('a device is paired only once it said whose it becomes: pairViaHost needs agree');
  const own = ofSite === true;
  const kept = own
    ? await Promise.resolve().then(() => findProfile(userPublicKey, [origin], fetch ? {fetch} : {})).catch(() => null) : null;
  const stop = () => new HostAdmissionError('stopped', PAIRING_STOPPED);
  if (!(await agree({userPublicKey, name: shownName(kept?.display_name), holds, origin, ofSite: own}))) throw stop();
  const entry = listed?.admission ?? undefined;
  const peer = await pair({...connectOptions, origin, workspace, userPublicKey, label, displayName, fetch, entry,
    profile: identityProfile({userPublicKey}, origin)}).catch(error => { throw stopped() ? stop() : entryRefused(error); });
  if (stopped()) {
    await peer.dispose?.();
    throw stop();
  }
  const was = keptMembership({host: {origin}}, memberships);
  keepMembership(memberships, {host: {origin}}, {origin, workspace: peer.workspace ?? workspace, userPublicKey, paired: true});
  // A browser that becomes another person here entered the workspaces it remembers
  // at this server as the other one: they are not this person's, so they are forgotten.
  // The own workspace a tab asked for as the person it was before this pairing is
  // one of them (`signInWorkspace` would otherwise reopen it as the person).
  if (connectOptions.runtime && was?.userPublicKey && was.userPublicKey !== userPublicKey) {
    forgetEnteredAt(connectOptions.runtime, {origin}, memberships);
  }
  return peer;
}

/**
 * Whether this browser opens the person's own desktop under an entry whose
 * home workspace is `home`: only a browser some host let in (a membership
 * it keeps: joined, paired or set up) does. One that none did is first sent
 * through the home's entry (`admission.mjs`), whose join is the answer: an
 * open entry lets it in, and one taking people by invitation only refuses
 * it, so anyone else has nothing here until invited.
 */
export async function ownDesktopOpen(home, {memberships = globalThis.localStorage} = {}) {
  if (!home?.admission) return true;
  try {
    return membershipOrigins(undefined, memberships).length > 0;
  } catch {
    return false; // No storage: nothing kept.
  }
}

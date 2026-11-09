/**
 * The workspaces this browser entered, as its tabs keep them: the ones it
 * was admitted to (an invitation it redeemed, an account it holds, a device
 * it paired) and the one a bare entry reopens.
 *
 * A workspace is remembered with the host that held it (its device key), so
 * one that another host now answers for is forgotten rather than offered
 * (`forgetGoneWorkspaces`). Remembering grants nothing: opening one is the
 * workspace's own admission again.
 *
 * This is storage and nothing else, which is why it is a module of its own:
 * a page that holds no peer still knows which workspaces the person has —
 * the sign-in route offers a grant that reaches one of them
 * (`sign-in.mjs`). The desktop reads it for its workspace switcher
 * (`workspaces.mjs`, which re-exports all of this).
 */

const ENTERED = 'tilo-entered-workspaces';
const CHOSEN = 'tilo-chosen-workspace';
/** The most workspaces remembered as entered; the least recently opened go first. */
const MAX_ENTERED = 32;
const HOST_DEVICE = /^[0-9a-f]{64}$/;

/** The origin a workspace's host is reached at, normalized for comparison. */
export const originOf = value => new URL(value).origin;

const readJson = (store, key, fallback) => {
  try { return JSON.parse(store?.getItem(key) ?? 'null') ?? fallback; } catch { return fallback; }
};
export const sameEntry = (a, b) => originOf(a.origin) === originOf(b.origin) && a.room === b.room;
const validEntry = entry => {
  try { return typeof entry?.room === 'string' && entry.room && Boolean(originOf(entry.origin)); } catch { return false; }
};
/** A remembered workspace names the host that held it: one that does not is not
 * offered, as nothing could ever tell whether it still exists. */
const keptEntry = entry => validEntry(entry) && HOST_DEVICE.test(entry.hostDevice ?? '');
const readEntered = store => {
  const list = readJson(store, ENTERED, []);
  return Array.isArray(list) ? list.filter(keptEntry) : [];
};
const writeEntered = (store, list) => store?.setItem(ENTERED, JSON.stringify(list.slice(0, MAX_ENTERED)));

/** The device key (hex) of the host whose certificate is `hostCert`, or
 * `null` when it names none. */
export function hostDeviceOf(hostCert) {
  const key = hostCert?.device_public_key;
  const text = typeof key === 'string' ? key : Array.from(key ?? [], byte => Number(byte).toString(16).padStart(2, '0')).join('');
  return HOST_DEVICE.test(text) ? text : null;
}

/**
 * The workspaces this browser entered as a member (an invitation it redeemed, an
 * account it holds, a device it paired), most recently opened first:
 * `{origin, room, hostDevice, name, unavailable}`. `hostDevice` is the host
 * that held the workspace when it was entered; `unavailable` says the last
 * attempt to open it failed without the host saying it is gone: the host
 * refused this browser, or could not be reached.
 */
export function enteredWorkspaces(store = globalThis.localStorage) {
  return readEntered(store).map(({origin, room, hostDevice, name = null, unavailable = false}) => ({
    origin: originOf(origin), room, hostDevice, name: typeof name === 'string' && name ? name : null, unavailable: unavailable === true,
  }));
}

/** Remember that this browser opened the workspace `{origin, room}`, held by the
 * host `hostDevice`, as a member, under `name` when one is known (an earlier
 * name stays otherwise). Workspaces remembered at the origin under another host
 * are gone with it, and forgotten. */
export function rememberEntered({origin, room, hostDevice, name = null}, store = globalThis.localStorage) {
  let entry;
  try { entry = {origin: originOf(origin), room, hostDevice}; } catch { return; }
  if (!keptEntry(entry)) return;
  forgetGoneWorkspaces(entry, store);
  const list = readEntered(store);
  const earlier = list.find(kept => sameEntry(kept, entry));
  const kept = name || earlier?.name || null;
  writeEntered(store, [{...entry, ...(kept ? {name: kept} : {})}, ...list.filter(other => !sameEntry(other, entry))]);
}

/** Name a remembered workspace after the workspace it showed, if it has no name yet. */
export function nameEntered({origin, room, name}, store = globalThis.localStorage) {
  if (typeof name !== 'string' || !name) return;
  const list = readEntered(store);
  const entry = list.find(kept => sameEntry(kept, {origin, room}));
  if (!entry || entry.name) return;
  entry.name = name;
  writeEntered(store, list);
}

/** The last attempt to open the remembered workspace `{origin, room}` failed: say
 * so where it is offered, and stop reopening it on a bare entry. */
export function markUnavailable({origin, room}, store = globalThis.localStorage) {
  const list = readEntered(store);
  const entry = list.find(kept => sameEntry(kept, {origin, room}));
  if (!entry) return;
  entry.unavailable = true;
  writeEntered(store, list);
  unchoose(store, entry);
}

/** Stop reopening `entry` on a bare entry, when it is the one chosen. */
function unchoose(store, entry) {
  const chosen = readJson(store, CHOSEN, null);
  if (chosen && validEntry(chosen) && sameEntry(chosen, entry)) store?.removeItem(CHOSEN);
}

/**
 * The host at `origin` answered as `hostDevice` (its device key, hex): the
 * workspaces remembered there under another host are gone with that host's
 * state, so they are forgotten, and a bare entry reopens none of them.
 * Returns the workspaces forgotten, `[{origin, room}]`. Only an answer from the
 * origin itself is passed here, never a failure to reach it.
 */
export function forgetGoneWorkspaces({origin, hostDevice}, store = globalThis.localStorage) {
  if (!HOST_DEVICE.test(hostDevice ?? '')) return [];
  let at;
  try { at = originOf(origin); } catch { return []; }
  const list = readEntered(store);
  const gone = list.filter(entry => originOf(entry.origin) === at && entry.hostDevice !== hostDevice);
  if (!gone.length) return [];
  writeEntered(store, list.filter(entry => !gone.includes(entry)));
  for (const entry of gone) unchoose(store, entry);
  return gone.map(entry => ({origin: originOf(entry.origin), room: entry.room}));
}

/**
 * Opening the remembered workspace `{origin, room}` failed: ask its origin which
 * host answers there now (`hostInfo(origin)`, resolving to that host's
 * certificate). Another host than the one that held the workspace means the workspace
 * is gone, and it is forgotten: `'gone'`. Anything else (the same host, or
 * no answer at all: offline, a network error, a failing host) says nothing
 * about the workspace, so it stays, marked as not opening last time:
 * `'unavailable'`. `null` for a workspace this browser does not remember.
 */
export async function settleFailedWorkspace({origin, room}, {hostInfo, store = globalThis.localStorage} = {}) {
  const entry = enteredWorkspaces(store).find(kept => sameEntry(kept, {origin, room}));
  if (!entry) return null;
  let answering = null;
  try {
    answering = hostDeviceOf(await hostInfo?.(entry.origin));
  } catch {
    // Not reached, or not answered: nothing the host said.
  }
  if (answering && answering !== entry.hostDevice) {
    forgetGoneWorkspaces({origin: entry.origin, hostDevice: answering}, store);
    return 'gone';
  }
  markUnavailable(entry, store);
  return 'unavailable';
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
  const entry = enteredWorkspaces(store).find(kept => sameEntry(kept, chosen));
  return entry && !entry.unavailable ? {origin: entry.origin, room: entry.room} : null;
}

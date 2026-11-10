/**
 * The workspaces this browser entered, as its tabs keep them: the ones it
 * was admitted to (an invitation it redeemed, an account it holds, a device
 * it paired) and the one a bare entry reopens.
 *
 * A workspace is remembered with the host that held it (its device key), so
 * one that another host now answers for is forgotten rather than offered
 * (`forgetGoneWorkspaces`). Remembering grants nothing: opening one is the
 * workspace's own admission again. How the list changes is the rule every
 * device keeps it by (`tilo-session-host`'s `entered`, through the runtime);
 * this module reads and writes it in the browser's storage.
 *
 * Reading needs no runtime, which is why it is a module of its own:
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
const writeEntered = (store, list) => store?.setItem(ENTERED, JSON.stringify(list));

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

/** Stop reopening `entry` on a bare entry, when it is the one chosen. */
function unchoose(store, entry) {
  const chosen = readJson(store, CHOSEN, null);
  if (chosen && validEntry(chosen) && sameEntry(chosen, entry)) store?.removeItem(CHOSEN);
}

/** The stored list, as the runtime's rules read it (they drop what is no
 * entry). */
const stored = store => {
  try { return store?.getItem(ENTERED) ?? '[]'; } catch { return '[]'; }
};

/** Keep what one of the runtime's rules answered (`{entered, unchoose}`):
 * the list as it is now, and no bare entry reopening the workspaces it
 * forgot or found not opening. Returns the answer. */
function keep(store, text) {
  const answer = JSON.parse(text);
  writeEntered(store, answer.entered);
  for (const place of answer.unchoose ?? []) unchoose(store, place);
  return answer;
}

/** Remember that this browser opened the workspace `{origin, room}`, held by
 * the host `hostDevice`, as a member, under `name` when one is known, by the
 * rules every device keeps the list by (`rememberEntered` in `runtime`):
 * first in the list, an earlier name staying, the workspaces remembered at
 * the origin under another host forgotten with it. */
export function rememberEntered(runtime, {origin, room, hostDevice, name = null}, store = globalThis.localStorage) {
  let at;
  try { at = originOf(origin); } catch { return; }
  if (typeof room !== 'string' || typeof hostDevice !== 'string') return;
  keep(store, runtime.rememberEntered(stored(store), JSON.stringify({origin: at, room, hostDevice, ...(name ? {name} : {})})));
}

/** Name a remembered workspace after the workspace it showed, if it has no name yet. */
export function nameEntered(runtime, {origin, room, name}, store = globalThis.localStorage) {
  if (typeof name !== 'string' || !name) return;
  let at;
  try { at = originOf(origin); } catch { return; }
  keep(store, runtime.nameEntered(stored(store), JSON.stringify({origin: at, room}), name));
}

/** The last attempt to open the remembered workspace `{origin, room}` failed: say
 * so where it is offered, and stop reopening it on a bare entry. */
export function markUnavailable(runtime, {origin, room}, store = globalThis.localStorage) {
  let at;
  try { at = originOf(origin); } catch { return; }
  keep(store, runtime.markEnteredUnavailable(stored(store), JSON.stringify({origin: at, room})));
}

/**
 * The host at `origin` answered as `hostDevice` (its device key, hex): the
 * workspaces remembered there under another host are gone with that host's
 * state, so they are forgotten, and a bare entry reopens none of them.
 * Returns the workspaces forgotten, `[{origin, room}]`. Only an answer from the
 * origin itself is passed here, never a failure to reach it.
 */
export function forgetGoneWorkspaces(runtime, {origin, hostDevice}, store = globalThis.localStorage) {
  let at;
  try { at = originOf(origin); } catch { return []; }
  if (typeof hostDevice !== 'string') return [];
  return keep(store, runtime.forgetGoneWorkspaces(stored(store), at, hostDevice)).unchoose ?? [];
}

/**
 * Opening the remembered workspace `{origin, room}` failed: ask its origin which
 * host answers there now (`hostInfo(origin)`, resolving to that host's
 * certificate), and settle it by the shared rules (`settleFailedWorkspace` in
 * `runtime`): `'gone'` when another host answers there now (forgotten),
 * `'unavailable'` otherwise (it stays, marked as not opening last time), and
 * `null` for a workspace this browser does not remember.
 */
export async function settleFailedWorkspace(runtime, {origin, room}, {hostInfo, store = globalThis.localStorage} = {}) {
  let at;
  try { at = originOf(origin); } catch { return null; }
  if (!enteredWorkspaces(store).some(kept => sameEntry(kept, {origin: at, room}))) return null;
  let answering = null;
  try {
    answering = hostDeviceOf(await hostInfo?.(at));
  } catch {
    // Not reached, or not answered: nothing the host said.
  }
  const {settled} = keep(store, runtime.settleFailedWorkspace(stored(store), JSON.stringify({origin: at, room}), answering ?? undefined));
  return settled === 'unknown' ? null : settled;
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

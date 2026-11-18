// A native app that holds this page as its person's device.
//
// Tilo's page in a system web view is a device of its person: its key, its session with the
// workspace's server, the documents it keeps in step. A native app around it draws views of its own
// and asks Tilo through the page. What its own code may ask as this device, beyond what the
// page's mount publishes, is handed to it here: once, and to nobody else. The page puts no
// handle on its session on a global, and it does not start to for a native host.
//
// A native app says it is there before any of the page's code runs: its document-start script
// defines `globalThis.tiloNativeHost` with a function `device`, and with `name` where it names
// its device itself (what its person reads where they let it in: `deviceLabel` in `pairing.mjs`). A browser has none, so in a
// browser nothing is handed to anyone. The host keeps what it is given where only its own code
// reaches it.
//
// What is handed over is the device's own entry and nothing wider. Every ask goes through the
// session this page holds with its workspace's host, as the person on this device, and the host
// decides each by the device's certificate, exactly as it does for the page's own windows: an
// app, an agent and anyone else's device are answered what an unknown action is answered.

/**
 * The entry of the device `peer` is, or `null` for a page whose workspace has no host to ask:
 *
 * - `person` and `workspace`: whom this device is certified as (root key, hex) and the workspace
 *   this page is in.
 * - `catalogue(query)`: what the host offers this person (`CatalogueQuery`).
 * - `invoke(target, argument, {confirmation, invocation})`: run one action and answer what the
 *   host said of it, `{invocation, state, output?, error?}`. Nothing of the call is kept in the
 *   page: an argument may be what the person typed into a protected field.
 * - `status(invocation)`: what became of one the host accepted and still runs.
 * - `ownWorkspace()`: the person's own workspace on this host, made the first time it is asked
 *   for, with this device in it: `{id, name, origin}`.
 * - `changed({of, after, waitMs})`: which of the documents `of` names changed in the workspace
 *   since `after`, held until one did or `waitMs` passed (`following` below):
 *   `{changed: [{app, document}], cursor}`. A word that something is to be read again, and
 *   nothing of what it reads as.
 */
export function deviceEntry({peer, ownWorkspace}, pace = {}) {
  const actions = peer?.actions;
  if (!actions || typeof actions.once !== 'function') return null;
  const follow = typeof actions.changes === 'function' && typeof actions.landed === 'function'
    ? following(actions, pace) : null;
  return Object.freeze({
    person: peer.userPublicKey ?? null,
    workspace: peer.workspace ?? null,
    catalogue: query => actions.catalogue(query),
    invoke: (target, argument, options) => actions.once(target, argument, options),
    status: invocation => actions.observe(invocation),
    ownWorkspace: () => {
      if (typeof ownWorkspace !== 'function') throw new Error('unknown_action: This page cannot ask for the person\'s own workspace');
      return ownWorkspace();
    },
    changed: async asked => {
      if (!follow) throw new Error('unknown_action: This page cannot tell when a document changed');
      return follow(asked);
    },
  });
}

/** How long one ask for changes is held at most: as long as the server holds its own. */
const HOLD_MS = 10_000;
/** Changes of one document within this time are one word, and the server's feed is read once in it at most. */
const SETTLE_MS = 1_000;
/** How many things one ask may name. */
const MOST_FOLLOWED = 32;

/** Whether a document is among what `of` names: `{app}` every document the app keeps in the
 * workspace, `{app, document}` that one. Throws for anything else, before anyone is asked. */
function followed(of) {
  const named = text => typeof text === 'string' && text.length > 0 && text.length <= 512;
  if (!Array.isArray(of) || of.length === 0 || of.length > MOST_FOLLOWED
    || !of.every(one => named(one?.app) && (one.document === undefined || one.document === null || typeof one.document === 'string'))) {
    throw new TypeError(`of is one to ${MOST_FOLLOWED} of {app, document?}`);
  }
  const wanted = of.map(one => ({app: one.app, document: one.document ?? null}));
  return change => wanted.some(one => one.app === change.app && (one.document === null || one.document === change.document));
}

/**
 * The entry's ask for changes, over the page's two asks of its workspace's server: the server's own
 * feed of its documents' changes (`actions.changes`) and its word that a write landed
 * (`actions.landed`). The server answers both for the person on this device, so a document is
 * named here only where the server tells this person of it; the entry reads nothing else, and a
 * write that woke the wait is no word until the feed names a document that was asked for.
 *
 * - No content: an answer names documents, `{app, document}`, each once.
 * - Only what was asked for: `of` names an app's documents, or one of them.
 * - Bounded: one ask at a time; the feed is read once in `settleMs` at most; what changes of a
 *   document while an answer settles is in that one answer.
 * - It ends by itself: after `waitMs`, `holdMs` at most, with nothing named; and an ask of the
 *   server that goes unanswered that long ends as `unanswered`.
 *
 * With no `after` nothing that happened yet is news: the answer is the cursor to ask on from.
 */
function following(actions, {holdMs = HOLD_MS, settleMs = SETTLE_MS} = {}) {
  let out = false;
  let lastRead = -Infinity;
  const pause = ms => new Promise(done => setTimeout(done, ms));
  const read = async cursor => {
    const early = lastRead + settleMs - Date.now();
    if (early > 0) await pause(early);
    lastRead = Date.now();
    let timer;
    const none = new Promise((_, refuse) => {
      timer = setTimeout(() => refuse(new Error('unanswered: The workspace\'s server did not say what changed')), holdMs);
    });
    try {
      return await Promise.race([actions.changes(cursor), none]);
    } finally {
      clearTimeout(timer);
    }
  };
  return async function changed({of, after = null, waitMs = 0} = {}) {
    const wanted = followed(of);
    if (out) throw new Error('busy: One ask for changes is out already');
    out = true;
    try {
      let cursor = typeof after === 'string' && after ? after : null;
      if (!cursor) return {changed: [], cursor: (await read(null)).cursor};
      const end = Date.now() + Math.min(Math.max(Number(waitMs) || 0, 0), holdMs);
      for (;;) {
        // Listened for before the feed is read: a write between the read and the wait still wakes it.
        const left = end - Date.now();
        const wait = left > 0 ? actions.landed(left) : null;
        let page;
        try {
          page = await read(cursor);
        } catch (error) {
          wait?.release();
          throw error;
        }
        cursor = page.cursor;
        const named = page.changed.filter(wanted);
        if (named.length) {
          wait?.release();
          // What else changes of these while the answer settles is in this same word.
          const more = await read(cursor);
          const told = new Map([...named, ...more.changed.filter(wanted)]
            .map(one => [`${one.app}\n${one.document}`, {app: one.app, document: one.document}]));
          return {changed: [...told.values()], cursor: more.cursor};
        }
        if (!wait || !(await wait.woke)) return {changed: [], cursor};
      }
    } finally {
      out = false;
    }
  };
}

/**
 * Hand this page's device entry to the native host in the document, when one does.
 * Answers whether one took it. Nothing is put where another script finds it.
 */
export function handToNativeHost({peer, ownWorkspace}, host = globalThis.tiloNativeHost) {
  if (typeof host?.device !== 'function') return false;
  const entry = deviceEntry({peer, ownWorkspace});
  if (!entry) return false;
  try {
    host.device(entry);
  } catch (error) {
    console.warn('the native host did not take this device\'s entry', error);
    return false;
  }
  return true;
}

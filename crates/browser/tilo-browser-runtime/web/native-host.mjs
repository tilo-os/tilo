// A native client keeps the peer admitted by the site's normal entry. Its WKWebView
// registers a document-start host and answers a fresh main-frame challenge before
// the entry hands it the peer's authenticated operations. The handle stays in the
// injected script's closure. The workspace decides every request using the same
// certificate and grants as requests from the browser.

import {hex} from './hex.mjs';

/**
 * The entry of the device `peer` is, or `null` for a page whose workspace has no host to ask:
 *
 * - `person` and `workspace`: whom this device is certified as (root key, hex) and the workspace
 *   this page is in.
 * - `catalogue(query)`: what the host offers this person (`CatalogueQuery`).
 * - `invoke(target, argument, {confirmation, invocation})`: run one action and answer what the
 *   host said of it, `{invocation, state, output?, error?}`. Nothing of the call is kept in the
 *   page: an argument may be what the person typed into a protected field.
 * - `workspaceRecord(app)`: resolve the hosted workspace record or open it on first use.
 * - `put` and `link`: upload a file in pieces and request a scoped link to content.
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
  const asks = (name, what) => (...asked) => {
    if (typeof actions[name] !== 'function') throw new Error(`unknown_action: This page cannot ${what}`);
    return actions[name](...asked);
  };
  const step = asks('contentPut', 'put content into its workspace');
  const linked = asks('contentLink', 'ask for a link to content');
  const records = new Map();
  // Run once, even if the host initially acknowledges a long-running action.
  async function invoke(target, argument, options) {
    let answer = await actions.once(target, argument, options);
    const end = Date.now() + 30_000;
    while (['accepted', 'progress', 'item'].includes(answer.state)) {
      if (Date.now() >= end) throw new Error('unanswered: The workspace has not finished the action');
      await new Promise(resolve => setTimeout(resolve, 150));
      answer = await actions.observe(answer.invocation);
    }
    return answer;
  }
  // This is the same open offered in the workspace catalogue. Opening an app
  // with one record per workspace returns that record, founding it only on first use.
  async function resolveRecord(app) {
    const query = {app_id: app};
    const listing = await actions.catalogue({...query, max_results: 0});
    const existing = listing.providers?.find(row => row.app_id === app && row.browsable && !row.document_id);
    if (existing) return existing.provider;
    const address = {resource: `app.${app}`, action: 'open'};
    const open = (await actions.catalogue({...query, target: address})).rows?.find(row =>
      row.app_id === app && row.action?.address?.resource === address.resource && row.action.address.action === address.action);
    if (!open) throw new Error('unknown_action: The workspace does not offer this app');
    const answer = await invoke({provider: open.provider, address: open.action.address}, null);
    const record = answer.output?.value;
    if (answer.state !== 'succeeded' || record?.record !== '' || !record.provider) {
      throw Object.assign(new Error(answer.error?.message ?? 'The workspace could not open the app'), {code: answer.error?.code ?? 'unavailable'});
    }
    return record.provider;
  }
  return Object.freeze({
    put: Object.freeze({
      begin: async size => ({upload: answered(await step({begin: {size: whole(size, 'size')}}), 'begun').upload}),
      append: async (upload, offset, piece) => ({written: answered(await step({append: {
        upload: whole(upload, 'upload'), offset: whole(offset, 'offset'), bytes: hexOf(piece)}}), 'appended').written}),
      finish: async upload => ({content: answered(await step({finish: {upload: whole(upload, 'upload')}}), 'finished').content}),
    }),
    link: async (content, name) => {
      const [address, saved] = [String(content ?? ''), String(name ?? '')];
      const hash = address.startsWith(CONTENT) ? address.slice(CONTENT.length) : address;
      const link = await linked({name: saved, files: [{path: saved, hash}], zip: false});
      return {path: link.path, expiresInSecs: link.expires_in_secs};
    },
    workspaceRecord(app) {
      if (!['so.tilo.chat', 'so.tilo.assistant', 'so.tilo.tasks'].includes(app)) throw new Error('unknown_action: No such workspace app');
      if (!records.has(app)) records.set(app, resolveRecord(app).catch(error => { records.delete(app); throw error; }));
      return records.get(app);
    },
    person: peer.userPublicKey ?? null,
    workspace: peer.workspace ?? null,
    catalogue: query => actions.catalogue(query),
    invoke,
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

/** The most bytes one piece of a put carries: what one frame of the session takes. */
const PIECE_BYTES = 384 * 1024;
/** How an address of content begins. */
const CONTENT = 'tilo.content:';

/** `value`, a whole number that is none below zero. */
function whole(value, what) {
  if (!Number.isSafeInteger(value) || value < 0) throw new TypeError(`${what} is a whole number, 0 or more`);
  return value;
}

/** `piece`, base64 text, as the hex a step of a put carries. */
function hexOf(piece) {
  let bytes;
  try {
    if (typeof piece !== 'string') throw new TypeError('no text');
    bytes = atob(piece);
  } catch {
    throw new TypeError('a piece is base64 text');
  }
  if (bytes.length > PIECE_BYTES) throw new Error(`invalid_request: A piece carries at most ${PIECE_BYTES} bytes`);
  return hex(Uint8Array.from(bytes, character => character.charCodeAt(0)));
}

/** The `kind` of answer a step of a put was given. */
function answered(answer, kind) {
  if (!answer?.[kind]) throw new Error('unavailable: A step of a put was answered with something else');
  return answer[kind];
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
export async function handToNativeHost({peer, ownWorkspace}, host = globalThis.tiloNativeHost,
  handler = globalThis.webkit?.messageHandlers?.tiloLive) {
  if (!host || !Object.isFrozen(host) || host.keepsDevice !== true || typeof host.device !== 'function'
      || typeof host.confirmDevice !== 'function' || typeof handler?.postMessage !== 'function') return false;
  const entry = deviceEntry({peer, ownWorkspace});
  if (!entry) return false;
  const nonce = crypto.randomUUID();
  try {
    const confirmed = host.confirmDevice(nonce);
    handler.postMessage({kind: 'device-challenge', nonce});
    if (!await confirmed) return false;
    return host.device(entry, nonce) === true;
  } catch {
    return false;
  }
}

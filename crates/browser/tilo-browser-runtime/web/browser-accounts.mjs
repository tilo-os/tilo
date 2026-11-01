import {requestResult} from './storage-inline.mjs';
import {transactionComplete} from './host-core.mjs';

/** Inspect canonical storage keys, fetching only Settings' personal accounts.
 * No credential/custody database, writer lease, or profile registry is opened.
 * With `currentDevice(profile)` (`peer.mjs` `profileDevice`), only the
 * namespace a profile's peer keeps its state in now counts: one a host set up
 * again left behind is never offered for that host. */
export async function readBrowserAccounts({origin, runtime, indexedDB = globalThis.indexedDB, currentDevice}) {
  const host = new URL(origin).host;
  const request = indexedDB.open('tilo-browser-records-v1');
  // A first visit has no accounts. Abort upgrade rather than create a database.
  request.onupgradeneeded = () => request.transaction.abort();
  let db;
  try { db = await requestResult(request); }
  catch (error) { if (error?.name === 'AbortError') return []; throw error; }
  try {
    if (!db.objectStoreNames.contains('records')) return [];
    const tx = db.transaction('records', 'readonly'), done = transactionComplete(tx);
    const store = tx.objectStore('records'), rows = [];
    const cursor = store.openKeyCursor();
    let scanned = 0, requested = 0, failure;
    cursor.onsuccess = () => {
      const item = cursor.result;
      if (!item) return;
      if (++scanned > 8192) { failure = Error('Account lookup exceeds its key budget'); tx.abort(); return; }
      try {
        if (typeof item.key === 'string') {
          const [namespace, key] = JSON.parse(item.key);
          const [profile, device] = JSON.parse(namespace);
          const match = /^tilo-identity:([a-f0-9]{64})@(.+)$/u.exec(profile);
          if (match?.[2] === host && typeof device === 'string' && device.length <= 256
            && (!currentDevice || currentDevice(profile) === device)
            && (key === runtime.browser_accounts_key(match[1])
              || key === runtime.browser_accounts_former_key(match[1]))) {
            if (++requested > 64) throw Error('Account lookup exceeds its profile budget');
            const current = key === runtime.browser_accounts_key(match[1]);
            const read = store.get(item.key);
            read.onsuccess = () => {
              const row = read.result;
              if (row?.namespace === namespace && row.key === key) rows.push({profile, device, root: match[1], bytes: row.bytes, current});
            };
          }
        }
      } catch (error) {
        // Other canonical keys are unrelated. A malformed Settings response
        // cannot turn into an account; explicit budget failures refuse lookup.
        if (error.message?.includes('budget')) { failure = error; tx.abort(); return; }
      }
      item.continue();
    };
    try { await done; } catch (error) { throw failure ?? error; }
    return projectBrowserAccounts(rows, runtime, origin);
  } finally { db.close(); }
}

/** The runtime reads the same typed PersonAccounts model Settings owns. */
export function projectBrowserAccounts(rows, runtime, origin) {
  if (rows.length > 64) throw Error('Account lookup exceeds its profile budget');
  const accounts = [];
  // A record kept before the person's own scope existed is offered only until
  // the scope has one: the row the app writes there from now on wins.
  const ordered = [...rows].sort((a, b) => Number(b.current === true) - Number(a.current === true));
  for (const row of ordered) {
    if (row.profile !== `tilo-identity:${row.root}@${new URL(origin).host}`) continue;
    const bytes = new Uint8Array(row.bytes);
    if (bytes.length > 1024 * 1024) throw Error('Account projection exceeds its read budget');
    for (const account of JSON.parse(runtime.project_browser_accounts(row.root, origin, bytes))) {
      if (account.userPublicKey !== row.root) throw Error('Account belongs to another browser profile');
      if (accounts.length >= 64) throw Error('Account projection exceeds its account budget');
      const selection = JSON.stringify([row.profile, row.device, account.id]);
      if (accounts.some(existing => existing.selection === selection)) continue;
      accounts.push(Object.freeze({...account, profile: row.profile, device: row.device, selection}));
    }
  }
  return accounts;
}

/** Re-read after asynchronous work; never substitute another account or profile. */
export async function currentBrowserAccount(selected, read) {
  const fresh = (await read()).find(account => account.selection === selected?.selection);
  if (!fresh || fresh.profile !== selected.profile || fresh.device !== selected.device
    || fresh.userPublicKey !== selected.userPublicKey) throw Error('Selected account changed or was removed');
  return fresh;
}

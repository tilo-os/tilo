import {requestResult} from './idb-transaction.mjs';
import {DATABASE_PREFIX,readFile} from './peer-files-db.mjs';

/** The Settings record of a person's accounts as a profile's store keeps it:
 * the page value `browser-accounts` (`src/peer_files.rs`). */
export const BROWSER_ACCOUNTS_FILE = 'state/page/browser-accounts';

/** The accounts this browser's profiles for `origin` keep, read from each
 * profile's store without opening it: no credential or device key database,
 * writer lease, or profile registry is opened. With `currentDevice(profile)`
 * (`peer.mjs` `profileDevice`), only the namespace a profile's peer keeps its
 * state in now counts: one a host set up again left behind is never offered
 * for that host. */
export async function readBrowserAccounts({origin, runtime, indexedDB = globalThis.indexedDB, IDBKeyRange = globalThis.IDBKeyRange, currentDevice}) {
  const host = new URL(origin).host;
  const names = (await indexedDB.databases()).map(database => database.name)
    .filter(name => typeof name === 'string' && name.startsWith(DATABASE_PREFIX));
  if (names.length > 8192) throw Error('Account lookup exceeds its key budget');
  const rows = [];
  for (const name of names) {
    let profile, device;
    try { [profile, device] = JSON.parse(name.slice(DATABASE_PREFIX.length)); } catch { continue; }
    const match = typeof profile === 'string' ? /^tilo-identity:([a-f0-9]{64})@(.+)$/u.exec(profile) : null;
    if (match?.[2] !== host || typeof device !== 'string' || device.length > 256
      || (currentDevice && currentDevice(profile) !== device)) continue;
    if (rows.length >= 64) throw Error('Account lookup exceeds its profile budget');
    const request = indexedDB.open(name);
    // A profile whose store went has no accounts. Abort upgrade rather than create a database.
    request.onupgradeneeded = () => request.transaction.abort();
    let db;
    try { db = await requestResult(request); }
    catch (error) { if (error?.name === 'AbortError') continue; throw error; }
    try {
      if (!db.objectStoreNames.contains('parts')) continue;
      const bytes = await readFile(db, BROWSER_ACCOUNTS_FILE, {IDBKeyRange});
      if (bytes) rows.push({profile, device, root: match[1], bytes});
    } finally { db.close(); }
  }
  return projectBrowserAccounts(rows, runtime, origin);
}

/** The runtime reads the same typed AccountList model Settings owns. */
export function projectBrowserAccounts(rows, runtime, origin) {
  if (rows.length > 64) throw Error('Account lookup exceeds its profile budget');
  const accounts = [];
  for (const row of rows) {
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

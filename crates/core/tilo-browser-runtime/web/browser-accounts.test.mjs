import test from 'node:test';
import assert from 'node:assert/strict';
import {BROWSER_ACCOUNTS_FILE, projectBrowserAccounts, currentBrowserAccount, readBrowserAccounts} from './browser-accounts.mjs';
import {applyPacked, openFilesDatabase} from './peer-files-db.mjs';
import {runtimeFake} from './runtime-surface.mjs';
import {fakeIndexedDB, fakeKeyRange as IDBKeyRange} from './testing/fake-storage.mjs';
const root = 'ab'.repeat(32), origin = 'https://host.example';
const row = {root, profile: `tilo-identity:${root}@host.example`, device: 'existing-device', bytes: [1]};
const runtime = runtimeFake({project_browser_accounts: () => JSON.stringify([{id: 'work', displayName: 'Work', userPublicKey: root}])});
test('picker reads a bounded public Settings projection scoped to the existing profile', () => {
  const choices = projectBrowserAccounts([row, {...row, profile: `tilo-identity:${root}@other.example`}], runtime, origin);
  assert.equal(choices.length, 1);
  assert.equal(choices[0].profile, row.profile);
  assert.equal(choices[0].device, row.device);
  assert.throws(() => projectBrowserAccounts([row], runtimeFake({project_browser_accounts: () => JSON.stringify([{id: 'wrong', userPublicKey: 'cd'.repeat(32)}])}), origin), /another browser profile/);
  assert.throws(() => projectBrowserAccounts(Array(65).fill(row), runtime, origin), /budget/);
  assert.throws(() => projectBrowserAccounts([{...row, bytes: new Uint8Array(1024 * 1024 + 1)}], runtime, origin), /budget/);
});
test('a changed or removed account cannot substitute another profile while connecting', async () => {
  const [selected] = projectBrowserAccounts([row], runtime, origin);
  assert.equal(await currentBrowserAccount(selected, async () => [selected]), selected);
  for (const fresh of [[], [{...selected, profile: 'replacement'}], [{...selected, device: 'replacement'}],
    [{...selected, userPublicKey: 'cd'.repeat(32)}], [{...selected, selection: 'replacement'}]]) {
    await assert.rejects(currentBrowserAccount(selected, async () => fresh), /changed or was removed/);
  }
});
test('the entry page reads the accounts record each profile\'s store keeps for this host', async () => {
  const indexedDB = fakeIndexedDB();
  // A commit of one file in one part, as the runtime packs it (`src/page_files.rs`).
  const keep = async (profile, device, path, bytes) => {
    const db = await openFilesDatabase(JSON.stringify([profile, device]), indexedDB);
    const name = new TextEncoder().encode(path), packed = new Uint8Array(28 + name.length + bytes.length);
    const view = new DataView(packed.buffer);
    view.setUint32(0, name.length, true); packed.set(name, 4); const at = 4 + name.length;
    view.setBigUint64(at, 1n, true); view.setUint32(at + 8, 1, true); view.setBigUint64(at + 12, 0n, true);
    view.setUint32(at + 20, bytes.length, true); packed.set(bytes, at + 24);
    await applyPacked(db, packed, {IDBKeyRange});
    db.close();
  };
  await keep(row.profile, 'existing-device', BROWSER_ACCOUNTS_FILE, [1, 2, 3]);
  await keep(row.profile, 'set-up-again', BROWSER_ACCOUNTS_FILE, [4]);
  await keep(`tilo-identity:${root}@other.example`, 'existing-device', BROWSER_ACCOUNTS_FILE, [5]);
  await keep(`tilo-identity:${'cd'.repeat(32)}@host.example`, 'no-record', 'state/page/other', [6]);
  const read = [];
  const projecting = runtimeFake({project_browser_accounts: (key, at, bytes) => {
    read.push([key, at, [...bytes]]);
    return JSON.stringify([{id: 'work', displayName: 'Work', userPublicKey: key}]);
  }});
  const databases = (await indexedDB.databases()).length;
  const choices = await readBrowserAccounts({origin, runtime: projecting, indexedDB, IDBKeyRange,
    currentDevice: profile => profile === row.profile ? 'existing-device' : 'no-record'});
  assert.deepEqual(read, [[root, origin, [1, 2, 3]]], 'only the current device\'s store of a profile for this host is read');
  assert.deepEqual(choices.map(choice => [choice.profile, choice.device, choice.id]), [[row.profile, 'existing-device', 'work']]);
  assert.equal((await indexedDB.databases()).length, databases, 'reading made no database');
  assert.deepEqual(await readBrowserAccounts({origin, runtime: projecting, indexedDB: fakeIndexedDB(), IDBKeyRange}), [],
    'a browser that keeps no profile offers no account');
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {projectBrowserAccounts, currentBrowserAccount} from './browser-accounts.mjs';
const root = 'ab'.repeat(32), origin = 'https://host.example';
const row = {root, profile: `tilo-identity:${root}@host.example`, device: 'existing-device', bytes: [1]};
const runtime = {project_browser_accounts: () => JSON.stringify([{id: 'work', displayName: 'Work', userPublicKey: root}])};
test('picker reads a bounded public Settings projection scoped to the existing profile', () => {
  const choices = projectBrowserAccounts([row, {...row, profile: `tilo-identity:${root}@other.example`}], runtime, origin);
  assert.equal(choices.length, 1);
  assert.equal(choices[0].profile, row.profile);
  assert.equal(choices[0].device, row.device);
  assert.throws(() => projectBrowserAccounts([row], {project_browser_accounts: () => JSON.stringify([{id: 'wrong', userPublicKey: 'cd'.repeat(32)}])}, origin), /another browser profile/);
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

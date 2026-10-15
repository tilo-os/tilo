/** Focused fixture/discovery regression; no compiled app or relay required. */
import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {launch} from '../../../../../tools/web/chrome.mjs';
import {mountChat} from './mount-chat.mjs';

const web = new URL('../../../../browser/tilo-browser-runtime/web/', import.meta.url);
// A small runtime substitute supplies the typed Settings projection. Storage,
// namespace selection and account discovery are the production browser modules;
// device custody is stopped below before the fixture can connect to a relay.
const host = `
import {createInlineStorage} from '/storage-inline.mjs';
const storage = createInlineStorage();
export async function openProfileStorage(profile, device) {
  const lease = await storage.acquireStorage(profile, device);
  return {lease, release: () => lease.release()};
}
export async function loadRuntime() {
  return {
    browser_accounts_key: root => 'accounts:' + root,
    project_browser_accounts: (root, origin, bytes) => new TextDecoder().decode(bytes),
    set_profile_value() {},
  };
}`;

test('mounted fixture seeds both accounts in the entry page’s current namespaces', async () => {
  const server = createServer(async (request, response) => {
    try {
      const path = new URL(request.url, 'http://localhost').pathname;
      if (path === '/') return response.end('<!doctype html><div id="surface"></div>');
      response.setHeader('content-type', 'text/javascript');
      if (path === '/host.mjs') return response.end(host);
      // Stop after real fixture seeding and selection, before connecting a relay.
      if (path === '/device-custody.mjs') return response.end(`
        export {idbStorage} from '/real-device-custody.mjs';
        export async function openDeviceCustody() { throw Error('account-selection-verified'); }
      `);
      const module = path === '/real-device-custody.mjs' ? 'device-custody.mjs' : path.slice(1);
      if (!/^[a-z0-9-]+\.mjs$/.test(module)) throw Error('invalid module path');
      response.end(await readFile(new URL(module, web)));
    } catch (error) {
      response.writeHead(404).end(String(error));
    }
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  let browser;
  try {
    browser = await launch({args: process.getuid?.() === 0 ? ['--no-sandbox'] : []});
    const tab = await browser.tab();
    await tab.navigate(`http://127.0.0.1:${server.address().port}/`);
    await tab.waitFor('document.readyState === "complete"', {timeout:5_000});
    const root = 'ab'.repeat(32), decoy = 'cd'.repeat(32), origin = 'https://fixture.example';
    const account = (userPublicKey, displayName) => Array.from(new TextEncoder().encode(
      JSON.stringify([{id:'fixture', userPublicKey, displayName}])));
    const fixture = {root, decoy, origin, accounts:account(root, 'Fixture reader'),
      decoyAccounts:account(decoy, 'Unadmitted'), certificate:{}, seed:Array(32).fill(1)};
    await tab.evaluate(`window.fixture = ${JSON.stringify(fixture)}; void 0`);
    // Cover both the default namespace and the namespace selected after a host
    // replacement, without teaching the fixture a second naming convention.
    for (const namespaces of [{}, {[`tilo-identity:${root}@fixture.example`]:'replacement-host-device'}]) {
      await tab.evaluate(`localStorage.setItem('tilo-profile-devices', ${JSON.stringify(JSON.stringify(namespaces))})`);
      const result = await mountChat(tab);
      assert.equal(result.error, 'account-selection-verified', JSON.stringify(result));
      const accounts = await tab.evaluate(`(async () => {
        const {readBrowserAccounts} = await import('/browser-accounts.mjs');
        const {profileDevice} = await import('/peer.mjs');
        return readBrowserAccounts({origin:fixture.origin, runtime, currentDevice:profileDevice});
      })()`);
      assert.equal(accounts.length, 2, 'the production entry must discover both seeded accounts');
      assert.deepEqual(await tab.evaluate('readAccounts()'), accounts);
      assert.equal(await tab.evaluate('selected.userPublicKey'), root);
      for (const row of accounts) {
        assert.equal(row.device, await tab.evaluate(`import('/peer.mjs').then(peer => peer.profileDevice(${JSON.stringify(row.profile)}))`));
      }
    }
  } finally {
    await browser?.close();
    await new Promise(done => server.close(done));
  }
});

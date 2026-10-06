/** Real compiled app + production peer/page on temporary governed HTTPS.
 * stdin is ephemeral fixture material, never a live identity or account. */
import assert from 'node:assert/strict';
import {createServer} from 'node:https';
import {readFileSync, existsSync, mkdirSync, writeFileSync} from 'node:fs';
import {resolve, join, extname} from 'node:path';
import {createInterface} from 'node:readline';
import {launch} from '../../../../../tools/web/chrome.mjs';

const bundle = resolve(process.argv[2]);
assert.ok(existsSync(join(bundle, 'resource.html')), 'built resource entry is required');
// Ephemeral TLS material arrives only over the native fixture's stdin pipe.
const input = createInterface({input: process.stdin});
const lines = input[Symbol.asyncIterator]();
const credentials = JSON.parse((await lines.next()).value);
const types = {'.html': 'text/html', '.mjs': 'text/javascript', '.js': 'text/javascript',
  '.css': 'text/css', '.wasm': 'application/wasm', '.json': 'application/json'};
const server = createServer(credentials, (request, response) => {
  const path = new URL(request.url, 'https://localhost').pathname;
  if (path === '/fixture.html') {
    response.writeHead(200, {'content-type': 'text/html'}).end('<!doctype html><link rel="stylesheet" href="/resource.css"><div id="surface"></div>');
    return;
  }
  const file = resolve(bundle, '.' + path);
  if (!file.startsWith(bundle + '/') || !existsSync(file)) { response.writeHead(404).end(); return; }
  response.writeHead(200, {'content-type': types[extname(file)] ?? 'application/octet-stream'}).end(readFileSync(file));
});
await new Promise(done => server.listen(0, '127.0.0.1', done));
const origin = `https://127.0.0.1:${server.address().port}`;
process.stdout.write(JSON.stringify({origin}) + '\n');
const fixture = JSON.parse((await lines.next()).value);
process.env.CHROME ??= '/usr/bin/chromium-browser';
let browser;
const stages = [];
const stage = name => {stages.push(name); process.stdout.write(JSON.stringify({stage: name}) + '\n');};
const evidence = resolve('target/mounted-resource');
mkdirSync(evidence, {recursive:true});
const deadline = setTimeout(async () => {
  console.error('Mounted resource journey exceeded its 130-second bound');
  await browser?.close();
  server.close();
  process.exit(1);
}, 130_000);
try {
  // Certificate exceptions exist only inside this disposable fixture browser.
  browser = await launch({args: ['--ignore-certificate-errors', '--enable-features=Vulkan',
    '--use-vulkan=swiftshader', '--use-angle=swiftshader',
    ...(process.getuid?.() === 0 ? ['--no-sandbox'] : [])]});
  const tab = await browser.tab();
  await tab.navigate(origin + '/fixture.html');
  await tab.waitFor('document.readyState === "complete"');
  await tab.send('Page.bringToFront');
  const permission = await browser.send('Browser.setPermission', {origin,
    permission: {name: 'clipboard-read', allowWithoutSanitization:true}, setting:'granted'});
  assert.equal(permission.error, undefined, JSON.stringify(permission.error));
  const supplied = await tab.send('Runtime.evaluate', {expression:`window.fixture = ${JSON.stringify(fixture)}; void 0`});
  assert.equal(supplied.result?.exceptionDetails, undefined, 'fixture bootstrap could not be supplied');
  const initialized = await tab.evaluate(`(async () => { try {
    const host = await import('/host.mjs');
    window.runtime = await host.loadRuntime();
    const {profileStore} = await import('/peer.mjs');
    const {idbStorage} = await import('/device-custody.mjs');
    const storage = idbStorage();
    for (const [root, bytes, device] of [[fixture.root, fixture.accounts, 'fixture-reader'],
      [fixture.decoy, fixture.decoyAccounts, 'unadmitted-fixture']]) {
      const profile = 'tilo-identity:' + root + '@' + new URL(fixture.origin).host;
      const opened = await host.openProfileStorage(profile, device);
      await opened.lease.commit([{key: runtime.browser_accounts_key(root), bytes: new Uint8Array(bytes), revision: '0'}]);
      if (root === fixture.root) {
        await profileStore(runtime, profile, device).put('certificate', JSON.stringify(fixture.certificate));
        const wrapKey = await crypto.subtle.generateKey({name:'AES-GCM', length:256}, false, ['encrypt','decrypt']);
        const iv = crypto.getRandomValues(new Uint8Array(12));
        const sealed = new Uint8Array(await crypto.subtle.encrypt({name:'AES-GCM', iv}, wrapKey, new Uint8Array(fixture.seed)));
        await storage.put('device-key:' + profile, {kind:'sealed-seed', wrapKey, iv, sealed});
      }
      await opened.release();
    }
    const {readBrowserAccounts, currentBrowserAccount} = await import('/browser-accounts.mjs');
    window.readAccounts = () => readBrowserAccounts({origin: fixture.origin, runtime});
    const accounts = await readAccounts();
    if (accounts.length !== 2) throw Error('two actual Settings account choices required');
    window.selected = accounts.find(account => account.userPublicKey === fixture.root);
    await currentBrowserAccount(selected, readAccounts);
    const {connectPeer, withCustody} = await import('/peer.mjs');
    const {openDeviceCustody} = await import('/device-custody.mjs');
    const custody = await openDeviceCustody({profile:selected.profile, storage, requireExisting:true});
    window.mountDiagnostic = [];
    const desktop = {...host, mountDesktop:(target, options)=>host.mountDesktop(target, {...options,
      onStatus:status=>{if(status.catalogueError || status.clipboard) mountDiagnostic.push(status); options.onStatus?.(status);}})};
    window.peer = await withCustody(custody, () => connectPeer({origin:fixture.origin,
      room:fixture.room, userPublicKey:selected.userPublicKey, profile:selected.profile,
      device:selected.device, custody, runtime, desktop, live:false, requireExistingCertificate:true}));
    await peer.whenReady();
    window.catalogueDiagnostic = {calls:0, replies:0};
    const catalogue = peer.actions.catalogue.bind(peer.actions);
    peer.actions.catalogue = async query => {
      catalogueDiagnostic.query = query;
      catalogueDiagnostic.calls++;
      try { const page = await catalogue(query); catalogueDiagnostic.replies++; catalogueDiagnostic.rows=page.rows.length; catalogueDiagnostic.locator=Boolean(page.locator); return page; }
      catch (error) { catalogueDiagnostic.error = String(error?.message ?? error); throw error; }
    };
    window.link = await peer.resourceLink(location.origin + '/resource.html', fixture.reference,
      fixture.provider, 'reveal-message');
    const {openResourceOperation} = await import('/resource-session.mjs');
    window.operation = {controller:new AbortController()};
    window.mount = await openResourceOperation({operation, link, target:document.querySelector('#surface'), connect:async()=>peer});
    await currentBrowserAccount(selected, readAccounts);
    mount.requestSemantics();
    return {actions:mount.actions(fixture.reference.id)};
  } catch (error) {
    return {error:String(error?.message ?? error), stack:error?.stack};
  }
  })()`);
  assert.equal(initialized.error, undefined, JSON.stringify(initialized));
  await tab.waitFor('document.querySelector("canvas")?.width > 0');
  assert.ok((await tab.evaluate('mount.actions(fixture.reference.id)')).some(action => action.address.action === 'reveal-message-range'));
  stage('compiled-native-chat-mounted-exact-range');
  const initialShot = await tab.screenshot();
  assert.ok(initialShot?.length, 'the mounted fixture must produce a rendered frame');
  writeFileSync(join(evidence, 'mounted-chat-range.png'), initialShot);
  const context = await tab.evaluate(`(() => {
    const all = mount.actions();
    const call = action => {
      const descriptor = all.find(row=>row.address.action === action);
      if (!descriptor) throw Error('missing public context action: ' + action);
      return mount.invokeAction(descriptor.address.resource, action);
    };
    const before = call('live-context-status');
    call('share-live-context');
    const sharing = call('live-context-status');
    call('stop-sharing-live-context');
    const stopped = call('live-context-status');
    return {before, sharing, stopped};
  })()`);
  assert.equal(context.before.output.value.sharing, false);
  assert.equal(context.sharing.output.value.sharing, true);
  assert.equal(context.sharing.output.value.videoCapture, false);
  assert.equal(context.stopped.output.value.sharing, false);
  stage('mounted-public-consent-and-stop-without-video');
  const clickAction = async action => {
    const response = await tab.send('Runtime.evaluate', {expression: `mount.invokeAction(fixture.shared, ${JSON.stringify(action)})`, returnByValue:true, userGesture:true});
    assert.equal(response.result?.exceptionDetails, undefined, JSON.stringify(response.result?.exceptionDetails));
    assert.equal(response.result?.result.value?.state, 'accepted');
  };
  await clickAction('copy-resource-link');
  try {
    await tab.waitFor('(async()=> (await navigator.clipboard.readText()).includes("#resource="))()', {timeout:15_000});
  } catch (error) {
    console.error('Clipboard read result', await tab.evaluate('navigator.clipboard.readText().then(text=>({length:text.length, resource:text.includes("#resource=")})).catch(error=>String(error))'));
    console.error('Copy diagnostic', await tab.evaluate('({catalogue:catalogueDiagnostic, bridge:mountDiagnostic, state:mount.state(), active:document.hasFocus()})'), tab.log);
    throw error;
  }
  const copied = await tab.evaluate('navigator.clipboard.readText()');
  assert.equal(new URL(copied).origin, origin);
  stage('native-copy-to-real-browser-clipboard');
  await clickAction('open-resource-link');
  let opened;
  for (let attempt=0; attempt<50; attempt++) {
    const targets = await browser.send('Target.getTargets');
    opened = targets.result.targetInfos.find(target => target.targetId !== tab.targetId && target.url === copied);
    if (opened) break;
    await new Promise(done => setTimeout(done, 100));
  }
  assert.ok(opened, 'native Open must create the exact browser resource destination');
  await browser.send('Target.closeTarget', {targetId:opened.targetId});
  stage('native-open-browser-destination');
  await tab.evaluate('(async()=>{await (await import("/resource-session.mjs")).closeResourceOperation(operation); return document.querySelectorAll("canvas").length;})()');
  const link = await tab.evaluate('link');
  await tab.navigate(link);
  await tab.waitFor('document.querySelector("select")?.options.length === 2');
  await tab.evaluate('document.querySelector("select").value = Array.from(document.querySelector("select").options).find(option=>option.textContent.includes("Unadmitted")).value; document.querySelector("#open").requestSubmit();');
  await tab.waitFor('document.querySelector("#status").textContent.includes("no existing browser device")');
  stage('explicit-unadmitted-account-refused-without-enrollment');
  await tab.evaluate('document.querySelector("select").value = Array.from(document.querySelector("select").options).find(option=>option.textContent === "Fixture reader").value; document.querySelector("#open").requestSubmit();');
  await tab.waitFor('document.querySelector("#status").textContent === "Shared resource open. Leave to close this session."');
  assert.ok(await tab.evaluate('document.querySelector("#surface canvas")?.width > 0'));
  stage('production-account-choice-and-resource-page-mount');
  await tab.evaluate('document.querySelector("#leave").click()');
  await tab.waitFor('document.querySelector("#status").textContent === "Left the shared resource." && !document.querySelector("canvas")');
  stage('leave-releases-mounted-app-and-device-custody');
  await tab.navigate(origin + '/fixture.html');
  await tab.navigate(link);
  await tab.waitFor('document.querySelector("select")?.options.length === 2');
  stage('page-navigation-reopens-existing-account-picker');
  stage('revoke');
  assert.equal(JSON.parse((await lines.next()).value).revoked, true);
  await tab.evaluate('document.querySelector("select").value = Array.from(document.querySelector("select").options).find(option=>option.textContent === "Fixture reader").value; document.querySelector("#open").requestSubmit();');
  await tab.waitFor('document.querySelector("#status").textContent !== "Checking your existing admission…" && document.querySelector("#status").textContent !== ""', {timeout:45_000});
  assert.equal(await tab.evaluate('document.querySelectorAll("canvas").length'), 0);
  assert.notEqual(await tab.evaluate('document.querySelector("#status").textContent'), 'Shared resource open. Leave to close this session.');
  stage('governed-revocation-refuses-link-reopen');
  writeFileSync(join(evidence, 'journey.json'), JSON.stringify({stages}, null, 2) + '\n');
} finally {
  clearTimeout(deadline);
  input.close();
  process.stdin.pause();
  await browser?.close();
  await new Promise(done => server.close(done));
}

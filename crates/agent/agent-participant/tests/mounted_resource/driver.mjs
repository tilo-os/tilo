/** Real compiled app + production peer/page on temporary governed HTTPS.
 * stdin is ephemeral fixture material, never a live identity or account. */
import assert from 'node:assert/strict';
import {createServer} from 'node:https';
import {readFileSync, existsSync, mkdirSync, writeFileSync} from 'node:fs';
import {resolve, join, extname} from 'node:path';
import {createInterface} from 'node:readline';
import {launch} from '../../../../../tools/web/chrome.mjs';
import {mountChat} from './mount-chat.mjs';

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
if (fixture.encryptedRestart) assert.notEqual(process.getuid?.(), 0, 'cold encrypted resource requires an unprivileged sandboxed browser');
process.env.CHROME ??= '/usr/bin/chromium-browser';
let browser;
let tab;
const stages = [];
const stage = name => {stages.push(name); process.stdout.write(JSON.stringify({stage: name}) + '\n');};
const evidence = resolve(fixture.encryptedRestart ? 'target/mounted-resource-encrypted' : fixture.disconnectRetry ? 'target/mounted-resource-recovery' : 'target/mounted-resource');
mkdirSync(evidence, {recursive:true});
const deadline = setTimeout(async () => {
  const pageStatus = await Promise.race([
    tab?.evaluate('document.querySelector("#status")?.textContent ?? ""', {timeout:250})
      .catch(() => 'status unavailable'),
    new Promise(resolve => setTimeout(() => resolve('status observation unavailable'), 250)),
  ]);
  writeFileSync(join(evidence, 'failure.json'), JSON.stringify({phase:'timeout',
    lastStage:stages.at(-1) ?? null, pageStatus: String(pageStatus ?? '').slice(0, 500)}, null, 2) + '\n');
  console.error('Mounted resource journey exceeded its 130-second bound');
  await browser?.close();
  server.close();
  process.exit(1);
}, 130_000);
try {
  if (fixture.encryptedRestart) stage('cold-remote-encrypted-canonical-primary');
  // Certificate exceptions exist only inside this disposable fixture browser.
  browser = await launch({args: ['--ignore-certificate-errors', '--enable-features=Vulkan',
    '--use-vulkan=swiftshader', '--use-angle=swiftshader',
    ...(process.getuid?.() === 0 ? ['--no-sandbox'] : [])]});
  tab = await browser.tab();
  await tab.navigate(origin + '/fixture.html');
  await tab.waitFor('document.readyState === "complete"');
  await tab.send('Page.bringToFront');
  const permission = await browser.send('Browser.setPermission', {origin,
    permission: {name: 'clipboard-read', allowWithoutSanitization:true}, setting:'granted'});
  assert.equal(permission.error, undefined, JSON.stringify(permission.error));
  const supplied = await tab.send('Runtime.evaluate', {expression:`window.fixture = ${JSON.stringify(fixture)}; void 0`});
  assert.equal(supplied.result?.exceptionDetails, undefined, 'fixture bootstrap could not be supplied');
  const initialized = await mountChat(tab);
  assert.equal(initialized.error, undefined, JSON.stringify(initialized));
  await tab.waitFor('document.querySelector("canvas")?.width > 0');
  assert.ok((await tab.evaluate('mount.actions(fixture.reference.id)')).some(action => action.address.action === 'reveal-message-range'));
  if (fixture.encryptedRestart) {
    const read = await tab.evaluate("mount.invokeAction(fixture.channel, 'read-messages', {pageSize:32})");
    assert.equal(read.state, 'completed');
    const original = read.output.value.content.messages.find(message => message.id === fixture.reference.id.split('/').pop());
    assert.ok(original, 'the exact original message must be readable');
    assert.equal(original.author, fixture.sourceAuthor, 'remote message retains the certified foreign writer identity');
    assert.notEqual(original.author, fixture.root, 'the original author must differ from the viewing person');
    assert.notEqual(fixture.sourcePeople.find(person => person.id === original.author)?.name, 'You', 'an unnamed writer must not persist the current-reader label as a shared name');
    writeFileSync(join(evidence, 'identity-boundary.json'), JSON.stringify({writer:original.author, viewer:fixture.root, sourcePeople:fixture.sourcePeople, semantics:await tab.evaluate('mount.state().semantics')}, null, 2));
  }
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
  if (fixture.disconnectRetry) {
    const before = await tab.evaluate('({peer:peer.getSnapshot(), frames:mount.state().frames})');
    assert.equal(before.peer.health.transport.authenticated, true);
    const network = await tab.send('Network.enable');
    assert.equal(network.error, undefined, 'fixture network control must be available');
    const blocked = await tab.send('Network.setBlockedURLs', {urls:[fixture.origin + '/*']});
    assert.equal(blocked.error, undefined, 'only the fixture relay origin must be blocked');
    // Close the existing certified carrier; a real browser request to the
    // blocked fixture origin must fail, rather than fabricating peer state.
    await tab.evaluate('peer.reconnect()');
    await tab.waitFor('!peer.getSnapshot().health.transport.authenticated && peer.getSnapshot().health.transport.phase !== "connected"');
    stage('browser-offline');
    assert.equal(JSON.parse((await lines.next()).value).nativePublished, true);
    const offlineRead = await tab.evaluate("mount.invokeAction(fixture.channel, 'read-messages', {pageSize:32})");
    assert.equal(offlineRead.state, 'completed');
    assert.equal(offlineRead.output.value.content.messages.filter(message =>
      message.text === 'Native fixture while browser disconnected').length, 0,
    'the disconnected browser must not already have received the new native post');
    const queued = await tab.evaluate(`mount.invokeAction(fixture.channel, 'post',
      {text:'Browser fixture queued during disconnect'})`);
    assert.equal(queued.state, 'completed', 'the original local post must complete once');
    await tab.evaluate('mount.flush()');
    await tab.waitFor('peer.getSnapshot().outgoingPending > 0 && !peer.getSnapshot().health.transport.authenticated');
    stage('compiled-browser-post-queued-without-transport');
    const unblocked = await tab.send('Network.setBlockedURLs', {urls:[]});
    assert.equal(unblocked.error, undefined, 'fixture relay network must be restored');
    const recovered = await tab.evaluate('peer.recover({kind:"retry-connection"})');
    assert.equal(recovered.operationId, 'retry-connection');
    await tab.waitFor('peer.getSnapshot().phase === "ready" && peer.getSnapshot().health.transport.authenticated');
    await tab.evaluate('peer.flush()');
    await tab.waitFor('peer.getSnapshot().outgoingPending === 0 && peer.getSnapshot().synchronization === "acknowledged"');
    await tab.waitFor(`(() => {
      const result = mount.invokeAction(fixture.channel, 'read-messages', {pageSize:32});
      if (result.state !== 'completed') return false;
      const messages = result.output.value.content.messages;
      return messages.filter(message => message.text === 'Browser fixture queued during disconnect').length === 1
        && messages.filter(message => message.text === 'Native fixture while browser disconnected').length === 1;
    })()`);
    const after = await tab.evaluate('peer.getSnapshot()');
    assert.equal(after.deviceId, before.peer.deviceId, 'retry keeps the original certified device');
    const frameBeforeOpen = await tab.evaluate('mount.state().frames');
    const openedChannel = await tab.evaluate("mount.invokeAction(fixture.channel, 'open-channel')");
    assert.equal(openedChannel.state, 'completed');
    await tab.waitFor('mount.state().presentation && mount.state().frames > ' + frameBeforeOpen);
    await tab.waitFor(`JSON.stringify(mount.state().semantics).includes('Browser fixture queued during disconnect')
      && JSON.stringify(mount.state().semantics).includes('Native fixture while browser disconnected')`);
    stage('browser-recovered');
    assert.equal(JSON.parse((await lines.next()).value).nativeReceivedOnce, true);
    stage('certified-retry-merges-both-original-posts-once');
  }
  await tab.evaluate('(async()=>{await (await import("/resource-session.mjs")).closeResourceOperation(operation); return document.querySelectorAll("canvas").length;})()');
  const link = await tab.evaluate('link');
  await tab.navigate(link);
  await tab.waitFor('document.querySelector("select")?.options.length === 2');
  await tab.evaluate('document.querySelector("select").value = Array.from(document.querySelector("select").options).find(option=>option.textContent.includes("Unadmitted")).value; document.querySelector("#open").requestSubmit();');
  await tab.waitFor('document.querySelector("#status").textContent.includes("no existing browser device")');
  stage('explicit-unadmitted-account-refused-without-enrollment');
  await tab.evaluate('document.querySelector("select").value = Array.from(document.querySelector("select").options).find(option=>option.textContent === "Fixture reader").value; document.querySelector("#open").requestSubmit();');
  await tab.waitFor('document.querySelector("#status").textContent !== "Checking your existing admission…" && document.querySelector("#status").textContent !== ""');
  assert.equal(await tab.evaluate('document.querySelector("#status").textContent'),
    'Shared resource open. Leave to close this session.', 'the admitted reader must reopen the shared resource');
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
  writeFileSync(join(evidence, 'journey.json'), JSON.stringify({mode: fixture.encryptedRestart ? 'certified-browser-encrypted-cold' : fixture.disconnectRetry ? 'certified-browser-disconnect-retry' : 'certified-browser-viewer',
    app:'so.tilo.chat', stages}, null, 2) + '\n');
} catch (error) {
  const pageStatus = await Promise.race([
    tab?.evaluate('document.querySelector("#status")?.textContent ?? ""', {timeout:250})
      .catch(() => 'status unavailable'),
    new Promise(resolve => setTimeout(() => resolve('status observation unavailable'), 250)),
  ]);
  writeFileSync(join(evidence, 'failure.json'), JSON.stringify({phase:'error',
    lastStage:stages.at(-1) ?? null, pageStatus:String(pageStatus ?? '').slice(0, 500),
    error:String(error).slice(0, 500)}, null, 2) + '\n');
  throw error;
} finally {
  clearTimeout(deadline);
  input.close();
  process.stdin.pause();
  await browser?.close();
  await new Promise(done => server.close(done));
}

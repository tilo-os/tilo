/** Real compiled app + production peer/page on temporary governed HTTPS.
 * stdin is ephemeral fixture material, never a live identity or account. */
import assert from 'node:assert/strict';
import {createServer} from 'node:https';
import {readFileSync, existsSync, mkdirSync, writeFileSync} from 'node:fs';
import {resolve, join, extname} from 'node:path';
import {createInterface} from 'node:readline';
import {launch} from '../../../../../tools/web/chrome.mjs';
import {mountChat} from './mount-chat.mjs';
import {observe} from './observe.mjs';
// A channel's latest messages, as the page reads them; a refusal is the
// answer, so a failed read says why.
const READ_MESSAGES = "(() => { try { return mount.invokeAction(fixture.channel, 'read-messages', {pageSize:32}); } catch (error) { return {state:'threw', error:String(error?.message ?? error)}; } })()";

const bundle = resolve(process.argv[2]);
assert.ok(existsSync(join(bundle, 'app/index.html')), 'built full-tab entry page is required');
// Ephemeral TLS material arrives only over the native fixture's stdin pipe.
const input = createInterface({input: process.stdin});
const lines = input[Symbol.asyncIterator]();
const credentials = JSON.parse((await lines.next()).value);
const types = {'.html': 'text/html', '.mjs': 'text/javascript', '.js': 'text/javascript',
  '.css': 'text/css', '.wasm': 'application/wasm', '.json': 'application/json'};
const server = createServer(credentials, (request, response) => {
  const path = new URL(request.url, 'https://localhost').pathname;
  if (path === '/fixture.html') {
    response.writeHead(200, {'content-type': 'text/html'}).end('<!doctype html><style>html,body{margin:0;height:100%}#surface{position:fixed;inset:0}</style><div id="surface"></div>');
    return;
  }
  // A followed link names the entry page by its directory (`/app/`), as a
  // web host serves it.
  const file = resolve(bundle, '.' + path + (path.endsWith('/') ? 'index.html' : ''));
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
let observer;
const stages = [];
const stage = name => {stages.push(name); observer?.mark(name); process.stdout.write(JSON.stringify({stage: name}) + '\n');};
const evidence = resolve(fixture.encryptedRestart ? 'target/mounted-resource-encrypted' : fixture.disconnectRetry ? 'target/mounted-resource-recovery' : 'target/mounted-resource');
mkdirSync(evidence, {recursive:true});
/** What the page says, and what its graphics start-up waits on while its
 * first frame is late (`data-gpu-step`: device, pipelines, surface,
 * first-frame). */
const pageState = () => Promise.race([
  tab?.evaluate(`JSON.stringify({status: document.querySelector("#status")?.textContent ?? "",
    gpuStep: document.documentElement.dataset.gpuStep ?? null})`, {timeout:250})
    .then(JSON.parse).catch(() => ({status: 'status unavailable'})) ?? {status: 'status unavailable'},
  new Promise(resolve => setTimeout(() => resolve({status: 'status observation unavailable'}), 250)),
]);
const deadline = setTimeout(async () => {
  const page = await pageState();
  writeFileSync(join(evidence, 'failure.json'), JSON.stringify({phase:'timeout',
    lastStage:stages.at(-1) ?? null, pageStatus: String(page.status ?? '').slice(0, 500),
    gpuStep: page.gpuStep ?? null}, null, 2) + '\n');
  console.error('Mounted resource journey exceeded its 130-second bound');
  await observer?.finish().catch(error => console.error('main-thread evidence unavailable', error));
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
  observer = await observe(browser, tab, evidence);
  await tab.navigate(origin + '/fixture.html');
  await tab.waitFor('document.readyState === "complete"');
  await tab.send('Page.bringToFront');
  const permission = await browser.send('Browser.setPermission', {origin,
    permission: {name: 'clipboard-read', allowWithoutSanitization:true}, setting:'granted'});
  assert.equal(permission.error, undefined, JSON.stringify(permission.error));
  const supplied = await tab.send('Runtime.evaluate', {expression:`window.fixture = ${JSON.stringify(fixture)}; void 0`});
  assert.equal(supplied.result?.exceptionDetails, undefined, 'fixture bootstrap could not be supplied');
  observer.mark('mount-start');
  const initialized = await mountChat(tab);
  assert.equal(initialized.error, undefined, JSON.stringify(initialized));
  await tab.waitFor('document.querySelector("canvas")?.width > 0');
  assert.ok((await tab.evaluate('mount.actions(fixture.reference.id)')).some(action => action.address.action === 'reveal-message-range'));
  if (fixture.encryptedRestart) {
    const read = await tab.evaluate(READ_MESSAGES);
    assert.equal(read.state, 'completed', JSON.stringify(read));
    const original = read.output.value.messages.find(message => message.id === fixture.reference.id.split('/').pop());
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
  // Sharing live context reaches outside the record, which a room's viewer
  // may not; an editor, once the page has learned the role the room gives
  // them, confirms sharing it and stopping, without video.
  const contextActions = ['live-context-status', 'share-live-context', 'stop-sharing-live-context'];
  if (fixture.disconnectRetry) {
    await tab.waitFor(`(() => {
      const offered = mount.actions().map(row => row.address.action);
      return ${JSON.stringify(contextActions)}.every(action => offered.includes(action));
    })()`);
    const context = await tab.evaluate(`(() => {
      const all = mount.actions();
      const call = action => {
        const descriptor = all.find(row=>row.address.action === action);
        if (!descriptor) throw Error('missing public context action: ' + action);
        // The room asks an editor before an outside action: they confirm.
        return mount.invokeAction(descriptor.address.resource, action, {}, {confirm:true});
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
  } else {
    const offered = await tab.evaluate('mount.actions().map(row => row.address.action)');
    assert.deepEqual(contextActions.filter(action => offered.includes(action)), [],
      'a viewer is not offered to share live context');
  }
  stage('mounted-public-consent-and-stop-without-video');
  // Chat's own controls under the message that shares the resource: they
  // ask the resource's app for its share-link and copy or open the URL.
  const press = async label => {
    const response = await tab.send('Runtime.evaluate', {expression: `(() => {
      const control = document.querySelector('[aria-label=${JSON.stringify(label)}]');
      if (!control) return false;
      control.click();
      return true;
    })()`, returnByValue:true, userGesture:true});
    assert.equal(response.result?.exceptionDetails, undefined, JSON.stringify(response.result?.exceptionDetails));
    return response.result?.result.value === true;
  };
  const pressWhenShown = async label => {
    for (let attempt = 0; attempt < 100; attempt++) {
      if (await press(label)) return;
      await tab.evaluate('mount.requestSemantics()');
      await new Promise(done => setTimeout(done, 100));
    }
    assert.fail(`Chat shows no "${label}" control`);
  };
  const linked = '(async()=> { const text = await navigator.clipboard.readText(); return text.includes("/app/#/@") && text.includes(fixture.room); })()';
  await pressWhenShown('Copy resource link');
  try {
    // The host is asked for its web entry on every copy; Chat gives up after 30 s.
    await tab.waitFor(linked, {timeout:40_000});
  } catch (error) {
    console.error('Clipboard read result', await tab.evaluate('navigator.clipboard.readText().then(text=>({length:text.length, linked:text.includes("/app/#/@")})).catch(error=>String(error))'));
    // Chat says under the message why it has no link.
    await tab.evaluate('mount.requestSemantics()');
    console.error('Shown text', await tab.evaluate('[...document.querySelectorAll("#surface [aria-label], #surface [role=status], #surface [role=alert]")].map(node => node.getAttribute("aria-label") || node.textContent).filter(Boolean).slice(-60)'));
    console.error('Copy diagnostic', await tab.evaluate('({bridge:mountDiagnostic, state:mount.state(), active:document.hasFocus()})'), tab.log);
    throw error;
  }
  const copied = await tab.evaluate('navigator.clipboard.readText()');
  assert.equal(new URL(copied).origin, origin, 'the link is under the web entry the host announces');
  assert.match(new URL(copied).hash, /\/so\.tilo\.chat\/-\/chat\.messages\.messages\/.+\?chars=6-7$/,
    'the link names the shared message and its exact range');
  stage('native-copy-to-real-browser-clipboard');
  await pressWhenShown('Open resource in browser');
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
    const offlineRead = await tab.evaluate(READ_MESSAGES);
    assert.equal(offlineRead.state, 'completed', JSON.stringify(offlineRead));
    assert.equal(offlineRead.output.value.messages.filter(message =>
      message.text === 'Native fixture while browser disconnected').length, 0,
    'the disconnected browser must not already have received the new native post');
    const queued = await tab.evaluate(`mount.invokeAction(fixture.messages, 'post',
      {channel:fixture.channelId, text:'Browser fixture queued during disconnect'})`);
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
      const messages = result.output.value.messages;
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
  await tab.evaluate('(async()=>{await mount.cleanup(); await peer.dispose(); return document.querySelectorAll("canvas").length;})()');
  const link = copied;
  // The full-tab entry page marks its outcome on <html data-entry>: open,
  // refused (no access) or failed.
  const entryOutcome = 'document.documentElement.dataset.entry';
  const waitForAccounts = async () => {
    const outcome = await tab.waitFor(`document.querySelectorAll(".tilo-entry-card button").length === 2
      ? 'accounts' : (${entryOutcome} || false)`, {timeout:15_000});
    assert.equal(outcome, 'accounts', `entry settled as ${outcome} instead of offering both fixture accounts`);
  };
  const chooseAccount = async name => {
    await waitForAccounts();
    await tab.evaluate(`Array.from(document.querySelectorAll(".tilo-entry-card button")).find(button => button.textContent.includes(${JSON.stringify(name)})).click()`);
  };
  observer.mark('navigate-link');
  await tab.navigate(link);
  await chooseAccount('Unadmitted');
  await tab.waitFor(`${entryOutcome} !== undefined`);
  assert.equal(await tab.evaluate(entryOutcome), 'refused');
  assert.equal(await tab.evaluate('document.querySelectorAll("canvas").length'), 0);
  stage('explicit-unadmitted-account-refused-without-enrollment');
  await tab.navigate(origin + '/fixture.html');
  await tab.navigate(link);
  observer.mark('reader-submit');
  await chooseAccount('Fixture reader');
  await tab.waitFor(`${entryOutcome} !== undefined`, {timeout:45_000});
  assert.equal(await tab.evaluate(entryOutcome), 'open', 'the admitted reader must reopen the shared resource');
  assert.ok(await tab.evaluate('document.querySelector("#surface canvas")?.width > 0'));
  assert.equal(await tab.evaluate('location.href'), link, 'the opened page keeps the exact link in its address');
  stage('production-account-choice-and-resource-page-mount');
  await tab.navigate(origin + '/fixture.html');
  assert.equal(await tab.evaluate('document.querySelectorAll("canvas").length'), 0);
  stage('leaving-the-page-releases-mounted-app-and-device-custody');
  await tab.navigate(link);
  await waitForAccounts();
  stage('page-navigation-reopens-existing-account-picker');
  stage('revoke');
  assert.equal(JSON.parse((await lines.next()).value).revoked, true);
  observer.mark('revoked-reader-submit');
  await chooseAccount('Fixture reader');
  await tab.waitFor(`${entryOutcome} !== undefined`, {timeout:45_000});
  assert.equal(await tab.evaluate(entryOutcome), 'refused');
  assert.equal(await tab.evaluate('document.querySelectorAll("canvas").length'), 0);
  stage('governed-revocation-refuses-link-reopen');
  // A page that cannot run is not open. Created synchronously, vello's
  // pipelines held the browser's GPU thread and blocked the reopened page
  // for 24-31 s here; what remains under SwiftShader is a few seconds.
  assert.ok(observer.presentedMounts() >= 2, 'the main-thread observer must see both mounts present');
  const longest = observer.longestStall();
  assert.ok(longest.ms < 15_000, `the page's main thread stalled ${longest.ms} ms on ${longest.url}`);
  writeFileSync(join(evidence, 'journey.json'), JSON.stringify({mode: fixture.encryptedRestart ? 'certified-browser-encrypted-cold' : fixture.disconnectRetry ? 'certified-browser-disconnect-retry' : 'certified-browser-viewer',
    app:'so.tilo.chat', stages}, null, 2) + '\n');
} catch (error) {
  const page = await pageState();
  writeFileSync(join(evidence, 'failure.json'), JSON.stringify({phase:'error',
    lastStage:stages.at(-1) ?? null, pageStatus:String(page.status ?? '').slice(0, 500),
    gpuStep: page.gpuStep ?? null, error:String(error).slice(0, 500)}, null, 2) + '\n');
  throw error;
} finally {
  clearTimeout(deadline);
  input.close();
  process.stdin.pause();
  await observer?.finish().catch(error => console.error('main-thread evidence unavailable', error));
  await browser?.close();
  await new Promise(done => server.close(done));
}

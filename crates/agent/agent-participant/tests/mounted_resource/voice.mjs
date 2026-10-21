/** Two isolated certified devices drive the actual compiled Chat call UI.
 * Audio is Chromium's synthetic device; all media and signalling are production. */
import assert from 'node:assert/strict';
import {createServer} from 'node:https';
import {readFileSync, existsSync, mkdirSync, writeFileSync} from 'node:fs';
import {resolve, join, extname} from 'node:path';
import {createInterface} from 'node:readline';
import {performance} from 'node:perf_hooks';
import {randomUUID} from 'node:crypto';
import {launch, sleep} from '../../../../../tools/web/chrome.mjs';
import {mountChat} from './mount-chat.mjs';

assert.notEqual(process.getuid?.(), 0, 'compiled voice requires an unprivileged sandboxed browser');
const bundle = resolve(process.argv[2]);
assert.ok(existsSync(join(bundle, 'app/index.html')), 'compiled full-tab entry page required');
const input = createInterface({input:process.stdin});
const lines = input[Symbol.asyncIterator]();
const credentials = JSON.parse((await lines.next()).value);
const types = {'.html':'text/html', '.mjs':'text/javascript', '.js':'text/javascript',
  '.css':'text/css', '.wasm':'application/wasm', '.json':'application/json'};
const server = createServer(credentials, (request, response) => {
  const path = new URL(request.url, 'https://localhost').pathname;
  if (path === '/fixture.html') {
    response.writeHead(200, {'content-type':'text/html'}).end('<!doctype html><style>html,body{margin:0;height:100%}#surface{position:fixed;inset:0}</style><div id="surface"></div>');
    return;
  }
  const file = resolve(bundle, '.' + path);
  if (!file.startsWith(bundle + '/') || !existsSync(file)) {response.writeHead(404).end(); return;}
  response.writeHead(200, {'content-type':types[extname(file)] ?? 'application/octet-stream'}).end(readFileSync(file));
});
await new Promise(done => server.listen(0, '127.0.0.1', done));
const origin = `https://127.0.0.1:${server.address().port}`;
process.stdout.write(JSON.stringify({origin}) + '\n');
const fixture = JSON.parse((await lines.next()).value);
const browsers = new Set();
const pages = [];
const stages = [];
const checkpoints = [];
const started = performance.now();
const runId = randomUUID();
let currentCheckpoint = 'setup';
let currentStage = 'setup';
const stage = name => {currentStage = name; stages.push(name); checkpoint(name); process.stdout.write(JSON.stringify({stage:name}) + '\n');};
const evidence = resolve('target/mounted-resource-voice');
mkdirSync(evidence, {recursive:true});
const checkpoint = (name, details = {}) => {
  currentCheckpoint = name;
  checkpoints.push({name, elapsedMs:Math.round(performance.now() - started), ...details});
  writeFileSync(join(evidence, 'progress.json'), JSON.stringify({runId, stage:currentStage, checkpoint:currentCheckpoint, checkpoints}, null, 2) + '\n');
};
const close = async () => {await Promise.allSettled([...browsers].map(browser => browser.close())); browsers.clear();};
const deadline = setTimeout(() => {
  writeFileSync(join(evidence, 'failure.json'), JSON.stringify({runId, stage:currentStage, checkpoint:currentCheckpoint,
    error:'DeadlineExceeded', elapsedMs:Math.round(performance.now() - started), checkpoints}, null, 2) + '\n');
  console.error('Mounted Chat voice exceeded its 130-second bound at', currentStage, 'checkpoint', currentCheckpoint);
  void close().finally(() => process.exit(1));
}, 130_000);
deadline.unref();

// A browser with `prompt` left out has no automatic answer to the
// microphone prompt: the journey sets the page's permission itself, as the
// person would in the site settings.
async function open(device, {prompt = true} = {}) {
  const browser = await launch({args:['--ignore-certificate-errors', '--enable-features=Vulkan',
    '--use-vulkan=swiftshader', '--use-angle=swiftshader', '--use-fake-device-for-media-stream',
    ...(prompt ? ['--use-fake-ui-for-media-stream'] : []), '--autoplay-policy=no-user-gesture-required',
    ]});
  browsers.add(browser);
  if (!prompt) await microphone(browser, 'denied');
  const tab = await browser.tab();
  pages.push(tab);
  tab.browser = browser;
  await tab.navigate(origin + '/fixture.html');
  await tab.waitFor('document.readyState === "complete"');
  await tab.send('Page.bringToFront');
  const bootstrap = {...fixture, ...device};
  const supplied = await tab.send('Runtime.evaluate', {expression:`window.fixture=${JSON.stringify(bootstrap)};void 0`});
  assert.equal(supplied.result?.exceptionDetails, undefined, 'fixture bootstrap failed');
  const initialized = await mountChat(tab);
  assert.equal(initialized.error, undefined, 'compiled Chat mount failed');
  await tab.waitFor('mount.state().frames > 0 && document.querySelector("canvas")?.width > 0');
  assert.ok((await tab.screenshot())?.length, 'the actual compiled Chat must present a frame');
  return tab;
}

async function microphone(browser, setting) {
  const reply = await browser.send('Browser.setPermission', {permission:{name:'microphone'}, setting, origin});
  assert.equal(reply.error, undefined, `the microphone permission must be ${setting}`);
}

async function action(tab, name, gesture = false) {
  const response = await tab.send('Runtime.evaluate', {expression:
    `mount.invokeAction(fixture.callRow, ${JSON.stringify(name)})`,
    returnByValue:true, userGesture:gesture});
  const thrown = response.result?.exceptionDetails;
  assert.equal(thrown, undefined, `public Chat action ${name} failed: ${thrown?.exception?.description ?? thrown?.exception?.value ?? thrown?.text ?? ''}`);
  const result = response.result.result.value;
  assert.equal(result.state, 'completed', `${name} must complete through the compiled app: ${JSON.stringify(result).slice(0, 400)}`);
  return result.output?.value;
}
const snapshot = tab => action(tab, 'call-status');
const inactive = tab => tab.evaluate('!peer.voice.call.stats().captureActive');
const resourcesClosed = tab => tab.evaluate('voiceEvidence.contexts.every(c=>c.state === "closed") && voiceEvidence.streams.every(s=>s.getTracks().every(t=>t.readyState === "ended")) && voiceEvidence.workers.every(w=>w.stopped)');
async function activateMuteControl(tab) {
  const response = await tab.send('Runtime.evaluate', {expression:`(() => {
    mount.requestSemantics();
    const button = Array.from(document.querySelectorAll('[data-tilo-semantic]')).find(element => element.getAttribute('aria-label') === 'Mute microphone');
    if (!button) throw Error('actual Chat microphone control is missing');
    button.click(); return true;
  })()`, returnByValue:true, userGesture:true});
  assert.equal(response.result?.exceptionDetails, undefined, 'native semantic microphone control must activate');
  assert.equal(response.result.result.value, true);
}
async function waitReported(tab, expression) {
  for (let attempt=0; attempt<100; attempt++) {
    const value = await snapshot(tab);
    if (expression(value)) return value;
    await sleep(100);
  }
  throw Error('compiled Chat did not report the expected real media state');
}

try {
  stage('mount-two-certified-compiled-chat-profiles');
  const first = await open({});
  const second = await open(fixture.secondDevice, {prompt:false});
  for (const tab of pages) {
    const controls = await tab.evaluate("mount.actions(fixture.callRow).map(a=>a.address.action)");
    for (const name of ['join-call','call-status','mute','unmute','leave-call']) assert.ok(controls.includes(name), `public ${name} missing`);
    await action(tab, 'join-call');
  }
  for (const tab of pages) {
    await tab.waitFor('peer.voice.call.state.phase === "live" && peer.voice.call.state.participants.length === 2', {timeout:30_000});
    const value = await waitReported(tab, value => value.reported.state === 'live');
    assert.equal(value.reported.muted, true);
    assert.equal(value.reported.capture_active, false);
    assert.equal(await inactive(tab), true);
    assert.equal(await tab.evaluate('voiceEvidence.streams.length'), 0, 'join must not request a microphone');
  }
  stage('actual-chat-joins-muted-without-capture');
  // The browser refuses the microphone: the person stays in the call,
  // muted, and the call says why; the other person is unaffected.
  await activateMuteControl(second);
  const refused = await waitReported(second, value => value.reported.issue === 'permission_denied');
  assert.equal(refused.reported.state, 'live', 'a refused microphone keeps the call');
  assert.equal(refused.reported.muted, true);
  assert.equal(refused.reported.capture_active, false);
  assert.equal(refused.clientIssue, null, 'Chat keeps the call: nothing failed');
  assert.equal(await second.evaluate('peer.voice.call.state.phase'), 'live');
  assert.equal(await second.evaluate('voiceEvidence.streams.length'), 0, 'no microphone was handed over');
  assert.equal(await first.evaluate('peer.voice.call.state.participants.length'), 2, 'the other person still hears the call');
  stage('actual-chat-refused-microphone-stays-in-the-call-muted');
  await microphone(second.browser, 'granted');
  await action(first, 'unmute', true);
  await activateMuteControl(second);
  for (const tab of pages) {
    await tab.waitFor('peer.voice.call.stats().captureActive === true', {timeout:10_000});
    await waitReported(tab, value => value.reported.capture_active && !value.reported.muted);
  }
  for (const tab of pages) {
    await tab.waitFor('Object.values(voiceEvidence.levels).some(count=>count > 5)', {timeout:15_000});
  }
  stage('compiled-chat-unmute-consent-and-bidirectional-decoded-audio');
  for (let index=0; index<pages.length; index++) {
    writeFileSync(join(evidence, `chat-call-${index}.png`), await pages[index].screenshot());
  }
  await activateMuteControl(second);
  await waitReported(second, value => value.reported.muted && !value.reported.capture_active);
  assert.equal(await inactive(second), true);
  assert.equal(await first.evaluate('peer.voice.call.stats().captureActive'), true, 'one device mute must not mute the other');
  stage('actual-chat-mute-stops-only-local-capture');
  await action(second, 'unmute', true);
  await waitReported(second, value => value.reported.capture_active);
  await action(second, 'leave-call');
  await second.waitFor('peer.voice.call.state.phase === "idle" && !peer.voice.call.stats().captureActive');
  await first.waitFor('peer.voice.call.state.participants.length === 1');
  assert.equal(await resourcesClosed(second), true, 'Leave closes every real browser audio resource');
  stage('actual-chat-leave-closes-tracks-worklets-and-carrier');
  await action(second, 'join-call');
  await second.waitFor('peer.voice.call.state.phase === "live" && peer.voice.call.state.participants.length === 2');
  assert.equal(await inactive(second), true, 'rejoin requires fresh consent');
  await action(second, 'unmute', true);
  await waitReported(second, value => value.reported.capture_active);
  stage('actual-chat-rejoin-requires-fresh-unmute');
  stage('revoke');
  checkpoint('await-owner-revocation-acknowledgement');
  const removal = JSON.parse((await lines.next()).value);
  assert.equal(removal.revoked, true);
  checkpoint('owner-revocation-acknowledged', {generation:removal.generation, hostElapsedMs:removal.elapsedMs});
  await first.waitFor('!peer.voice.call.stats().captureActive', {timeout:20_000});
  checkpoint('revoked-capture-stopped');
  await first.waitFor('voiceEvidence.contexts.every(c=>c.state === "closed") && voiceEvidence.streams.every(s=>s.getTracks().every(t=>t.readyState === "ended"))');
  assert.equal(await resourcesClosed(first), true, 'revocation closes every real browser audio resource');
  checkpoint('revoked-audio-resources-closed');
  assert.equal(await second.evaluate('peer.voice.call.stats().captureActive'), true, 'revocation must not stop another admitted person');
  checkpoint('other-person-still-capturing');
  await action(second, 'leave-call');
  checkpoint('other-person-leave-accepted');
  await second.waitFor('peer.voice.call.state.phase === "idle"');
  assert.equal(await resourcesClosed(second), true);
  stage('owner-revocation-stops-only-the-revoked-person');
  const observations = [];
  for (const tab of pages) observations.push(await tab.evaluate(`({
    frames:mount.state().frames,
    decodedAudioReportsAboveThreshold:Object.values(voiceEvidence.levels),
    captureRequests:voiceEvidence.streams.length,
    tracksEnded:voiceEvidence.streams.every(s=>s.getTracks().every(t=>t.readyState === 'ended')),
    contextsClosed:voiceEvidence.contexts.every(c=>c.state === 'closed'),
    workersStopped:voiceEvidence.workers.every(w=>w.stopped),
  })`));
  writeFileSync(join(evidence, 'journey.json'), JSON.stringify({runId, mode:'compiled-chat-browser-voice', app:'so.tilo.chat',
    devices:2, syntheticAudio:true, physicalMicrophone:false, decodedRmsThreshold:0.015,
    observations, stages, checkpoints}, null, 2) + '\n');
} catch (error) {
  // The fixture is ephemeral, so what failed can be said in full: a bare
  // error name leaves a timeout and a refused action indistinguishable.
  const reason = String(error?.message ?? error).slice(0, 500);
  writeFileSync(join(evidence, 'failure.json'), JSON.stringify({runId, stage:currentStage, checkpoint:currentCheckpoint, error:error.name, reason, checkpoints}, null, 2));
  throw Error(`Mounted Chat voice failed at ${currentStage} (checkpoint ${currentCheckpoint}): ${error.name}: ${reason}`);
} finally {
  clearTimeout(deadline);
  input.close();
  process.stdin.pause();
  await close();
  await new Promise(done => server.close(done));
}

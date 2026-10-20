// Two browser participants of one governed call (voice_browsers.rs serves
// the page and the call): each joins with a fake microphone, hears the
// other and the native participant, a mute stops what the muted one sends,
// and leaving takes it out of the call.
//
// usage: node driver.mjs FIXTURE_URL [--webkit]
//   FIXTURE_URL  the fixture's page origin (http://127.0.0.1:PORT)
//   --webkit     Playwright's WebKit (playwright-core from $PLAYWRIGHT_CORE)
//                instead of two headless Chrome profiles
// Prints one JSON line of what each page heard; exits non-zero on a failed
// expectation.
import assert from 'node:assert/strict';
import {mkdtempSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {launch, sleep} from '../../../../../tools/web/chrome.mjs';

const [base] = process.argv.slice(2).filter(arg => !arg.startsWith('--'));
const webkit = process.argv.includes('--webkit');
if (!base) throw new Error('usage: node driver.mjs FIXTURE_URL [--webkit]');
const fixture = async path => (await fetch(new URL(path, base))).text();
const mark = label => fetch(new URL(`/api/mark/${label}`, base), {method: 'POST'});

const MEDIA = ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--autoplay-policy=no-user-gesture-required'];

const browsers = new Set();

async function chromePage(who) {
  // CI's isolated test container runs as root; Chromium requires this flag
  // there. These profiles contain only generated fixture identities.
  result.stage = `${who}:launch`;
  console.error('voice fixture', result.stage);
  const browser = await launch({args: [...MEDIA, ...(process.getuid?.() === 0 ? ['--no-sandbox'] : [])]});
  browsers.add(browser);
  const close = async () => {if (browsers.delete(browser)) await browser.close();};
  try {
    result.stage = `${who}:tab`;
    console.error('voice fixture', result.stage);
    const tab = await browser.tab();
    result.stage = `${who}:navigate`;
    console.error('voice fixture', result.stage);
    await tab.navigate(`${base}/page.html?who=${who}`);
    result.stage = `${who}:ready`;
    console.error('voice fixture', result.stage);
    await tab.waitFor('window.ready === true', {timeout: 30_000});
    const gesture = async expression => {
      const reply = await tab.send('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true, userGesture: true});
      if (reply.error || reply.result.exceptionDetails) throw new Error(JSON.stringify(reply));
      return reply.result.result.value;
    };
    return {who, gesture, evaluate: tab.evaluate, waitFor: tab.waitFor, log: tab.log, close};
  } catch (error) {await close(); throw error;}
}

async function webkitPage(who) {
  const core = process.env.PLAYWRIGHT_CORE ?? 'playwright-core';
  const {webkit: engine} = await import(core.startsWith('/') ? pathToFileURL(resolve(core, 'index.mjs')).href : core);
  const context = await engine.launchPersistentContext(mkdtempSync(join(tmpdir(), `tilo-webkit-${who}-`)), {permissions: ['microphone']});
  const page = context.pages()[0] ?? await context.newPage();
  const log = [];
  page.on('pageerror', error => log.push(`EXC ${error.message}`));
  page.on('console', message => { if (['error', 'warning'].includes(message.type())) log.push(`${message.type()} ${message.text()}`); });
  await page.goto(`${base}/page.html?who=${who}`);
  await page.waitForFunction('window.ready === true', null, {timeout: 30_000});
  const evaluate = expression => page.evaluate(expression);
  const waitFor = async (expression, {timeout = 120_000} = {}) => (await page.waitForFunction(expression, null, {timeout, polling: 250})).jsonValue();
  const gesture = async expression => {await page.evaluate(() => {document.body.innerHTML = '<button id=activate>Activate microphone</button>';}); await page.click('#activate'); return evaluate(expression);};
  return {who, gesture, evaluate, waitFor, log, close: () => context.close()};
}

const open = webkit ? webkitPage : chromePage;
const call = await fixture('/api/call');
const channel = await fixture('/api/channel');
const alice = await fixture('/api/alice');
const pages = [];
const participation = page => page.evaluate('window.voice.state.participants.find(p => p.self)?.namespace.split("/")[1] ?? null');
const heard = (page, from) => page.evaluate(`window.received[${JSON.stringify(from)}] ?? 0`);
const result = {engine: webkit ? 'webkit' : 'chromium'};
// Fail and close these profiles before the Rust fixture's unchanged 120 s
// deadline can kill the driver. Never leave fake-microphone browsers behind.
const deadline = setTimeout(() => {
  console.error('voice fixture deadline exceeded', JSON.stringify(result));
  void Promise.allSettled([...browsers].map(browser => {browsers.delete(browser); return browser.close();})).finally(() => process.exit(1));
}, 105_000);
deadline.unref();
try {
  pages.push(await open('bob'));
  pages.push(await open('carol'));
  result.stage = 'call join';
  console.error('voice fixture', result.stage);
  const [bob, carol] = pages;
  for (const page of pages) await page.evaluate(`window.act({op: "join", channel_id: ${JSON.stringify(channel)}})`);
  for (const page of pages) await page.waitFor('window.voice.state.phase === "live" && window.voice.state.participants.length === 3', {timeout: 60_000});
  for (const page of pages) {
    assert.equal(await page.evaluate('window.voice.state.call'), call, 'shared controls joined the governed channel call');
    // A fresh profile never chose to be muted: joining turns the microphone on.
    assert.equal(await page.evaluate('window.voice.state.muted'), false, 'shared join starts with the microphone on');
    await page.waitFor('window.voice.stats().captureActive === true', {timeout: 10_000});
  }
  const ids = {bob: await participation(bob), carol: await participation(carol)};
  result.participations = ids;
  await mark(`bob=${ids.bob}`);
  await mark(`carol=${ids.carol}`);

  // Both ways between the browsers, and from the native participant.
  await bob.waitFor(`(window.received[${JSON.stringify(ids.carol)}] ?? 0) > 100`, {timeout: 60_000});
  await carol.waitFor(`(window.received[${JSON.stringify(ids.bob)}] ?? 0) > 100`, {timeout: 60_000});
  await bob.waitFor(`(window.received[${JSON.stringify(alice)}] ?? 0) > 100`, {timeout: 60_000});
  // The fake microphone beeps: each hears the other speak, decoded from Opus.
  await bob.waitFor(`window.heardSpeaking.has(${JSON.stringify(ids.carol)})`, {timeout: 30_000});
  await carol.waitFor(`window.heardSpeaking.has(${JSON.stringify(ids.bob)})`, {timeout: 30_000});
  result.heard = {bobFromCarol: await heard(bob, ids.carol), carolFromBob: await heard(carol, ids.bob), bobFromAlice: await heard(bob, alice)};
  result.carrier = {bob: await bob.evaluate('window.voice.stats().carrier'), carol: await carol.evaluate('window.voice.stats().carrier')};

  // Reopen a lost carrier and reconnect the certified session while actual
  // decoded audio continues in both directions.
  const reconnectAt = await heard(carol, ids.bob);
  assert.equal(await bob.evaluate('window.voice.networkChanged(); window.voice.stats().captureActive'), false, 'lost carrier immediately releases the microphone');
  await bob.waitFor('window.voice.stats().captureActive === true', {timeout: 10_000});
  await carol.waitFor(`(window.received[${JSON.stringify(ids.bob)}] ?? 0) > ${reconnectAt + 25}`, {timeout: 30_000});
  await bob.evaluate('window.reconnect()');
  await bob.waitFor('window.voice.state.phase === "live"', {timeout: 30_000});
  assert.equal(await participation(bob), ids.bob, 'session reconnect keeps its participation');
  // A real heartbeat renews the host's 60 s grant before expiration.
  await bob.waitFor('window.tokens.size >= 2', {timeout: 50_000});
  const decodedAt = {bob: await bob.evaluate(`window.speechCounts[${JSON.stringify(ids.carol)}] ?? 0`), carol: await carol.evaluate(`window.speechCounts[${JSON.stringify(ids.bob)}] ?? 0`)};
  const renewedAt = await heard(carol, ids.bob);
  await carol.waitFor(`(window.received[${JSON.stringify(ids.bob)}] ?? 0) > ${renewedAt + 25}`, {timeout: 30_000});
  await bob.waitFor(`(window.speechCounts[${JSON.stringify(ids.carol)}] ?? 0) > ${decodedAt.bob}`, {timeout: 30_000});
  await carol.waitFor(`(window.speechCounts[${JSON.stringify(ids.bob)}] ?? 0) > ${decodedAt.carol}`, {timeout: 30_000});
  result.renewed = {decodedSpeech: true, bobTokens: await bob.evaluate('window.tokens.size'), bobParticipation: await participation(bob)};

  // Carol mutes: Bob stops hearing her (frames in flight aside), not Alice.
  await carol.evaluate('window.act({op: "set_muted", muted: true})');
  assert.equal(await carol.evaluate('window.voice.stats().captureActive'), false, 'mute releases the fake microphone track');
  await mark('carol-muted');
  await sleep(500);
  const mutedAt = await heard(bob, ids.carol);
  const aliceAt = await heard(bob, alice);
  await sleep(2000);
  result.muted = {fromCarol: (await heard(bob, ids.carol)) - mutedAt, fromAlice: (await heard(bob, alice)) - aliceAt};
  assert.ok(result.muted.fromCarol <= 2, `Bob still hears muted Carol: ${result.muted.fromCarol} frames`);
  assert.ok(result.muted.fromAlice > 50, `Bob stopped hearing Alice: ${result.muted.fromAlice} frames`);
  await mark('carol-unmuted');
  await carol.gesture('window.act({op: "set_muted", muted: false})');
  const unmutedAt = await heard(bob, ids.carol);
  await bob.waitFor(`(window.received[${JSON.stringify(ids.carol)}] ?? 0) > ${unmutedAt + 25}`, {timeout: 30_000});

  // Carol leaves: Bob's roster shrinks to Alice and himself.
  await carol.evaluate('window.act({op: "leave"})');
  await mark('carol-left');
  await bob.waitFor('window.voice.state.participants.length === 2', {timeout: 30_000});
  result.left = {carolPhase: await carol.evaluate('window.voice.state.phase'), bobRoster: await bob.evaluate('window.voice.state.participants.length')};
  assert.equal(result.left.carolPhase, 'idle');
  await carol.waitFor('window.resources.contexts.every(c => c.state === "closed") && window.resources.streams.every(s => s.getTracks().every(t => t.readyState === "ended")) && window.resources.workers.every(w => w.stopped)', {timeout: 10_000});
  result.cleanup = {contexts: await carol.evaluate('window.resources.contexts.map(c => c.state)'), tracks: await carol.evaluate('window.resources.streams.flatMap(s => s.getTracks().map(t => t.readyState))'), workers: await carol.evaluate('window.resources.workers.every(w => w.stopped)')};
  await mark('done');
  result.logs = Object.fromEntries(pages.map(page => [page.who, page.log.filter(line => !/AudioContext was not allowed/.test(line))]));
  result.reports = {bob: await bob.evaluate('window.reports'), carol: await carol.evaluate('window.reports')};
  console.log(JSON.stringify(result));
} catch (error) {
  result.error = String(error?.stack ?? error);
  for (const page of pages) {
    result[page.who] = await page.evaluate('({state: window.voice.state, received: window.received, reports: window.reports, stats: window.voice.stats()})').catch(e => String(e));
    result[`${page.who}Log`] = page.log;
  }
  console.log(JSON.stringify(result, (key, value) => typeof value === 'bigint' ? String(value) : value));
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  for (const page of pages) await page.close().catch(() => {});
}

// One browser participant of one governed call (voice_browsers.rs serves the
// page and carries what it makes to the call's plane): the page joins with a
// fake microphone, publishes its frames as its plane's voice, sees the native
// participant's picture, sends its own camera to it, mutes, and leaves.
//
// usage: node driver.mjs FIXTURE_URL
//   FIXTURE_URL  the fixture's page origin (http://127.0.0.1:PORT)
// Prints one JSON line of what the page heard and saw; exits non-zero on a
// failed expectation.
import assert from 'node:assert/strict';
import {launch, sleep} from '../../../../../tools/web/chrome.mjs';

const [base] = process.argv.slice(2);
if (!base) throw new Error('usage: node driver.mjs FIXTURE_URL');
const fixture = async path => (await fetch(new URL(path, base))).text();
const mark = label => fetch(new URL(`/api/mark/${label}`, base), {method: 'POST'});

const MEDIA = ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream', '--autoplay-policy=no-user-gesture-required'];

const result = {engine: 'chromium'};
let browser;
let tab;
// Fail and close the profile before the Rust fixture's own deadline can kill
// the driver. Never leave a fake-microphone browser behind.
const deadline = setTimeout(() => {
  console.error('voice fixture deadline exceeded', JSON.stringify(result));
  void (browser?.close() ?? Promise.resolve()).finally(() => process.exit(1));
}, 105_000);
deadline.unref();
try {
  // CI's isolated test container runs as root; Chromium requires this flag
  // there. The profile contains only generated fixture identities.
  result.stage = 'launch';
  console.error('voice fixture', result.stage);
  browser = await launch({args: [...MEDIA, ...(process.getuid?.() === 0 ? ['--no-sandbox'] : [])]});
  tab = await browser.tab();
  result.stage = 'navigate';
  console.error('voice fixture', result.stage);
  await tab.navigate(`${base}/page.html?who=bob`);
  await tab.waitFor('window.ready === true', {timeout: 30_000});
  const evaluate = tab.evaluate;
  const waitFor = (expression, timeout = 60_000) => tab.waitFor(expression, {timeout});
  const gesture = async expression => {
    const reply = await tab.send('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true, userGesture: true});
    if (reply.error || reply.result.exceptionDetails) throw new Error(JSON.stringify(reply));
    return reply.result.result.value;
  };
  const call = await fixture('/api/call');
  const channel = await fixture('/api/channel');
  const native = await fixture('/api/native');

  result.stage = 'call join';
  console.error('voice fixture', result.stage);
  await gesture(`window.act({op: "join", channel_id: ${JSON.stringify(channel)}})`);
  // The page and the native device are the whole call.
  await waitFor('window.voice.state.phase === "live" && window.voice.state.participants.length === 2');
  assert.equal(await evaluate('window.voice.state.call'), call, 'shared controls joined the governed channel call');
  // Joining is listening, muted: the microphone is asked for on unmute.
  assert.equal(await evaluate('window.voice.state.muted'), true, 'shared join starts muted');
  assert.equal(await evaluate('window.voice.stats().captureActive'), false, 'joining captures nothing');
  await gesture('window.act({op: "set_muted", muted: false})');
  await waitFor('window.voice.stats().captureActive === true', 10_000);
  // What the microphone makes is published as the page's voice, frame by
  // frame, as it is made.
  await waitFor('window.publishedFrames > 20', 15_000);

  // The page's camera goes into the call, and the page shows the native
  // device's camera: each sees the other's picture.
  await gesture('window.act({op: "set_camera", on: true})');
  await waitFor('window.voice.state.camera === true', 15_000);
  const alice = await evaluate(`window.voice.state.participants.find(participant => participant.device === ${JSON.stringify(native)})?.user ?? null`);
  assert.ok(alice, 'the native participant is named in the page\'s call');
  await evaluate(`window.voice.showPictures([{person: ${JSON.stringify(alice)}, source: "camera", layer: "low"}])`);
  await waitFor('window.voice.stats().counters.pictures > 5');
  const seen = await evaluate('window.voice.takePictures(0, 10000000)');
  result.seen = seen.pictures.map(picture => ({person: picture.person, stream: picture.stream, source: picture.source, bytes: picture.object.length}));
  assert.ok(seen.pictures.some(picture => picture.stream.startsWith(native)), 'the native picture reached the page');
  assert.ok(seen.pictures.every(picture => picture.person === alice), 'the picture names the native participant');

  // The page's own pictures went out too: the native device shows its camera
  // in the call, so the page's camera encodes and publishes as it makes them.
  await sleep(1500);
  assert.ok((await evaluate('window.voice.stats().counters.sent')) > 5, 'the page publishes its own pictures as it encodes them');

  // Mutually: blocking the page stops what it sends.
  await mark('page-muted');
  await gesture('window.act({op: "set_muted", muted: true})');
  assert.equal(await evaluate('window.voice.stats().captureActive'), false, 'mute releases the fake microphone track');
  const mutedAt = await evaluate('window.publishedFrames');
  await sleep(300);
  assert.equal(await evaluate('window.publishedFrames'), mutedAt, 'a muted microphone publishes nothing');
  await mark('page-unmuted');
  await gesture('window.act({op: "set_muted", muted: false})');
  await waitFor(`window.publishedFrames > ${mutedAt}`);

  // Leaving ends the call here and closes everything it opened.
  await mark('page-left');
  await evaluate('window.act({op: "leave"})');
  await waitFor('window.voice.state.phase === "idle"', 15_000);
  await waitFor('window.resources.contexts.every(context => context.state === "closed") && window.resources.streams.every(stream => stream.getTracks().every(track => track.readyState === "ended")) && window.resources.workers.every(worker => worker.stopped)', 10_000);
  result.left = {
    phase: await evaluate('window.voice.state.phase'),
    contexts: await evaluate('window.resources.contexts.map(context => context.state)'),
    tracks: await evaluate('window.resources.streams.flatMap(stream => stream.getTracks().map(track => track.readyState))'),
    workers: await evaluate('window.resources.workers.every(worker => worker.stopped)'),
  };
  await mark('done');
  result.logs = tab.log.filter(line => !/AudioContext was not allowed/.test(line));
  result.reports = await evaluate('window.reports');
  console.log(JSON.stringify(result));
} catch (error) {
  result.error = String(error?.stack ?? error);
  result.state = await tab?.evaluate('window.voice?.state ?? null').catch(() => null);
  result.reports = await tab?.evaluate('window.reports ?? null').catch(() => null);
  result.played = await tab?.evaluate('window.played ?? null').catch(() => null);
  result.levels = await tab?.evaluate('window.levels ?? null').catch(() => null);
  result.speech = await tab?.evaluate('window.speechCounts ?? null').catch(() => null);
  result.stats = await tab?.evaluate('window.voice?.stats() ?? null').catch(() => null);
  result.logs = tab?.log ?? [];
  console.log(JSON.stringify(result));
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  await browser?.close().catch(() => {});
}

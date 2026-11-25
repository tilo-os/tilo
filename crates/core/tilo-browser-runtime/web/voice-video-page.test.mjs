// A page's call pictures in a real browser: the camera stream a call carries, at the camera's
// own frame rate, decoded with WebCodecs where the page's tile stands (`voice-video.mjs`'s
// `createPictureDecoder`, drawn by `drawPicture`). The stream is coded in the browser with the
// profile the native call's encoder writes (H.264, Annex B, the call's layers), so what is timed
// is the path a delivered picture takes.
//
// Needs Chrome (`CHROME`) with H.264: t's `web-page-tests` check runs it with the image's
// Chromium.
import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {launch, serve} from '../../../../tools/web/chrome.mjs';

const web = dirname(fileURLToPath(import.meta.url));

/** A page whose only tile is the canvas a call's picture is drawn in. */
function page() {
  const out = mkdtempSync(join(tmpdir(), 'voice-video-page-'));
  writeFileSync(join(out, 'index.html'), `<!doctype html><meta charset="utf-8">
<style>body{margin:0;background:#000}.tilo-call-picture{display:block;width:640px;height:360px}</style>
<canvas class="tilo-call-picture"></canvas>
<script type="module">
try {
const {createPictureDecoder, drawPicture} = await import('./voice-video.mjs');
const {encodeVideoObject} = await import('./video-profile.mjs');
const PERSON = 'ab'.repeat(32);
const WIDTH = 640, HEIGHT = 360, FPS = 30, FRAMES = 60;

// The pictures a call's camera makes, coded as the call codes them: H.264 among the layers
// the profile carries, each behind a tilo-video/1 header.
const source = new OffscreenCanvas(WIDTH, HEIGHT);
const context = source.getContext('2d');
const objects = [];
const chunks = [];
const encoder = new VideoEncoder({
  output: chunk => chunks.push(chunk),
  error: error => { globalThis.pageError = 'the encoder failed: ' + error; },
});
encoder.configure({
  codec: 'avc1.42E01F', width: WIDTH, height: HEIGHT, bitrate: 800_000, framerate: FPS,
  latencyMode: 'realtime', avc: {format: 'annexb'},
});
for (let index = 0; index < FRAMES; index++) {
  const timestamp = Math.round(index * 1e6 / FPS);
  context.fillStyle = '#203040';
  context.fillRect(0, 0, WIDTH, HEIGHT);
  context.fillStyle = '#e0a020';
  context.fillRect((index * 17) % (WIDTH - 80), 40, 80, 80);
  const frame = new VideoFrame(source, {timestamp});
  encoder.encode(frame, {keyFrame: index === 0 || index % FPS === 0});
  frame.close();
}
await encoder.flush();
for (const [index, chunk] of chunks.entries()) {
  const payload = new Uint8Array(chunk.byteLength);
  chunk.copyTo(payload);
  objects.push(encodeVideoObject({
    keyframe: chunk.type === 'key', layer: 'mid', sequence: index, timestampUs: chunk.timestamp,
    width: WIDTH, height: HEIGHT, payload,
  }));
}

// A call delivers its pictures as its camera makes them: one every 1000 / FPS ms. The page
// decodes each where its tile stands.
const canvas = document.querySelector('canvas');
const made = [];
const reports = [];
const decoder = createPictureDecoder({
  onFrame: (person, source, frame) => { made.push(performance.now()); drawPicture(canvas, frame); },
  report: value => reports.push(value.type),
});
await new Promise(done => {
  let fed = 0;
  const next = () => {
    if (fed >= objects.length) { done(); return; }
    decoder.admit({person: PERSON, source: 'camera', stream: 'device/1', object: objects[fed++]});
    setTimeout(next, 1000 / FPS);
  };
  next();
});
// The decoder hands pictures back on its own schedule: wait for the ones on their way.
for (let until = performance.now() + 3000; made.length < objects.length && performance.now() < until;) {
  await new Promise(resolve => setTimeout(resolve, 10));
}
const first = made[0];
const last = made.at(-1);
const seconds = (last - first) / 1000;
// The camera's colour at the frame's left edge, as the tile drew it.
const pixel = [...canvas.getContext('2d').getImageData(4, 4, 1, 1).data];
globalThis.result = {
  fed: objects.length, frames: made.length, fps: seconds > 0 ? (made.length - 1) / seconds : 0,
  reports, width: canvas.width, height: canvas.height, pixel,
};
} catch (error) {
  globalThis.pageError = String(error?.stack ?? error);
}
document.documentElement.dataset.page = globalThis.result ? 'ready' : 'failed';
</script>`);
  return out;
}

test('a camera the call delivers is decoded in the page at the camera\'s frame rate, drawn in its tile', async t => {
  const out = page();
  const server = await serve(out, {also: [web]});
  t.after(async () => {
    await browser?.close();
    await server.close();
    rmSync(out, {recursive: true, force: true});
  });
  let browser;
  try {
    browser = await launch();
  } catch (error) {
    if (String(error.message).startsWith('no Chrome')) {
      t.skip('no headless Chrome on this machine');
      return;
    }
    throw error;
  }
  const tab = await browser.tab();
  await tab.navigate(`${server.url}/index.html`);
  await tab.waitFor('globalThis.result !== undefined || globalThis.pageError !== undefined', {timeout: 180_000});
  const failed = await tab.evaluate('globalThis.pageError ?? null');
  assert.equal(failed, null, `the page decoded the call's pictures: ${failed}`);
  const result = await tab.evaluate('globalThis.result');
  console.log(`# ${result.frames} pictures decoded at ${result.fps.toFixed(1)} a second`);
  assert.equal(result.fed, 60, 'the call delivered every picture its camera made');
  assert.deepEqual(result.reports, [], 'no picture failed to decode');
  assert.equal(result.frames, 60, `every delivered picture was decoded and drawn: ${JSON.stringify(result)}`);
  assert.ok(result.fps >= 25, `the tile moves at the camera's rate: ${result.fps.toFixed(1)} frames a second`);
  assert.deepEqual([result.width, result.height], [640, 360], 'the picture is drawn at its own size');
  const [red, green, blue] = result.pixel;
  assert.ok(
    Math.abs(red - 0x20) <= 24 && Math.abs(green - 0x30) <= 24 && Math.abs(blue - 0x40) <= 24,
    `the tile shows the camera's picture: rgb(${result.pixel.slice(0, 3).join(',')})`,
  );
});

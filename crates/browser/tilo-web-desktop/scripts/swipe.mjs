// Two-finger sideways swipes on the browser desktop, as a Mac trackpad sends
// them: trusted pixel `wheel` events with deltaX, a small lead-in and a
// momentum tail. A swipe pans the strip under the fingers 1:1, momentum
// included, and stops at its ends; with nowhere to pan it scrolls the app
// under the pointer (Sheets) sideways; no swipe is left to the browser
// (history navigation). Reports the frames presented per second while a
// swipe pans.
//
// usage: node swipe.mjs BUNDLE_DIR [--webkit]
//   BUNDLE_DIR  build.sh --bundle desktop output
//   --webkit    Playwright's WebKit instead of headless Chrome; resolves
//               playwright-core from $PLAYWRIGHT_CORE, else from this tree
// Exits non-zero on the first failed expectation.
import assert from 'node:assert/strict';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const args = process.argv.slice(2);
const bundle = args.find(arg => !arg.startsWith('--'));
if (!bundle) throw Error('usage: node swipe.mjs BUNDLE_DIR [--webkit]');
const webkit = args.includes('--webkit');
const server = await serve(resolve(bundle));

// A page to drive: evaluate, one trusted wheel event, a screenshot.
async function chrome() {
  const browser = await launch();
  const tab = await browser.tab();
  await tab.navigate(`${server.url}/index.html`);
  return {
    evaluate: tab.evaluate,
    errors: () => tab.log.filter(line => line.startsWith('EXC')),
    move: (x, y) => tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y}),
    wheel: (x, y, deltaX, deltaY) => tab.send('Input.dispatchMouseEvent', {type: 'mouseWheel', x, y, deltaX, deltaY}),
    shot: async () => (await tab.screenshot()).toString('base64'),
    close: () => browser.close(),
  };
}
async function playwrightWebkit() {
  const core = process.env.PLAYWRIGHT_CORE ?? 'playwright-core';
  const {webkit: engine} = await import(core.startsWith('/') ? pathToFileURL(resolve(core, 'index.mjs')).href : core);
  // A persistent (throwaway) profile: WebKit refuses the runtime's storage
  // in an ephemeral one.
  const profile = mkdtempSync(join(tmpdir(), 'tilo-webkit-'));
  const context = await engine.launchPersistentContext(profile, {viewport: {width: 1280, height: 800}});
  const browser = {close: async () => {await context.close(); rmSync(profile, {recursive: true, force: true});}};
  const page = context.pages()[0] ?? await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(`EXC ${error.message}`));
  await page.goto(`${server.url}/index.html`);
  return {
    evaluate: expression => page.evaluate(expression),
    errors: () => errors,
    move: (x, y) => page.mouse.move(x, y),
    wheel: (x, y, deltaX, deltaY) => page.mouse.wheel(deltaX, deltaY),
    shot: async () => (await page.screenshot()).toString('base64'),
    close: () => browser.close(),
  };
}
const page = webkit ? await playwrightWebkit() : await chrome();
const {evaluate} = page;

for (let i = 0; ; i++) {
  await sleep(1000);
  if (await evaluate('Boolean(globalThis.tiloMount?.navigation)').catch(() => false)) break;
  assert.ok(i < 90, `the desktop never presented: ${await evaluate("document.getElementById('status')?.textContent")}`);
}
// Every wheel reaching the document after the surface: an uncancelled one is
// one the browser may turn into history navigation.
await evaluate(`globalThis.swipeLog=[];addEventListener('wheel',e=>swipeLog.push({x:e.deltaX,prevented:e.defaultPrevented}));true`);
// The strip's offset each animation frame, while tracing.
await evaluate(`globalThis.panTrace=null;(function frame(t){if(globalThis.panTrace)panTrace.push([t,tiloMount.navigation.getSnapshot().desktopPan?.offset??null,tiloMount.state()?.frames??null]);requestAnimationFrame(frame);})(0);true`);

const snapshot = () => evaluate('tiloMount.navigation.getSnapshot()');
const command = body => evaluate(`(async()=>{const s=tiloMount.navigation.getSnapshot();await tiloMount.navigation.dispatch({...${JSON.stringify(body)},generation:s.generation});return true;})()`);
const strip = async () => {
  const s = await snapshot();
  const windows = s.entries.filter(e => e.route.workspace === s.active.workspace && e.route.strip === s.active.strip);
  return {windows, at: windows.findIndex(e => e.route.tab === s.active.tab), pan: s.desktopPan};
};

const {width, height} = await evaluate('(({width,height})=>({width,height}))(tiloMount.canvas.getBoundingClientRect())');
const [cx, cy] = [Math.round(width / 2), Math.round(height / 2)];
await page.move(cx, cy);
// Page deltaX of one Mac trackpad swipe: the fingers' lead-in, then the OS
// momentum decaying after they lift. Positive deltaX moves toward later
// windows, as it scrolls a page right.
const PROFILE = [1, 2, 4, 7, 11, 14, 16, 15];
for (let v = 15; v > 0.75; ) { v *= 0.9; PROFILE.push(Math.round(v * 4) / 4); }
const TRAVEL = PROFILE.reduce((sum, dx) => sum + dx, 0);
let wheels = 0;
async function swipe(sign, profile = PROFILE) {
  const sent = await evaluate('swipeLog.length');
  for (const dx of profile) {
    await page.wheel(cx, cy, sign * dx, 0);
    wheels++;
    await sleep(16);
  }
  // Longer than the surface's swipe gap, so the next swipe is a new one.
  await sleep(400);
  // The page's deltaX sum, as the engine delivered the wheels.
  return (await evaluate('swipeLog')).slice(sent).reduce((sum, e) => sum + e.x, 0);
}

// Enough windows to overflow the viewport, Sheets among them.
const apps = (await snapshot()).applications.map(a => a.id);
for (const [i, app] of ['so.tilo.calculator', 'so.tilo.notes', 'so.tilo.sheets', 'so.tilo.writer', 'so.tilo.calendar'].entries()) {
  if (i >= 3 && (await strip()).pan?.max > TRAVEL * 2) break;
  assert.ok(i >= 3 || apps.includes(app), `${app} is in this bundle: ${apps}`);
  if (!apps.includes(app)) continue;
  await command({kind: 'launch', app});
  await sleep(1500);
}
let s = await strip();
assert.ok(s.pan?.max > TRAVEL * 2, `the strip overflows by more than two swipes: ${JSON.stringify(s.pan)}`);
await command({kind: 'select', route: s.windows[0].route});
await sleep(600);
s = await strip();
assert.ok(s.pan.offset < 0.5, `the first window is at home: ${JSON.stringify(s.pan)}`);

await evaluate('panTrace=[];true');
const started = Date.now();
const forward = await swipe(1);
const trace = await evaluate('const t=panTrace;panTrace=null;t');
const elapsed = Date.now() - started - 400;
s = await strip();
assert.ok(forward > 0, `the engine delivered a forward swipe: ${forward}`);
assert.ok(Math.abs(s.pan.offset - forward) < 1, `the strip panned 1:1 with the fingers: ${s.pan.offset} for ${forward} px of deltaX`);
assert.equal(s.at, 0, 'panning does not choose a window');
const moving = trace.filter(([, offset]) => offset != null && offset > 0.5 && offset < forward - 0.5);
const steps = new Set(moving.map(([, offset]) => offset.toFixed(1))).size;
assert.ok(steps >= 8, `the strip moved continuously, not in one step: ${steps} distinct offsets`);
const first = trace.find(([, , frames]) => frames != null), last = trace.findLast(([, , frames]) => frames != null);
const presented = first && last ? last[2] - first[2] : undefined;
const panFrames = moving.length;
// The driver's round trips pace the wheels, so frames per wheel event says
// whether rendering keeps up; per second is bounded by the input rate here.
console.log(JSON.stringify({step: 'pan', engine: webkit ? 'webkit' : 'chrome', offset: s.pan.offset, deltaX: forward, distinctOffsets: steps,
  wheels: PROFILE.length, wheelsPerSecond: Math.round(PROFILE.length / (elapsed / 1000)), animationFramesWhileMoving: panFrames,
  rafPerSecond: Math.round(trace.length / (elapsed / 1000)), presentedFrames: presented,
  presentedPerSecond: presented == null ? null : Math.round(presented / (elapsed / 1000)),
  presentedPerWheel: presented == null ? null : +(presented / PROFILE.length).toFixed(2), ok: true}));

const back = await swipe(-1);
s = await strip();
assert.ok(Math.abs(s.pan.offset - (forward + back)) < 1, `swiping back pans back: ${s.pan.offset}`);

// A trackpad reports at 120 Hz; the driver cannot, so wheel the canvas
// from inside the page at that rate and count the frames presented.
const burst = await evaluate(`new Promise(done=>{
  const canvas=tiloMount.canvas,start=performance.now(),frames=tiloMount.state()?.frames??0,from=tiloMount.navigation.getSnapshot().desktopPan.offset;
  const offsets=new Set();let sent=0;
  const timer=setInterval(()=>{
    canvas.dispatchEvent(new WheelEvent('wheel',{deltaX:3,deltaY:0,deltaMode:0,clientX:${cx},clientY:${cy},bubbles:false,cancelable:true}));
    offsets.add(tiloMount.navigation.getSnapshot().desktopPan.offset.toFixed(1));
    if(++sent<240)return;
    clearInterval(timer);
    setTimeout(()=>{const seconds=(performance.now()-start)/1000;done({seconds,wheels:sent,wheelsPerSecond:Math.round(sent/seconds),
      presentedPerSecond:Math.round(((tiloMount.state()?.frames??0)-frames)/seconds),travel:tiloMount.navigation.getSnapshot().desktopPan.offset-from,distinctOffsets:offsets.size});},100);
  },1000/120);
})`);
assert.ok(burst.travel > 0, `a 120 Hz wheel run pans: ${JSON.stringify(burst)}`);
console.log(JSON.stringify({step: 'rate', ...burst, ok: true}));
await sleep(400);

// A long flick past the end stops the strip there.
const flick = Array.from({length: Math.ceil((s.pan.max * 2) / 40)}, () => 40);
await swipe(1, flick);
s = await strip();
assert.ok(Math.abs(s.pan.offset - s.pan.max) < 1, `a flick stops at the end: ${JSON.stringify(s.pan)}`);
await swipe(1);
assert.ok(Math.abs((await strip()).pan.offset - s.pan.max) < 1, 'and stays there');
console.log(JSON.stringify({step: 'ends', max: s.pan.max, ok: true}));

// Sheets alone: nowhere to pan, so the swipe scrolls its grid sideways.
for (const w of s.windows.filter(w => w.label.toLowerCase() !== 'sheets')) await command({kind: 'close', route: w.route});
await sleep(1500);
s = await strip();
assert.deepEqual(s.windows.map(w => w.label.toLowerCase()), ['sheets'], 'only Sheets is left');
await page.move(cx, cy);
await sleep(1000);
const before = await page.shot();
await sleep(600);
assert.equal(await page.shot(), before, 'the idle desktop is stable, so a changed frame is the swipe');
await swipe(1);
await swipe(1);
await sleep(600);
const scrolled = await page.shot();
assert.notEqual(scrolled, before, 'the swipe scrolled Sheets sideways');
s = await strip();
assert.ok(s.pan.max < 0.5 && s.pan.offset < 0.5, `and the strip, which fits, stayed home: ${JSON.stringify(s.pan)}`);
await swipe(-1);
await swipe(-1);
await sleep(600);
assert.notEqual(await page.shot(), scrolled, 'and back');
console.log(JSON.stringify({step: 'sheets', ok: true}));

const log = await evaluate('swipeLog');
// WebKit coalesces wheel events that arrive faster than it dispatches them.
if (webkit) assert.ok(log.length > wheels / 2 && log.length <= wheels, `the swipes reached the page: ${log.length} of ${wheels}`);
else assert.equal(log.length, wheels, 'every swipe event reached the page');
assert.deepEqual(log.filter(e => !e.prevented), [], 'no wheel over the desktop is left to browser navigation');
assert.deepEqual(page.errors(), [], 'no page exception');
console.log(JSON.stringify({step: 'history', wheels: log.length, ok: true}));
await page.close();
await server.close();

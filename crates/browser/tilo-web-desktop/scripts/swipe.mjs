// Two-finger sideways swipes on the browser desktop, as a Mac trackpad sends
// them: trusted `wheel` events with deltaX, a sub-threshold lead-in and a
// momentum tail. One swipe focuses one neighbouring window along the strip;
// with no window that way it scrolls the app under the pointer (Sheets)
// sideways; no swipe is left to the browser (history navigation).
//
// usage: node swipe.mjs BASE_URL
//   BASE_URL  where build.sh's desktop output is served (serve.mjs)
//   Chromium  started with --headless=new --enable-unsafe-webgpu
//             --remote-debugging-port=${CDP_PORT:-9333} and a fresh profile
// Exits non-zero on the first failed expectation.
import assert from 'node:assert/strict';

const [base] = process.argv.slice(2);
const port = process.env.CDP_PORT ?? '9333';
const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, {once: true}));
let id = 0;
const pending = new Map();
const errors = [];
ws.addEventListener('message', ({data}) => {
  const msg = JSON.parse(data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  else if (msg.method === 'Runtime.exceptionThrown') errors.push(msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text);
});
let sessionId;
const send = (method, params = {}) => new Promise((resolve, reject) => {
  const n = ++id; pending.set(n, msg => msg.error ? reject(Error(`${method}: ${msg.error.message}`)) : resolve(msg.result));
  ws.send(JSON.stringify({id: n, method, params, sessionId}));
});
const sleep = ms => new Promise(r => setTimeout(r, ms));
const evaluate = async expression => {
  const {result, exceptionDetails} = await send('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true});
  if (exceptionDetails) throw Error(`${expression}: ${exceptionDetails.exception?.description ?? exceptionDetails.text}`);
  return result.value;
};

const {targetId} = await send('Target.createTarget', {url: 'about:blank'});
({sessionId} = await send('Target.attachToTarget', {targetId, flatten: true}));
await send('Runtime.enable');
await send('Page.enable');
await send('Page.navigate', {url: `${base}/index.html`});
for (let i = 0; ; i++) {
  await sleep(1000);
  if (await evaluate("Boolean(globalThis.tiloMount?.navigation)")) break;
  assert.ok(i < 90, `the desktop never presented: ${await evaluate("document.getElementById('status')?.textContent")}`);
}
// Every wheel reaching the document after the surface: an uncancelled one is
// one the browser may turn into history navigation.
await evaluate(`globalThis.swipeLog=[];addEventListener('wheel',e=>swipeLog.push({x:e.deltaX,prevented:e.defaultPrevented}));true`);

const snapshot = () => evaluate('tiloMount.navigation.getSnapshot()');
const command = body => evaluate(`(async()=>{const s=tiloMount.navigation.getSnapshot();await tiloMount.navigation.dispatch({...${JSON.stringify(body)},generation:s.generation});return true;})()`);
const strip = async () => {
  const s = await snapshot();
  const windows = s.entries.filter(e => e.route.workspace === s.active.workspace && e.route.strip === s.active.strip);
  return {windows, at: windows.findIndex(e => e.route.tab === s.active.tab), active: s.active};
};

const {width, height} = await evaluate('(({width,height})=>({width,height}))(tiloMount.canvas.getBoundingClientRect())');
const [cx, cy] = [Math.round(width / 2), Math.round(height / 2)];
await send('Input.dispatchMouseEvent', {type: 'mouseMoved', x: cx, y: cy});
// Page deltaX of one Mac trackpad swipe, momentum included. Positive deltaX
// scrolls right; the runtime reads it negated, as winit does everywhere.
const PROFILE = [2, 5, 14, 30, 48, 55, 44, 30, 18, 10, 5, 2, 1];
let swipes = 0;
async function swipe(sign) {
  swipes++;
  for (const dx of PROFILE) {
    await send('Input.dispatchMouseEvent', {type: 'mouseWheel', x: cx, y: cy, deltaX: sign * dx, deltaY: 0});
    await sleep(16);
  }
  // Longer than the surface's swipe gap, so the next swipe is a new one.
  await sleep(400);
}

// Three windows or more, Sheets among them.
const apps = (await snapshot()).applications.map(a => a.id);
for (const app of ['so.tilo.calculator', 'so.tilo.notes', 'so.tilo.sheets']) {
  assert.ok(apps.includes(app), `${app} is in this bundle: ${apps}`);
  await command({kind: 'launch', app});
  await sleep(1500);
}
let s = await strip();
assert.ok(s.windows.length >= 3, `three windows to walk: ${JSON.stringify(s.windows.map(w => w.label))}`);
await command({kind: 'select', route: s.windows[0].route});
await sleep(300);
assert.equal((await strip()).at, 0);

await swipe(-1);
s = await strip();
assert.equal(s.at, 1, `one swipe right-to-left steps one window forward, momentum included (at ${s.at})`);
await swipe(-1);
assert.equal((await strip()).at, 2, 'the next swipe steps the next window');
await swipe(1);
assert.equal((await strip()).at, 1, 'swiping back steps back');
console.log(JSON.stringify({step: 'strip', windows: s.windows.length, ok: true}));

// Sheets alone: nowhere to step, so the swipe scrolls its grid sideways.
s = await strip();
for (const w of s.windows.filter(w => w.label.toLowerCase() !== 'sheets')) await command({kind: 'close', route: w.route});
await sleep(1500);
s = await strip();
assert.deepEqual(s.windows.map(w => w.label.toLowerCase()), ['sheets'], 'only Sheets is left');
const shot = async () => (await send('Page.captureScreenshot', {format: 'png'})).data;
await send('Input.dispatchMouseEvent', {type: 'mouseMoved', x: cx, y: cy});
await sleep(1000);
const before = await shot();
await sleep(600);
assert.equal(await shot(), before, 'the idle desktop is stable, so a changed frame is the swipe');
await swipe(1);
await swipe(1);
await sleep(600);
const scrolled = await shot();
assert.notEqual(scrolled, before, 'the swipe scrolled Sheets sideways');
assert.equal((await strip()).at, 0, 'and focus stayed on Sheets');
await swipe(-1);
await swipe(-1);
await sleep(600);
assert.notEqual(await shot(), scrolled, 'and back');
console.log(JSON.stringify({step: 'sheets', ok: true}));

const log = await evaluate('swipeLog');
assert.equal(log.length, PROFILE.length * swipes, 'every swipe event reached the page');
assert.deepEqual(log.filter(e => !e.prevented), [], 'no wheel over the desktop is left to browser navigation');
assert.deepEqual(errors, [], 'no page exception');
console.log(JSON.stringify({step: 'history', wheels: log.length, ok: true}));
await send('Target.closeTarget', {targetId});
ws.close();

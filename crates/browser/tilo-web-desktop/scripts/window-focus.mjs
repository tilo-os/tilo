// Choosing and closing windows on the browser desktop. With three windows
// open, a host's select makes each window active in turn, a press in a
// window's body (not its tab bar) makes that window active, and closing the
// first window leaves the desktop drawing.
//
// usage: node window-focus.mjs BUNDLE_DIR
//   BUNDLE_DIR  build.sh --bundle desktop output
// Exits non-zero on the first failed expectation.
import assert from 'node:assert/strict';
import {resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const bundle = process.argv[2];
if (!bundle) throw Error('usage: node window-focus.mjs BUNDLE_DIR');
const server = await serve(resolve(bundle));
// SwiftShader's WebGPU: a host GPU in headless Chrome can lose its device
// mid-run.
const browser = await launch({args: ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader']});
const tab = await browser.tab();
await tab.navigate(`${server.url}/`);
const {evaluate} = tab;

for (let i = 0; ; i++) {
  await sleep(1000);
  if (await evaluate('Boolean(globalThis.tiloMount?.navigation)').catch(() => false)) break;
  assert.ok(i < 90, `the desktop never presented: ${await evaluate("document.getElementById('status')?.textContent")}`);
}

const snapshot = () => evaluate('tiloMount.navigation.getSnapshot()');
const command = body => evaluate(`(async()=>{const s=tiloMount.navigation.getSnapshot();await tiloMount.navigation.dispatch({...${JSON.stringify(body)},generation:s.generation});return true;})()`);
const active = async () => {
  const s = await snapshot();
  return s.entries.find(e => e.route.tab === s.active?.tab)?.label;
};
// Frames can be far apart in a software-rendered headless browser: wait for
// the state rather than a fixed time.
async function until(label, condition) {
  for (let i = 0; i < 100; i++) {
    const value = await condition();
    if (value) return value;
    await sleep(100);
  }
  assert.fail(`${label}: ${JSON.stringify(await evaluate('tiloMount.state()'))} ${JSON.stringify((await snapshot()).entries)}`);
}
const failure = () => evaluate('tiloMount.state()?.error ?? null');
const click = async (x, y) => {
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  for (const type of ['mousePressed', 'mouseReleased'])
    await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
};

const apps = ['so.tilo.chat', 'so.tilo.notes', 'so.tilo.calculator'];
for (const app of apps) await command({kind: 'launch', app});
const windows = await until('three windows open', async () => {
  const s = await snapshot();
  return s.entries.length === apps.length && s.entries;
});

for (const target of [0, 2, 1, 2, 0]) {
  const {route, label} = windows[target];
  await command({kind: 'select', route});
  await until(`select ${label}`, async () => (await active()) === label);
}
console.log(JSON.stringify({step: 'select', ok: true}));

// The first window is active and at home; presses along the row land in
// the bodies of the windows beside it.
const {width, height} = await evaluate('(({width,height})=>({width,height}))(tiloMount.canvas.getBoundingClientRect())');
const before = await active();
const chosen = new Set([before]);
for (const x of [0.9, 0.6, 0.3, 0.1].map(f => Math.round(width * f))) {
  await click(x, Math.round(height * 0.6));
  await sleep(300);
  chosen.add(await active());
}
assert.ok(chosen.size > 1, `a press in another window's body made it active: still ${before}`);
assert.equal(await failure(), null, 'the presses kept the desktop drawing');
console.log(JSON.stringify({step: 'click', active: [...chosen], ok: true}));

const frames = await evaluate('tiloMount.state()?.frames ?? 0');
await command({kind: 'close', route: (await snapshot()).entries[0].route});
await until('the first window closed', async () => (await snapshot()).entries.length === apps.length - 1);
await until('a frame after the close', async () => (await evaluate('tiloMount.state()?.frames ?? 0')) > frames + 1);
assert.equal(await failure(), null, 'closing a window keeps the desktop drawing');
console.log(JSON.stringify({step: 'close', ok: true}));

assert.deepEqual(tab.log.filter(line => line.startsWith('EXC')), [], 'no page exception');
await browser.close();
await server.close();

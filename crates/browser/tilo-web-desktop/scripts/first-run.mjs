// A new member's first open of the browser desktop, in headless Chrome:
//
// 1. The desktop opens with the top bar and an empty workspace that shows the
//    welcome launchpad, and no window.
// 2. "Open an app" opens search with the apps listed.
// 3. The Notes card opens Notes, focused.
// 4. The overview opens from its chord, lists the window and offers a new
//    workspace; Escape closes it.
// 5. A reload restores the same window instead of the welcome.
// 6. A page that takes invitations (`mountDesktop({onInvite})`) gets the
//    launchpad's Invite.
//
// usage: node first-run.mjs BUNDLE_DIR
//   BUNDLE_DIR  build.sh --bundle desktop output
// Writes screenshots to SHOTS_DIR when set. Exits non-zero on any failure.
import {writeFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const [bundle] = process.argv.slice(2);
if (!bundle) throw Error('usage: node first-run.mjs BUNDLE_DIR');
const shots = process.env.SHOTS_DIR;

const server = await serve(resolve(bundle));
// SwiftShader's WebGPU, as the app-sdk lane runs the bundle: a host GPU in
// headless Chrome can lose its device mid-run.
const browser = await launch({args: ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader']});
const tab = await browser.tab();
const failures = [];
const check = (ok, what) => {
  console.log(`${ok ? 'ok' : 'FAIL'} ${what}`);
  if (!ok) failures.push(what);
};
const shot = async name => { if (shots) writeFileSync(`${shots}/first-run-${name}.png`, await tab.screenshot()); };

const state = () => tab.evaluate('globalThis.tiloMount?.state() ?? null');
const windows = () => tab.evaluate('globalThis.tiloMount.navigation.getSnapshot().entries.map(e => e.label)');
// Every semantic node as {name, widget, rect}, from the frame's snapshot.
const nodes = async () => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()');
  await sleep(500);
  return tab.evaluate(`(() => {
    const out = [];
    const node = (n, widget) => { out.push({name: n.name ?? '', widget, rect: n.rect ?? n.bounds ?? null}); (n.children || []).forEach(c => node(c, widget)); };
    const walk = n => { if (n.semantics) node(n.semantics, n.widget); (n.children || []).forEach(walk); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) walk(root);
    return out;
  })()`);
};
const named = async name => (await nodes()).find(n => n.name === name);

async function until(predicate, timeout = 20000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await predicate().catch(() => undefined);
    if (value) return value;
    if (Date.now() > deadline) return undefined;
    await sleep(250);
  }
}

async function key(key, code, keyCode, modifiers = 0) {
  await tab.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', key, code, windowsVirtualKeyCode: keyCode, modifiers});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key, code, windowsVirtualKeyCode: keyCode, modifiers});
  await sleep(500);
}

async function click(x, y) {
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
  await tab.send('Input.dispatchMouseEvent', {type: 'mousePressed', x, y, button: 'left', clickCount: 1});
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseReleased', x, y, button: 'left', clickCount: 1});
  await sleep(600);
}

/** Click the semantic node named `name` at its centre. */
async function press(name) {
  const node = await named(name);
  const rect = node?.rect;
  if (!rect) return false;
  const [x0, y0, x1, y1] = Array.isArray(rect) ? rect : [rect.x0 ?? rect.x, rect.y0 ?? rect.y, rect.x1 ?? rect.x + rect.width, rect.y1 ?? rect.y + rect.height];
  await click((x0 + x1) / 2, (y0 + y1) / 2);
  return true;
}

async function open(query = '') {
  await tab.navigate(`${server.url}/index.html${query}`);
  await tab.waitFor("/frames [1-9]/.test(document.getElementById('status')?.textContent ?? '')", {timeout: 300000});
  await sleep(2000);
}

try {
  await tab.send('Emulation.setDeviceMetricsOverride', {width: 1440, height: 900, deviceScaleFactor: 1, mobile: false});
  await open();

  // 1. Top bar and welcome, no window.
  await shot('welcome');
  check((await nodes()).some(n => n.widget === 'TopBarChrome' || n.name === 'Overview'), 'the top bar is drawn');
  check(await named('Welcome to Tilo'), 'the empty workspace welcomes');
  check((await windows()).length === 0, 'a first open has no window');

  // 2. Open an app.
  check(await press('Open an app'), '"Open an app" is a control');
  check(await until(async () => (await state())?.searchOpen), '"Open an app" opens search');
  check((await nodes()).some(n => n.name === 'Open Notes'), 'search lists the apps');
  await shot('open-app');
  await key('Escape', 'Escape', 27);
  check(await until(async () => !(await state())?.searchOpen), 'Escape closes search');

  // 3. A card opens its app.
  check(await press('Notes'), 'the Notes card is a control');
  check(await until(async () => JSON.stringify(await windows()) === '["Notes"]'), 'the Notes card opens Notes');
  check(!(await named('Welcome to Tilo')), 'the welcome leaves with the first window');
  await sleep(2000);
  await shot('notes');

  // 4. The overview.
  const mac = await tab.evaluate('/Mac/.test(navigator.platform)');
  // CDP modifier bits: Alt 1, Ctrl 2, Shift 8.
  await key('o', 'KeyO', 79, mac ? 1 | 2 : 1 | 8);
  check(await until(async () => await named('Overview')), 'the overview chord opens the overview');
  check(await named('New workspace'), 'the overview offers a new workspace');
  check((await nodes()).some(n => n.widget === 'OverviewPanel' && n.name === 'Notes'), 'the overview lists the window');
  await shot('overview');
  await key('Escape', 'Escape', 27);
  check(await until(async () => !(await named('Overview'))), 'Escape closes the overview');

  // 5. Reload restores the layout.
  await tab.evaluate('globalThis.tiloMount.flush?.()');
  await open();
  check(JSON.stringify(await windows()) === '["Notes"]', 'a reload restores the window');
  check(!(await named('Welcome to Tilo')), 'a reload does not welcome again');
  await shot('restored');

  // 6. Invite reaches a page that takes it.
  await open('?room=bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb&workspace=Acme%20Robotics&invite');
  check(await named('Acme Robotics') || (await nodes()).some(n => n.name.includes('Acme Robotics')), 'the workspace takes the page\'s name');
  check(await press('Invite people'), 'the Invite card is a control');
  check(await until(async () => (await tab.evaluate('globalThis.tiloInvites')) > 0), 'Invite calls the page\'s onInvite');
  await shot('invite');

  const exceptions = tab.log.filter(line => line.startsWith('EXC') || /panic/i.test(line));
  check(exceptions.length === 0, `no exceptions or panics${exceptions.length ? `: ${exceptions.join(' | ')}` : ''}`);
} finally {
  await browser.close();
  server.close();
}
console.log(failures.length ? `${failures.length} failed` : 'all passed');
process.exit(failures.length ? 1 : 0);

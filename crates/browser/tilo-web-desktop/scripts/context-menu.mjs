// A secondary click on the browser desktop opens the menu of what it lands
// on, and never the browser's own. In the word processor's words that is the
// text's menu: cut, copy, paste, paste without formatting and select all. The
// right button opens it at the pointer, a Mac's Control-click does the same,
// the Menu key and Shift+F10 open it at the caret, and a row chosen with the
// pointer runs and closes it.
//
// usage: node context-menu.mjs BUNDLE_DIR [OUT_DIR]
//   BUNDLE_DIR  build.sh --bundle desktop output
//   OUT_DIR     where to keep a picture of the open menu
// Exits non-zero on the first failed expectation.
import assert from 'node:assert/strict';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';
import {verb} from '../../tilo-browser-runtime/web/desktop-navigation.mjs';

const [bundle, out] = process.argv.slice(2);
if (!bundle) throw Error('usage: node context-menu.mjs BUNDLE_DIR [OUT_DIR]');
if (out) mkdirSync(out, {recursive: true});
const server = await serve(resolve(bundle));
const browser = await launch();
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
// A Mac makes a secondary click with Control and the primary button. Off a Mac
// the page is told it is on one (its platform alone; the browser stays what it
// is), so the Control-click below is pressed wherever this runs.
const onMac = process.platform === 'darwin';
if (!onMac) await tab.send('Emulation.setUserAgentOverride', {userAgent: await tab.evaluate('navigator.userAgent'), platform: 'MacIntel'});
await tab.navigate(`${server.url}/`);
const {evaluate} = tab;

// Whatever happens, the browser and the server this started are put away; a
// step that failed leaves a picture of what was on the page instead.
let failure = null;
try {
  await run();
} catch (error) {
  failure = error;
  const shot = out && await tab.screenshot().catch(() => null);
  if (shot) writeFileSync(join(out, 'context-menu-failed.png'), shot);
} finally {
  await browser.close().catch(() => {});
  await server.close().catch(() => {});
}
if (failure) {
  console.error(failure.stack ?? String(failure));
  process.exit(1);
}

async function run() {
  for (let i = 0; ; i++) {
    await sleep(1000);
    if (await evaluate('Boolean(globalThis.tiloMount?.navigation)').catch(() => false)) break;
    assert.ok(i < 90, `the desktop never presented: ${await evaluate("document.getElementById('status')?.textContent")}`);
  }
  const snapshot = () => evaluate('tiloMount.navigation.getSnapshot()');
  const command = action => evaluate(`(async()=>{await tiloMount.navigation.dispatch(${JSON.stringify(action)});return true;})()`);
  // Every drawn semantic node: its role, its name and where it is on the page.
  const nodes = () => evaluate(`(async()=>{
    tiloMount.requestSemantics();
    await new Promise(done => setTimeout(done, 0));
    const state = tiloMount.state(), r = tiloMount.canvas.getBoundingClientRect(), size = state.logicalSize ?? [r.width, r.height];
    const sx = r.width / size[0], sy = r.height / size[1], found = [];
    const walk = (n, rect) => {
      if (!n || typeof n !== 'object') return;
      const s = n.semantics ?? n;
      if (s.state?.hidden) return;
      const own = n.rect ?? rect, b = s.bounds ? [s.bounds.x0, s.bounds.y0, s.bounds.x1 - s.bounds.x0, s.bounds.y1 - s.bounds.y0] : own;
      if (typeof s.role === 'string' && b) found.push({role: s.role, name: s.name ?? null, disabled: Boolean(s.state?.disabled),
        x: r.left + b[0] * sx, y: r.top + b[1] * sy, w: b[2] * sx, h: b[3] * sy});
      for (const child of n.children ?? []) walk(child, own);
      if (n.semantics) for (const child of n.semantics.children ?? []) walk(child, own);
    };
    walk(state.semantics?.root ?? state.semantics, null);
    return found;
  })()`);
  const rows = async () => (await nodes()).filter(n => n.role === 'MenuItem');
  // Frames can be far apart in a software-rendered headless browser: wait for
  // the state rather than a fixed time.
  async function until(label, condition, tries = 900) {
    for (let i = 0; i < tries; i++) {
      const value = await condition();
      if (value) return value;
      await sleep(100);
    }
    assert.fail(`${label}: ${JSON.stringify(await evaluate('tiloMount.state()?.error ?? null'))} ${JSON.stringify((await nodes()).filter(n => /Menu|Document/.test(n.role)))}`);
  }
  const mouse = (type, x, y, extra = {}) => tab.send('Input.dispatchMouseEvent', {type, x, y, ...extra});
  const press = async (x, y, button = 'left', modifiers = 0) => {
    await mouse('mouseMoved', x, y, {modifiers});
    await mouse('mousePressed', x, y, {button, buttons: button === 'right' ? 2 : 1, clickCount: 1, modifiers});
    await mouse('mouseReleased', x, y, {button, buttons: 0, clickCount: 1, modifiers});
  };
  const key = async (name, code, keyCode, modifiers = 0) => {
    for (const type of ['rawKeyDown', 'keyUp'])
      await tab.send('Input.dispatchKeyEvent', {type, key: name, code, windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode, modifiers});
  };
  const TEXT_ROWS = ['Cut', 'Copy', 'Paste', 'Paste without formatting', 'Select all'];
  const names = list => list.map(row => row.name);
  const menuOpen = label => until(`${label} opens the text's menu`, async () => {
    const shown = await rows();
    return TEXT_ROWS.every(name => names(shown).includes(name)) && shown;
  });
  const menuClosed = label => until(`${label} closes the menu`, async () => (await rows()).length === 0);
  const escape = async label => { await key('Escape', 'Escape', 27); await menuClosed(label); };

  await command(verb.openApp('so.tilo.word-processor'));
  const page = await until('the word processor showing its document', async () =>
    (await nodes()).find(n => n.role === 'Document' && n.w > 200 && n.h > 200));
  const words = {x: Math.round(page.x + page.w / 2), y: Math.round(page.y + Math.min(page.h / 2, 260))};
  await press(words.x, words.y);
  await tab.send('Input.insertText', {text: 'Hello world'});
  await sleep(500);

  // What the page hears of each request for a context menu: every one must be
  // answered for the browser, so its own menu never shows over the desktop.
  await evaluate(`(() => {
    globalThis.__menus = [];
    addEventListener('contextmenu', event => {
      const entry = {button: event.button, ctrl: event.ctrlKey};
      __menus.push(entry);
      setTimeout(() => { entry.prevented = event.defaultPrevented; }, 0);
    }, true);
    return true;
  })()`);

  // The right button, on the page.
  await press(words.x, words.y, 'right');
  let shown = await menuOpen('the right button');
  const near = row => Math.abs(row.x - words.x) < 320 && Math.abs(row.y - words.y) < 400;
  assert.ok(shown.filter(row => TEXT_ROWS.includes(row.name)).every(near), `the menu stands at the pointer: ${JSON.stringify(shown)}`);
  assert.equal(shown.find(row => row.name === 'Paste').disabled, false, 'Paste is offered without reading the clipboard');
  if (out) writeFileSync(join(out, 'context-menu.png'), await tab.screenshot());
  console.log(JSON.stringify({step: 'right-button', rows: names(shown), ok: true}));

  // A row chosen with the pointer runs and puts the menu away.
  const selectAll = shown.find(row => row.name === 'Select all');
  await press(Math.round(selectAll.x + selectAll.w / 2), Math.round(selectAll.y + selectAll.h / 2));
  await menuClosed('choosing Select all');
  // With the words selected, cut and copy are there to choose.
  await press(words.x, words.y, 'right');
  shown = await menuOpen('the right button, again');
  assert.deepEqual(shown.filter(row => ['Cut', 'Copy'].includes(row.name)).map(row => row.disabled), [false, false], 'a selection offers cut and copy');
  await escape('Escape');
  console.log(JSON.stringify({step: 'choose-row', ok: true}));

  // The keyboard: the Menu key, then Shift+F10.
  await key('ContextMenu', 'ContextMenu', 93);
  await menuOpen('the Menu key');
  await escape('Escape after the Menu key');
  await key('F10', 'F10', 121, 8);
  await menuOpen('Shift+F10');
  await escape('Escape after Shift+F10');
  console.log(JSON.stringify({step: 'keyboard', ok: true}));

  // A Mac's secondary click with one button: Control and the primary button.
  assert.ok(await evaluate('/Mac/.test(navigator.platform)'), 'the page takes itself to be on a Mac');
  await press(words.x, words.y, 'left', 2);
  await menuOpen('Control-click');
  await escape('Escape after Control-click');
  console.log(JSON.stringify({step: 'control-click', mac: onMac ? 'this machine' : 'platform emulated', ok: true}));

  await sleep(50);
  const asked = await evaluate('__menus');
  assert.ok(asked.length >= 2, `the page heard the requests for a context menu: ${JSON.stringify(asked)}`);
  assert.ok(asked.every(entry => entry.prevented), `the browser's own menu never shows over the desktop: ${JSON.stringify(asked)}`);
  assert.equal(await evaluate('tiloMount.state()?.error ?? null'), null, 'the desktop kept drawing');
  assert.deepEqual(tab.log.filter(line => line.startsWith('EXC')), [], 'no page exception');
  console.log(JSON.stringify({step: 'browser-menu', asked: asked.length, ok: true}));
}

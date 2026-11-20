// A link into another app goes to the window that already shows what it
// names. Mail's "Add an email account" leads to Settings: with Settings
// open, no second Settings window opens, the open one comes forward and
// shows External accounts; with none open, the link opens one there.
//
// usage: node app-link.mjs BUNDLE_DIR [OUT_DIR]
//   BUNDLE_DIR  build.sh --bundle desktop output
//   OUT_DIR     where to keep a picture of where the link led
// Exits non-zero on the first failed expectation.
import assert from 'node:assert/strict';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';
import {verb} from '../../tilo-browser-runtime/web/desktop-navigation.mjs';

const [bundle, out] = process.argv.slice(2);
if (!bundle) throw Error('usage: node app-link.mjs BUNDLE_DIR [OUT_DIR]');
if (out) mkdirSync(out, {recursive: true});
const server = await serve(resolve(bundle));
const browser = await launch();
const tab = await browser.tab();
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
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
  if (shot) writeFileSync(join(out, 'app-link-failed.png'), shot);
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
  const windowsOf = async app => (await snapshot()).entries.filter(entry => entry.app === app);
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
      if (b) found.push({role: typeof s.role === 'string' ? s.role : JSON.stringify(s.role ?? null), name: s.name ?? null,
        x: r.left + b[0] * sx, y: r.top + b[1] * sy, w: b[2] * sx, h: b[3] * sy});
      for (const child of n.children ?? []) walk(child, own);
      if (n.semantics) for (const child of n.semantics.children ?? []) walk(child, own);
    };
    walk(state.semantics?.root ?? state.semantics, null);
    return found;
  })()`);
  // Frames can be far apart in a software-rendered headless browser: wait for
  // the state rather than a fixed time.
  async function until(label, condition, tries = 900) {
    for (let i = 0; i < tries; i++) {
      const value = await condition();
      if (value) return value;
      await sleep(100);
    }
    assert.fail(`${label}: ${JSON.stringify(await evaluate('tiloMount.state()?.error ?? null'))} ${JSON.stringify((await snapshot()).entries)}`);
  }
  const click = async (x, y) => {
    await tab.send('Input.dispatchMouseEvent', {type: 'mouseMoved', x, y});
    for (const type of ['mousePressed', 'mouseReleased'])
      await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
  };
  const SETTINGS = 'so.tilo.settings', MAIL = 'so.tilo.mail';
  // Mail's own way to its accounts, where it is drawn on screen.
  const link = () => until('Mail offering its link to External accounts', async () => {
    const {width} = await evaluate('(({width}) => ({width}))(tiloMount.canvas.getBoundingClientRect())');
    return (await nodes()).find(n => n.role === 'Button' && n.name === 'Add an email account' && n.x >= 0 && n.x + n.w <= width);
  });
  // The page Settings shows: its title over the pane, on screen.
  const onAccounts = () => until('Settings showing External accounts', async () =>
    (await nodes()).some(n => /Heading/.test(n.role) && n.name === 'External accounts' && n.w > 0));

  await command(verb.openApp(SETTINGS));
  const [settings] = await until('Settings open', async () => { const open = await windowsOf(SETTINGS); return open.length === 1 && open; });
  await command(verb.openApp(MAIL));
  const [mail] = await until('Mail open', async () => { const open = await windowsOf(MAIL); return open.length === 1 && open; });
  await command(verb.focus(mail.route));
  await until('Mail in front', async () => (await snapshot()).active?.tab === mail.route.tab);
  assert.ok(!(await nodes()).some(n => /Heading/.test(n.role) && n.name === 'External accounts'), 'Settings starts on another page');

  // Settings is open: the link goes to its window.
  let button = await link();
  await click(Math.round(button.x + button.w / 2), Math.round(button.y + button.h / 2));
  await until('the open Settings window coming forward', async () => (await snapshot()).active?.tab === settings.route.tab);
  await onAccounts();
  assert.equal((await windowsOf(SETTINGS)).length, 1, `no second Settings window: ${JSON.stringify((await snapshot()).entries)}`);
  if (out) writeFileSync(join(out, 'app-link.png'), await tab.screenshot());
  console.log(JSON.stringify({step: 'reuses-the-open-window', settings: settings.route.tab, ok: true}));

  // Pressed again from Mail, it is still the one window.
  await command(verb.focus(mail.route));
  await until('Mail in front again', async () => (await snapshot()).active?.tab === mail.route.tab);
  button = await link();
  await click(Math.round(button.x + button.w / 2), Math.round(button.y + button.h / 2));
  await until('Settings in front again', async () => (await snapshot()).active?.tab === settings.route.tab);
  assert.equal((await windowsOf(SETTINGS)).length, 1, 'still one Settings window');
  console.log(JSON.stringify({step: 'pressed-again', ok: true}));

  // No Settings window: the link opens one, on External accounts.
  await command(verb.closeTab(settings.route));
  await until('Settings closed', async () => (await windowsOf(SETTINGS)).length === 0);
  await command(verb.focus(mail.route));
  button = await link();
  await click(Math.round(button.x + button.w / 2), Math.round(button.y + button.h / 2));
  const [opened] = await until('the link opening Settings', async () => { const open = await windowsOf(SETTINGS); return open.length === 1 && open; });
  await until('the new Settings window in front', async () => (await snapshot()).active?.tab === opened.route.tab);
  await onAccounts();
  console.log(JSON.stringify({step: 'opens-when-none-shows-it', ok: true}));

  assert.equal(await evaluate('tiloMount.state()?.error ?? null'), null, 'the desktop kept drawing');
  assert.deepEqual(tab.log.filter(line => line.startsWith('EXC')), [], 'no page exception');
}

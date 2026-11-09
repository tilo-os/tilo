// The entry page's first screen (web/first-run.mjs) in headless Chrome, at a
// phone's width and a laptop's, light and dark: it shows at the first paint
// with no runtime loaded, fits without sideways scrolling, its primary
// button is in view and copies inside the click, and the wait and the
// connected states read. Screenshots of each state go to SHOTS_DIR.
//
// usage: node first-screen.mjs [WEB_DIR]
//   WEB_DIR  the page's sources (default: ../web); no bundle is needed, since
//            the screen is the page's own markup.
// Exits non-zero on any failure.
import {mkdirSync, writeFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {launch, serve, sleep} from '../../../../tools/web/chrome.mjs';

const web = resolve(process.argv[2] ?? new URL('../web', import.meta.url).pathname);
const shots = process.env.SHOTS_DIR;
if (shots) mkdirSync(shots, {recursive: true});
const LINK = `https://tilo.sh/#/@eu1.tilo.sh/w/0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0?pair=${'5e'.repeat(32)}${'a7'.repeat(16)}`;

const server = await serve(web);
const browser = await launch();
const failures = [];
const check = (ok, what) => {
  console.log(`${ok ? 'ok' : 'FAIL'} ${what}`);
  if (!ok) failures.push(what);
};

for (const [width, height, mobile] of [[390, 844, true], [1280, 800, false]]) {
  for (const scheme of ['light', 'dark']) {
    const name = `${width}-${scheme}`;
    // A fresh profile each time: the screen is for a browser new here.
    const context = await browser.context();
    const tab = await browser.tab({context, firstRun: true});
    await tab.send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: 2, mobile});
    await tab.send('Emulation.setEmulatedMedia', {features: [{name: 'prefers-color-scheme', value: scheme}]});
    await tab.send('Browser.grantPermissions', {origin: server.url, browserContextId: context, permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite']}).catch(() => {});
    await tab.navigate(`${server.url}/`);
    await tab.waitFor(`document.readyState !== 'loading' && getComputedStyle(document.getElementById('first-run')).display !== 'none'`, {timeout: 20000});
    const layout = await tab.evaluate(`(() => {
      const button = document.getElementById('first-run-agent').getBoundingClientRect();
      return {sideways: document.documentElement.scrollWidth > innerWidth || document.getElementById('first-run').scrollWidth > innerWidth,
        inView: button.top >= 0 && button.bottom <= innerHeight, height: button.height, ground: getComputedStyle(document.getElementById('first-run')).backgroundColor};
    })()`);
    check(!layout.sideways, `${name}: nothing scrolls sideways`);
    check(layout.inView, `${name}: the primary button is in the first view`);
    check(layout.height >= 44, `${name}: the primary button is a comfortable target (${layout.height}px)`);
    check(scheme === 'light' ? layout.ground !== 'rgb(18, 18, 20)' : layout.ground === 'rgb(18, 18, 20)', `${name}: the ground follows the scheme (${layout.ground})`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}.png`, await tab.screenshot());

    // The page offers setup as it would once it is in; the click is a real
    // one, so the clipboard takes the prompt inside it.
    await tab.evaluate(`import('./first-run.mjs').then(({firstRun}) => {
      firstRun.offerSetup(async () => ({link: ${JSON.stringify(LINK)}, expiresAt: Date.now() + 10 * 60 * 1000}));
    })`);
    const at = await tab.evaluate(`(() => { const r = document.getElementById('first-run-agent').getBoundingClientRect(); return {x: r.x + r.width / 2, y: r.y + r.height / 2}; })()`);
    for (const type of ['mousePressed', 'mouseReleased']) {
      await tab.send('Input.dispatchMouseEvent', {type, x: at.x, y: at.y, button: 'left', clickCount: 1});
    }
    await tab.waitFor(`document.getElementById('first-run-prompt').textContent.includes('tilo login')`, {timeout: 10000});
    const copied = await tab.evaluate(`Promise.race([navigator.clipboard.readText().catch(() => null), new Promise(r => setTimeout(() => r(null), 3000))])`);
    check(copied === null || copied.includes(LINK), `${name}: the click copied the prompt with the link${copied === null ? ' (clipboard not readable here)' : ''}`);
    const waiting = await tab.evaluate(`({state: document.documentElement.dataset.firstRun, sideways: document.getElementById('first-run').scrollWidth > innerWidth,
      left: document.getElementById('first-run-left').textContent})`);
    check(waiting.state === 'waiting', `${name}: the screen waits for the agent`);
    check(!waiting.sideways, `${name}: the prompt wraps rather than scrolling sideways`);
    check(/Works once, for \d+:\d\d more/.test(waiting.left), `${name}: it says how long the prompt works (${waiting.left})`);
    await sleep(300);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-waiting.png`, await tab.screenshot());

    await tab.evaluate(`import('./first-run.mjs').then(({firstRun}) => firstRun.connected('Claude Code on laptop'))`);
    await sleep(600);
    check(await tab.evaluate(`document.getElementById('first-run-connected').textContent === 'Claude Code on laptop is connected'`),
      `${name}: the screen says which agent connected`);
    if (shots) writeFileSync(`${shots}/first-screen-${name}-connected.png`, await tab.screenshot());
    await tab.waitFor(`!document.getElementById('first-run')`, {timeout: 5000});
    check(await tab.evaluate(`localStorage.getItem('tilo-first-run') === '1'`), `${name}: the screen is not shown again`);
    await tab.close();
  }
}

await browser.close();
await server.close();
if (failures.length) {
  console.error(`${failures.length} failed`);
  process.exit(1);
}

// The web page boots for real: the built bundle in headless Chromium, served
// beside a running relay, as a person's browser loads it. The tab arrives,
// joins the Lounge's workspace, opens Notes from the search, and Notes'
// window shows. Then a sideways swipe over Sheets, whose grid scrolls
// sideways, scrolls Sheets and leaves the strip of windows where it is.
//
// What it catches is what compiles and passes every unit test yet stops a
// real page: the page's scripts calling a runtime function the wasm module
// does not export under that name, a read of the browser's storage before the
// page loaded it, or std's clock reached in wasm, which panics there. Any
// exception the page throws, and any panic or error its console reports,
// fails the run, even when the page went on.
//
// usage: node page-boot.mjs SITE_URL RELAY_ORIGIN OUT_DIR
//
// Prints a JSON report and keeps the tab's console and a screenshot in
// OUT_DIR; exits non-zero when a step did not happen or the page said it
// failed.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';

const [site, relay, out] = process.argv.slice(2);
if (!site || !relay || !out) {
  console.error('usage: node page-boot.mjs SITE_URL RELAY_ORIGIN OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const report = {site, relay, at: new Date().toISOString()};
// As the gate's other pages: the test certificates, a software GPU, a job
// container's small /dev/shm, and no sandbox where the run is root.
const browserArgs = [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
];
const browser = await launch({args: browserArgs});
// A person who has been past the first screen: their page opens the Lounge.
const tab = await browser.tab();

// What the page said that is not a script's exception or a console error:
// the browser's own log and requests that failed, kept for the report only.
const loading = [];
const asked = new Map();
browser.on(({sessionId, method, params}) => {
  if (sessionId !== tab.sessionId) return;
  if (method === 'Network.requestWillBeSent') asked.set(params.requestId, params.request.url);
  else if (method === 'Network.loadingFailed') {
    loading.push(`NET ${asked.get(params.requestId) ?? params.requestId}: ${params.errorText}`);
  } else if (method === 'Log.entryAdded') {
    loading.push(`LOG ${params.entry.level} ${params.entry.text}`.slice(0, 600));
  }
});
await tab.send('Network.enable', {}).catch(() => {});
await tab.send('Log.enable', {}).catch(() => {});
await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false})
  .catch(() => {});

// Every exception the page threw and every console line that reports a
// failure: an error, or any line that says the runtime panicked.
const failures = () => tab.log.filter(line => /^(EXC|ERROR) /.test(line) || /panic/i.test(line));

const finish = async (code, message) => {
  if (message) report.error = message;
  report.failures = failures();
  report.console = tab.log.slice(-60);
  report.loading = loading.slice(-30);
  report.text = await tab.evaluate(`(document.body.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 2000)`)
    .catch(() => '');
  writeFileSync(join(out, 'console.log'), [...tab.log, '--- loading', ...loading].join('\n'));
  const shot = await tab.screenshot().catch(() => null);
  if (shot) writeFileSync(join(out, 'page.png'), shot);
  writeFileSync(join(out, 'report.json'), JSON.stringify(report, null, 2));
  await browser.close().catch(() => {});
  if (message) {
    const said = tab.log.slice(-30);
    if (said.length) console.error(`--- the page said:\n${said.join('\n')}`);
    console.error(`FAIL: ${message}`);
  }
  console.log(JSON.stringify(report));
  process.exit(code);
};
const fail = message => finish(1, message);
// A step fails as soon as the page reports a failure, not only when the wait
// for what it should show runs out.
const checkPage = async step => {
  const found = failures();
  if (found.length) await fail(`the page failed while ${step}: ${found.slice(0, 5).join(' | ')}`);
};

// Wait until `expression` holds in the page, or fail naming where the page
// stopped instead.
const until = async (expression, what, timeout = 180_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    await checkPage(what);
    const value = await tab.evaluate(expression).catch(() => false);
    if (value) return value;
    if (Date.now() > deadline) {
      const shown = await tab.evaluate(`JSON.stringify({
        arrival: document.documentElement.dataset.arrival ?? null,
        door: document.documentElement.dataset.door ?? null,
        entry: document.documentElement.dataset.entry ?? null,
        status: document.getElementById('status')?.textContent?.trim().slice(0, 300) ?? null,
      })`).catch(() => 'unreadable');
      await fail(`${what} never happened: ${shown}`);
    }
    await sleep(250);
  }
};

// Keys as a person types them on the desktop.
const key = async (name, modifiers = 0) => {
  const named = {Enter: 13, Escape: 27};
  const code = name.length === 1 ? `Key${name.toUpperCase()}` : name;
  const event = {key: name, code, windowsVirtualKeyCode: named[name] ?? name.toUpperCase().charCodeAt(0), modifiers};
  await tab.send('Input.dispatchKeyEvent', {type: 'rawKeyDown', ...event, ...(name === 'Enter' ? {text: '\r'} : {})});
  await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', ...event});
};
const type = async text => {
  for (const ch of text) {
    await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch});
    await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
  }
};
// The names the search shows, in order, as the desktop publishes them.
const paletteRows = async () => {
  await tab.evaluate('globalThis.tiloMount?.requestSemantics?.()').catch(() => {});
  await sleep(300);
  return JSON.parse(await tab.evaluate(`(() => {
    let rows = null;
    const collect = node => {
      rows = [];
      const walk = semantics => { rows.push(semantics.name ?? ''); (semantics.children || []).forEach(walk); };
      if (node.semantics) walk(node.semantics);
    };
    const find = node => { if (node.widget === 'ActionPalette') collect(node); (node.children || []).forEach(find); };
    const root = globalThis.tiloMount?.state()?.semantics?.root;
    if (root) find(root);
    return JSON.stringify(rows);
  })()`).catch(() => 'null'));
};

// 1. The page boots and arrives.
await tab.navigate(site);
await until(`['done', 'stopped'].includes(document.documentElement.dataset.arrival)`, 'the page arriving');
const arrival = await tab.evaluate(`document.documentElement.dataset.arrival`);
if (arrival !== 'done') {
  await fail(`the page stopped: ${await tab.evaluate(`JSON.stringify({entry: document.documentElement.dataset.entry ?? null,
    status: document.getElementById('status')?.textContent?.trim().slice(0, 300) ?? null})`)}`);
}
console.log('ok: the page booted and arrived');

// 2. It joined the Lounge's workspace: a membership on the relay's host.
const membership = await until(`(() => {
  const all = JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}');
  const kept = all[${JSON.stringify(new URL(relay).origin)}];
  return kept?.room ? JSON.stringify(kept) : '';
})()`, 'joining the Lounge\'s workspace');
report.membership = JSON.parse(membership);
console.log(`ok: the page joined the workspace ${report.membership.room}`);

// 3. Notes opens from the search, and its window shows.
const mac = await tab.evaluate('/Mac/.test(navigator.platform)');
const box = JSON.parse(await tab.evaluate(`JSON.stringify((() => {
  const r = globalThis.tiloMount?.canvas?.getBoundingClientRect();
  return r ? {x: r.x + r.width / 2, y: r.y + r.height * 0.85} : {x: 640, y: 700};
})())`));
for (const type of ['mousePressed', 'mouseReleased']) {
  await tab.send('Input.dispatchMouseEvent', {type, x: box.x, y: box.y, button: 'left', clickCount: 1});
}
// Open `name` from the search, as a person does, and wait for its window.
const open = async (name, app) => {
  await key('k', mac ? 4 : 2);
  await until('!!globalThis.tiloMount?.state()?.searchOpen', `the search opening for ${name}`);
  await type(name.toLowerCase());
  // The rows shown right after typing can be the previous query's: Enter
  // waits until two reads agree that the app is the first result.
  const first = rows => (rows ? rows[rows.indexOf('Results') + 1] ?? null : null);
  const deadline = Date.now() + 120_000;
  for (;;) {
    await checkPage(`searching for ${name}`);
    const rows = await paletteRows();
    await sleep(300);
    const again = await paletteRows();
    if (first(rows) === name && JSON.stringify(rows) === JSON.stringify(again)) break;
    if (Date.now() > deadline) await fail(`the search offers no ${name} first: ${JSON.stringify(again)}`);
  }
  await key('Enter');
  await until(`(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === ${JSON.stringify(app)})`,
    `${name}'s window showing`);
};
await open('Notes', 'so.tilo.notes');
console.log('ok: Notes opened from the search');

// 4. Notes stays up a moment, as a person would look at it, and nothing the
// page did meanwhile failed.
await sleep(3000);
await checkPage('showing Notes');

// 5. A sideways swipe over an app whose content scrolls sideways is the
// app's: Sheets' grid scrolls and the strip of windows stays where it is. The
// strip overflows first (a few more windows), so it could pan; Sheets is
// opened last, from the search, and swiped right after, as the Lounge gate
// does it.
const frames = () => tab.evaluate('globalThis.tiloMount?.state()?.frames ?? 0').catch(() => 0);
const settled = async (by = 2) => {
  const from = await frames();
  await until(`(globalThis.tiloMount?.state()?.frames ?? 0) >= ${from + by}`, `the desk drawing ${by} more frame(s)`);
};
const panOf = async () => JSON.parse(await tab.evaluate(
  'JSON.stringify(globalThis.tiloMount?.navigation?.getSnapshot?.()?.desktopPan ?? null)').catch(() => 'null'));
for (const [name, app] of [['Calculator', 'so.tilo.calculator'], ['Clocks', 'so.tilo.clocks'], ['Weather', 'so.tilo.weather']]) {
  await open(name, app);
}
await open('Sheets', 'so.tilo.sheets');
await settled(2);
const centre = JSON.parse(await tab.evaluate(`JSON.stringify((() => {
  const r = globalThis.tiloMount?.canvas?.getBoundingClientRect();
  return {x: Math.round(r.x + r.width / 2), y: Math.round(r.y + r.height / 2)};
})())`));
const before = await panOf();
if (!before || !(before.max > 0)) await fail(`the strip does not overflow, so a pan could not show: ${JSON.stringify(before)}`);
const swipe = [];
for (const step of [6, 12, 18, 24, 18, 12, 6]) {
  await tab.send('Input.dispatchMouseEvent', {type: 'mouseWheel', x: centre.x, y: centre.y, deltaX: step, deltaY: 0});
  swipe.push((await panOf())?.offset ?? null);
}
await settled(2);
const after = await panOf();
// Where the strip goes with no input at all: a strip still settling from
// Sheets' opening moves here too, and that is no wheel's doing.
await sleep(1500);
const resting = await panOf();
report.sheets = {before, swipe, after, resting};
if (after?.offset !== before.offset) {
  await fail(`a sideways swipe over Sheets panned the strip from ${before.offset} to ${after?.offset} (each event: ${swipe.join(', ')}; at rest: ${resting?.offset})`);
}
await checkPage('swiping over Sheets');
console.log('ok: a sideways swipe over Sheets scrolls Sheets and leaves the strip');
report.frames = await frames();
await finish(0);

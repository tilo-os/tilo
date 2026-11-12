// The web page boots for real: the built bundle in headless Chromium, served
// beside a running relay, as a person's browser loads it. The tab arrives,
// joins the Lounge's workspace, opens Notes from the search, and Notes'
// window shows. Then its GPU device is lost, and the desk draws on with its
// windows. (A sideways swipe over Sheets is the people's desk case,
// people.mjs.)
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
import {clock} from './timing.mjs';
import {stepLimit, deskMetrics, DESK, machine} from './waits.mjs';

const [site, relay, out] = process.argv.slice(2);
if (!site || !relay || !out) {
  console.error('usage: node page-boot.mjs SITE_URL RELAY_ORIGIN OUT_DIR');
  process.exit(2);
}
mkdirSync(out, {recursive: true});
const mark = clock();
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
await tab.send('Emulation.setDeviceMetricsOverride', deskMetrics)
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
    console.error(`the machine as it failed: ${machine().text}`);
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
const until = async (expression, what, timeout = stepLimit(4)) => {
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
        // The mount's own account: which frame it last presented, and how
        // many frames its GPU left unreported — a page that stopped drawing
        // while it stayed alive says so here.
        frames: globalThis.tiloMount?.state()?.frames ?? null,
        gpuStalls: globalThis.tiloMount?.state()?.gpuStalls ?? null,
        phase: globalThis.tiloMount?.state()?.phase ?? null,
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
mark('the page arrived');

// 2. It joined the Lounge's workspace: a membership on the relay's host.
const membership = await until(`(() => {
  const all = JSON.parse(globalThis.localStorage.getItem('tilo-host-memberships') ?? '{}');
  const kept = all[${JSON.stringify(new URL(relay).origin)}];
  return kept?.room ? JSON.stringify(kept) : '';
})()`, 'joining the Lounge\'s workspace');
report.membership = JSON.parse(membership);
console.log(`ok: the page joined the workspace ${report.membership.room}`);
mark('the page joined the workspace');

// 3. Notes opens from the search, and its window shows.
const mac = await tab.evaluate('/Mac/.test(navigator.platform)');
const box = JSON.parse(await tab.evaluate(`JSON.stringify((() => {
  const r = globalThis.tiloMount?.canvas?.getBoundingClientRect();
  return r ? {x: r.x + r.width / 2, y: r.y + r.height * 0.85} : DESK.empty;
})())`));
for (const type of ['mousePressed', 'mouseReleased']) {
  await tab.send('Input.dispatchMouseEvent', {type, x: box.x, y: box.y, button: 'left', clickCount: 1});
}
// Open `name` from the search, as a person does, and wait for its window.
const open = async (name, app) => {
  const began = Date.now();
  await key('k', mac ? 4 : 2);
  await until('!!globalThis.tiloMount?.state()?.searchOpen', `the search opening for ${name}`);
  await type(name.toLowerCase());
  // The card the desk publishes for the query typed, once it owes no row:
  // nothing before that is its answer to this text (the rows shown right after
  // typing can be the previous query's), and Enter acts on its first row.
  const asked = JSON.stringify(name.toLowerCase());
  const deadline = Date.now() + stepLimit(3);
  let card = null;
  for (;;) {
    await checkPage(`searching for ${name}`);
    card = JSON.parse(await tab.evaluate(`(() => {
      const s = globalThis.tiloMount?.state?.() ?? null;
      const card = s?.searchCard ?? null;
      return JSON.stringify(card && s.searchSearching === false && card.query === ${asked} ? card : null);
    })()`).catch(() => 'null'));
    if (card?.rows?.[0]?.title === name) break;
    if (Date.now() > deadline) {
      await fail(`the search offers no ${name} first: ${JSON.stringify(card?.rows?.map(row => row.title) ?? await paletteRows())}`);
    }
    await sleep(250);
  }
  // Which frame showed the window, counted two ways: from the frame counter
  // the page reads when the browser fires Enter, and from the count the desk
  // says it had presented when it took the key (\`framesAtKey\`). A frame
  // presented between the two is the browser's event still on its way, not
  // the desk being slow.
  await tab.evaluate(`(() => {
    const mount = globalThis.tiloMount;
    const host = mount.canvas.parentElement;
    const w = globalThis.__bootOpen = {app: ${JSON.stringify(app)}, atKeydown: null, shown: null, atKey: null, trace: []};
    addEventListener('keydown', event => {
      if (event.key === 'Enter' && w.atKeydown === null) w.atKeydown = Number(host.dataset.frames ?? 0);
    }, {capture: true, once: true});
    const read = () => {
      if (w.atKeydown === null || w.shown !== null) return;
      const state = mount.state() ?? {};
      const frame = Number(host.dataset.frames ?? 0);
      if (w.trace.at(-1)?.frame !== frame) {
        w.trace.push({frame, atKey: state.framesAtKey ?? null, searchOpen: !!state.searchOpen,
          listed: (mount.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === w.app)});
      }
      if ((mount.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === w.app)) {
        w.shown = Number(host.dataset.frames ?? 0);
        w.atKey = mount.state()?.framesAtKey ?? null;
      }
    };
    new MutationObserver(read).observe(host, {attributes: true, attributeFilter: ['data-frames']});
    w.read = read;
  })()`);
  await key('Enter');
  await until(`(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.app === ${JSON.stringify(app)})`,
    `${name}'s window showing`);
  await until(`(() => { const w = globalThis.__bootOpen; w?.read?.(); return w?.shown !== null; })()`,
    `the frame showing ${name}'s window`, stepLimit(2));
  const counted = JSON.parse(await tab.evaluate('JSON.stringify(globalThis.__bootOpen)'));
  report.opens = [...(report.opens ?? []), counted];
  console.log(`frames: ${name} shown on frame ${counted.shown}; the page read ${counted.atKeydown} at Enter, the desk had presented ${counted.atKey} when it took the key; frames between: ${JSON.stringify(counted.trace)}`);
  console.log(`TIMING open=${app} seconds=${((Date.now() - began) / 1000).toFixed(1)}`);
};
await open('Notes', 'so.tilo.notes');
console.log('ok: Notes opened from the search');
mark('Notes opened');

// 4. Notes stays up a moment, as a person would look at it, and nothing the
// page did meanwhile failed.
await sleep(3000);
await checkPage('showing Notes');
mark('Notes looked at');

// What the desk drew since: its own frame count, waited on as frames, not a stopwatch.
const frames = () => tab.evaluate('globalThis.tiloMount?.state()?.frames ?? 0').catch(() => 0);
const settled = async (by = 2) => {
  const from = await frames();
  await until(`(globalThis.tiloMount?.state()?.frames ?? 0) >= ${from + by}`, `the desk drawing ${by} more frame(s)`);
};

// 5. The GPU device is lost mid-session, as a driver reset or the browser
// does it: the desk makes a new device and draws on, with every window it
// had, and a person can keep working there. A desktop that stopped instead
// says so in its phase.
const beforeLoss = {
  frames: await frames(),
  gpu: await tab.evaluate('globalThis.tiloMount?.state()?.gpuGeneration ?? null'),
  windows: (await tab.evaluate('JSON.stringify(globalThis.tiloMount?.navigation?.getSnapshot?.().entries?.map(entry => entry.app) ?? [])').then(JSON.parse)),
};
if (!await tab.evaluate('globalThis.tiloMount?.loseGpu?.() ?? false')) {
  await fail('the page had no GPU device to lose');
}
await until(`(() => {
  const state = globalThis.tiloMount?.state();
  return !!state && state.phase !== 'failed' && (state.gpuGeneration ?? 0) > ${beforeLoss.gpu ?? 0}
    && (state.frames ?? 0) > ${beforeLoss.frames};
})()`, 'the desk drawing again on a new GPU device after its device was lost', stepLimit(2));
await open('Calculator', 'so.tilo.calculator');
await settled(2);
const afterLoss = JSON.parse(await tab.evaluate(`JSON.stringify({
  phase: globalThis.tiloMount?.state()?.phase ?? null,
  error: globalThis.tiloMount?.state()?.error ?? null,
  gpu: globalThis.tiloMount?.state()?.gpuGeneration ?? null,
  windows: globalThis.tiloMount?.navigation?.getSnapshot?.().entries?.map(entry => entry.app) ?? [],
})`));
report.gpuLoss = {beforeLoss, afterLoss};
if (afterLoss.phase === 'failed' || afterLoss.error) {
  await fail(`the desktop stopped after its GPU device was lost: ${JSON.stringify(afterLoss)}`);
}
const kept = beforeLoss.windows.filter(app => !afterLoss.windows.includes(app));
if (kept.length) await fail(`windows were gone after the GPU device was lost: ${JSON.stringify(kept)}`);
await checkPage('drawing on after the GPU device was lost');
console.log('ok: a GPU device lost mid-session is made again, and the desk draws on with its windows');
mark('a GPU device lost and made again');
report.frames = await frames();
await finish(0);

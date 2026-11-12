// A person reloads their workspace page again and again in one browser: how
// long each old page takes to let go, what it was still doing as it went,
// and whether a reload ever crashes the tab.
//
//   node reloads.mjs SITE_URL OUT_DIR [TIMES]
//
// One returning person (past the first screen) enters the workspace the site
// opens, waits for the desk to arrive and draw, then reloads TIMES times
// (default 20, or TILO_RELOADS). For each reload it prints one line:
//
//   TIMING reload n=3 result=ok deviceLocks=#0 hide=12 handlers=4 unload=0 fetch=310 connect=0
//     request=320 response=360 body=380 protocol=h2 commit=420 arrived=5200 frame=6100
//
// all in ms: `hide` from the reload asked to the old page's first pagehide
// listener; `handlers` the time the old page's pagehide listeners took, from
// the first to one added after the page arrived (so after the runtime's own);
// `oldRanUntil` the last tick a timer the old page started in its pagehide
// ran, so how long it lived on after it hid; `gpuDevices` the GPU devices it
// had asked for; `unload` the old document's unload event (the new document's navigation
// timing); `fetch` the new document's fetchStart, which waits for the old one
// to let go; `connect`, `request`, `response`, `body` its connection's setup
// and when it asked, its first byte and its last; `commit` the new document committed (Page.frameNavigated);
// `arrived` the page says it arrived; `frame` its desk drew a frame. A reload
// whose tab crashed, or whose page never arrived, is counted and the run goes
// on in a fresh tab. It fails when any reload crashed or never arrived, when
// the reloads made more than one device, and when a second tab of the browser
// waits for the first, does not work beside it (each opens a window on the
// workspace's desk and each must see the other's), or when the first,
// reloaded beside it, is not the same device again. It prints the counts and
// the slowest reloads either way.
import {mkdirSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {launch, sleep} from '../../../../tools/web/chrome.mjs';
import {deskMetrics, machine, stepLimit} from './waits.mjs';

const [site, out, timesArg] = process.argv.slice(2);
if (!site || !out) {
  console.error('usage: node reloads.mjs SITE_URL OUT_DIR [TIMES]');
  process.exit(2);
}
const times = Number(timesArg ?? process.env.TILO_RELOADS ?? 20);
mkdirSync(out, {recursive: true});

// In every document the tab opens: a returning person, the first pagehide
// listener (registered before any of the page's own), and what the document
// left for the next one in sessionStorage.
//
// The GPU devices the page asks for are kept, so a reload the run marks
// (every second one with TILO_RELOADS_RELEASE_GPU=1) destroys them in
// that first pagehide: reloads that do and reloads that don't, side by side on
// the same machine, say whether an old page's GPU device is what holds the
// new one back.
const EARLY = `(() => {
  try { localStorage.setItem('tilo-setup', JSON.stringify({step: 'done'})); } catch {}
  const devices = [];
  const ask = globalThis.GPUAdapter?.prototype?.requestDevice;
  if (ask) {
    GPUAdapter.prototype.requestDevice = async function (...args) {
      const device = await ask.apply(this, args);
      devices.push(device);
      return device;
    };
  }
  addEventListener('pagehide', () => {
    try { sessionStorage.setItem('tilo-reload-hide', String(Date.now())); } catch {}
    try { sessionStorage.setItem('tilo-reload-devices', String(devices.length)); } catch {}
    if (sessionStorage.getItem('tilo-reload-release-gpu') === '1') {
      for (const device of devices) { try { device.destroy(); } catch {} }
    }
    // The old page still runs its tasks after it hid, until the browser
    // tears it down: the last tick it ran says how long it lived on.
    setInterval(() => { try { sessionStorage.setItem('tilo-reload-alive', String(Date.now())); } catch {} }, 25);
  }, {capture: true});
})();`;
// Added once the page arrived, so it runs after the runtime's own listeners.
const LATE = `(() => {
  addEventListener('pagehide', () => {
    try { sessionStorage.setItem('tilo-reload-hidden', String(Date.now())); } catch {}
  });
  return true;
})()`;
// What the new document can say about the one before it.
const BEFORE = `JSON.stringify((() => {
  const nav = performance.getEntriesByType('navigation')[0];
  const read = key => Number(sessionStorage.getItem(key) ?? NaN);
  return {
    origin: performance.timeOrigin,
    hide: read('tilo-reload-hide'),
    hidden: read('tilo-reload-hidden'),
    alive: read('tilo-reload-alive'),
    devices: read('tilo-reload-devices'),
    unload: nav ? nav.unloadEventEnd - nav.unloadEventStart : null,
    fetch: nav ? nav.fetchStart : null,
    connect: nav && nav.connectEnd > nav.connectStart ? nav.connectEnd - nav.connectStart : 0,
    request: nav ? nav.requestStart : null,
    response: nav ? nav.responseStart : null,
    body: nav ? nav.responseEnd : null,
    protocol: nav?.nextHopProtocol ?? null,
  };
})())`;
// The device keys this browser holds for the page's profile: the locks
// held on them now (one per tab: a tab is a device of its own), and the keys
// kept. A reload takes back the key the page before it held.
const DEVICE_LOCKS = `navigator.locks.query().then(state => JSON.stringify(state.held
  .map(lock => lock.name).filter(name => name.startsWith('tilo-device-custody:'))))`;
const DEVICE_KEYS = `new Promise(resolve => {
  const request = indexedDB.open('tilo-device', 1);
  request.onsuccess = () => {
    const db = request.result;
    if (!db.objectStoreNames.contains('custody')) return resolve('[]');
    const keys = db.transaction('custody').objectStore('custody').getAllKeys();
    keys.onsuccess = () => resolve(JSON.stringify(keys.result.filter(key => String(key).startsWith('device-key:'))));
    keys.onerror = () => resolve('[]');
  };
  request.onerror = () => resolve('[]');
})`;
const ARRIVED = `['done', 'stopped'].includes(document.documentElement.dataset.arrival)
  && document.documentElement.dataset.arrival`;
const FRAMES = 'globalThis.tiloMount?.state?.()?.frames ?? 0';

const browser = await launch({args: [
  '--ignore-certificate-errors',
  '--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader',
  '--disable-dev-shm-usage',
  ...(process.getuid?.() === 0 ? ['--no-sandbox'] : []),
]});
const crashed = new Set();
browser.on(({method, sessionId}) => {
  if (method === 'Inspector.targetCrashed') crashed.add(sessionId);
});

const until = async (tab, expression, ms) => {
  const deadline = Date.now() + ms;
  for (;;) {
    if (crashed.has(tab.sessionId)) return null;
    const value = await Promise.race([tab.evaluate(expression).catch(() => null), sleep(2_000).then(() => null)]);
    if (value) return value;
    if (Date.now() > deadline) return null;
    await sleep(50);
  }
};

const committed = new Map();
browser.on(({method, sessionId, params}) => {
  if (method === 'Page.frameNavigated' && !params.frame.parentId) committed.set(sessionId, Date.now());
});

const open = async () => {
  const tab = await browser.tab();
  await tab.send('Inspector.enable').catch(() => {});
  await tab.send('Emulation.setDeviceMetricsOverride', deskMetrics).catch(() => {});
  await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: EARLY});
  await tab.navigate(site);
  const arrived = await until(tab, ARRIVED, stepLimit(6));
  return {tab, arrived};
};

const lines = [];
const counts = {ok: 0, crashed: 0, neverArrived: 0, stopped: 0};
let {tab, arrived} = await open();
if (!arrived) {
  console.error(`FAIL: the first visit never arrived (the machine: ${machine().text})`);
  await browser.close().catch(() => {});
  process.exit(1);
}
console.log(`ok: the workspace page arrived (${arrived})`);
for (let n = 1; n <= times; n += 1) {
  await until(tab, `(${FRAMES}) > 0`, stepLimit(3));
  await tab.evaluate(LATE).catch(() => {});
  committed.delete(tab.sessionId);
  const release = process.env.TILO_RELOADS_RELEASE_GPU === '1' && n % 2 === 0;
  // Every second reload (TILO_RELOADS_IDLE=0 for none) waits until the desk
  // has drawn nothing for ten seconds, so no frame of the old page is still
  // being drawn as it goes: side by side with the reloads that go at once,
  // they say whether a frame still drawing is what holds the new page back.
  const idle = process.env.TILO_RELOADS_IDLE !== '0' && n % 2 === 0;
  if (idle) {
    let frames = Number(await tab.evaluate(FRAMES).catch(() => 0));
    for (let still = Date.now(), until = Date.now() + stepLimit(3); Date.now() < until && Date.now() - still < 10_000;) {
      await sleep(250);
      const now = Number(await tab.evaluate(FRAMES).catch(() => frames));
      if (now !== frames) { frames = now; still = Date.now(); }
    }
  }
  await tab.evaluate(`sessionStorage.setItem('tilo-reload-release-gpu', '${release ? 1 : 0}')`).catch(() => {});
  const asked = Date.now();
  await tab.reload().catch(() => {});
  const came = await until(tab, ARRIVED, stepLimit(6));
  const result = crashed.has(tab.sessionId) ? 'crashed' : !came ? 'never-arrived' : came === 'stopped' ? 'stopped' : 'ok';
  const arrivedAt = Date.now();
  const drew = result === 'ok' ? await until(tab, `(${FRAMES}) > 0`, stepLimit(3)) : null;
  const before = JSON.parse(await tab.evaluate(BEFORE).catch(() => 'null') ?? 'null') ?? {};
  const locks = JSON.parse(await tab.evaluate(DEVICE_LOCKS).catch(() => '[]') ?? '[]');
  const line = {
    n, result,
    gpuReleased: release ? 'yes' : 'no',
    idleFirst: idle ? 'yes' : 'no',
    deviceLocks: locks.length ? locks.map(name => name.replace(/^tilo-device-custody:[^#]*/, '') || '#0').join(',') : 'none',
    hide: before.hide ? before.hide - asked : null,
    handlers: before.hidden && before.hide ? before.hidden - before.hide : null,
    oldRanUntil: before.alive ? before.alive - asked : null,
    gpuDevices: Number.isFinite(before.devices) ? before.devices : null,
    unload: before.unload == null ? null : Math.round(before.unload),
    fetch: before.fetch == null ? null : Math.round(before.origin + before.fetch - asked),
    connect: before.connect == null ? null : Math.round(before.connect),
    request: before.request == null ? null : Math.round(before.origin + before.request - asked),
    response: before.response == null ? null : Math.round(before.origin + before.response - asked),
    body: before.body == null ? null : Math.round(before.origin + before.body - asked),
    protocol: before.protocol,
    commit: committed.has(tab.sessionId) ? committed.get(tab.sessionId) - asked : null,
    arrived: came ? arrivedAt - asked : null,
    frame: drew ? Date.now() - asked : null,
  };
  lines.push(line);
  counts[{ok: 'ok', crashed: 'crashed', 'never-arrived': 'neverArrived', stopped: 'stopped'}[result]] += 1;
  console.log(`TIMING reload ${Object.entries(line).map(([k, v]) => `${k}=${v ?? '-'}`).join(' ')}`);
  if (result !== 'ok') {
    console.log(`  the page said: ${tab.log.slice(-12).join(' | ').slice(0, 2000)}`);
    console.log(`  the machine: ${machine().text}`);
    if (result === 'crashed') console.log(`  the browser said: ${browser.crashLines().split('\n').slice(-20).join(' | ')}`);
    await tab.close().catch(() => {});
    ({tab} = await open());
  }
}
// Every key this person's reloads made: one device, the first visit's. A
// reload takes the key the page before it held back, waiting for it to let
// go, so twenty reloads are one device of the person.
const keys = JSON.parse(await tab.evaluate(DEVICE_KEYS).catch(() => '[]') ?? '[]');
console.log(`TIMING device-keys=${keys.length} (${keys.join(', ')})`);
if (keys.length > 1) {
  console.error(`FAIL: ${keys.length} device keys for one person in one tab: a reload became another device`);
}

// A second tab of the same browser while the first is in: it never waits for
// the first, it is in and works at once, as a device of its own (a tab is
// one, as the protocol takes one writer per device key). Both edit the
// workspace's desk, each opening a window, and each sees the other's. Then
// the first reloads under that contention and is the same device again.
const WINDOWS = `JSON.stringify((globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).map(entry => entry.label))`;
const opens = app => `globalThis.tiloMount.navigation.dispatch({effect: 'open-app', args: {app: ${JSON.stringify(app)}}}).then(() => true)`;
const shows = label => `(globalThis.tiloMount?.navigation?.getSnapshot?.().entries ?? []).some(entry => entry.label === ${JSON.stringify(label)})`;
const tabs = {waited: false, secondIn: false, secondSeesFirst: false, firstSeesSecond: false, reloadedIn: false, sameDevice: false};
const second = await browser.tab();
await second.send('Emulation.setDeviceMetricsOverride', deskMetrics).catch(() => {});
await second.send('Page.addScriptToEvaluateOnNewDocument', {source: EARLY});
await second.navigate(site);
const secondArrived = await until(second, `document.documentElement.dataset.custody === 'waiting' ? 'waiting' : (${ARRIVED})`, stepLimit(6));
tabs.waited = secondArrived === 'waiting';
tabs.secondIn = secondArrived === 'done';
console.log(`TIMING two-tabs second-arrived=${secondArrived ?? 'never'}`);
// The tab a person acts in is the one in front: a browser holds back the
// frames of a tab behind another, as it does for a person.
const front = one => one.send('Page.bringToFront').catch(() => {});
if (tabs.secondIn) {
  await until(second, `(${FRAMES}) > 0`, stepLimit(3));
  await front(tab);
  await until(tab, `(${FRAMES}) > 0`, stepLimit(3));
  await tab.evaluate(opens('so.tilo.notes')).catch(() => {});
  await until(tab, shows('Notes'), stepLimit(3));
  await front(second);
  tabs.secondSeesFirst = Boolean(await until(second, shows('Notes'), stepLimit(4)));
  await second.evaluate(opens('so.tilo.chat')).catch(() => {});
  await until(second, shows('Chat'), stepLimit(3));
  await front(tab);
  tabs.firstSeesSecond = Boolean(await until(tab, shows('Chat'), stepLimit(4)));
  console.log(`TIMING two-tabs first=${await tab.evaluate(WINDOWS).catch(() => '?')} second=${await second.evaluate(WINDOWS).catch(() => '?')}`);
  const before = JSON.parse(await tab.evaluate(DEVICE_LOCKS).catch(() => '[]') ?? '[]');
  await tab.reload().catch(() => {});
  tabs.reloadedIn = (await until(tab, ARRIVED, stepLimit(6))) === 'done';
  const after = JSON.parse(await tab.evaluate(DEVICE_LOCKS).catch(() => '[]') ?? '[]');
  tabs.sameDevice = before.length === 2 && after.length === 2 && JSON.stringify(before.sort()) === JSON.stringify(after.sort());
  console.log(`TIMING two-tabs reloaded=${tabs.reloadedIn} locks-before=${before.join(',')} locks-after=${after.join(',')}`);
}
const tabKeys = JSON.parse(await tab.evaluate(DEVICE_KEYS).catch(() => '[]') ?? '[]');
console.log(`TIMING two-tabs ${Object.entries(tabs).map(([k, v]) => `${k}=${v}`).join(' ')} device-keys=${tabKeys.length}`);
const twoTabsBad = tabs.waited || !tabs.secondIn || !tabs.secondSeesFirst || !tabs.firstSeesSecond || !tabs.reloadedIn
  || !tabs.sameDevice || tabKeys.length !== 2;
if (tabs.waited) console.error('FAIL: a second tab of the browser waited for the first');
if (!tabs.secondIn) console.error('FAIL: a second tab of the browser never came in');
if (tabs.secondIn && !tabs.secondSeesFirst) console.error('FAIL: the window the first tab opened is not on the second tab\'s desk');
if (tabs.secondIn && !tabs.firstSeesSecond) console.error('FAIL: the window the second tab opened is not on the first tab\'s desk');
if (tabs.secondIn && !(tabs.reloadedIn && tabs.sameDevice)) console.error('FAIL: the first tab, reloaded beside the second, did not come back as the same device');
if (tabKeys.length !== 2) console.error(`FAIL: two tabs, reloaded ${times + 1} times between them, hold ${tabKeys.length} device keys, not 2`);
await second.close().catch(() => {});
// The commit, with and without the old page's GPU device destroyed as it went.
const median = values => { const v = values.filter(x => x != null).sort((a, b) => a - b); return v.length ? v[Math.floor(v.length / 2)] : null; };
const commitOf = released => lines.filter(l => l.result === 'ok' && (l.gpuReleased === 'yes') === released).map(l => l.commit);
console.log(`TIMING commit-median gpu-released=${median(commitOf(true))} gpu-kept=${median(commitOf(false))} (ms after the reload)`);
const commitIdle = idle => lines.filter(l => l.result === 'ok' && (l.idleFirst === 'yes') === idle).map(l => l.commit);
console.log(`TIMING commit-median idle-first=${median(commitIdle(true))} at-once=${median(commitIdle(false))} (ms after the reload)`);
const slowest = [...lines].sort((a, b) => (b.fetch ?? 0) - (a.fetch ?? 0)).slice(0, 3);
console.log(`TIMING reloads=${times} ok=${counts.ok} crashed=${counts.crashed} never-arrived=${counts.neverArrived} stopped=${counts.stopped}`);
console.log(`the old page held the navigation longest (fetch ms): ${slowest.map(l => `#${l.n} ${l.fetch}`).join(', ')}`);
writeFileSync(join(out, 'reloads.json'), JSON.stringify({counts, lines, machine: machine().text}, null, 2));
await browser.close().catch(() => {});
const lost = counts.crashed + counts.neverArrived + counts.stopped;
if (lost) console.error(`FAIL: ${lost} of ${times} reloads did not come back (${counts.crashed} crashed, ${counts.neverArrived} never arrived, ${counts.stopped} stopped)`);
const bad = lost + (keys.length > 1 ? 1 : 0) + (twoTabsBad ? 1 : 0);
process.exit(bad ? 1 : 0);

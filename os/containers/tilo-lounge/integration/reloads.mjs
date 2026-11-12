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
// on in a fresh tab. It fails when any reload crashed or never arrived, and
// prints the counts and the slowest reloads either way.
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
// The device key this browser holds for the page's profile: the lock held on
// it now, and the keys kept. A browser profile is one device of the person:
// a reload, or a second tab, that finds the key still held waits for it, and
// a second key would be a second device with its own certificate and state.
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
// A second tab of the same browser while the first is in, then the first
// reloaded under that contention: the second waits for the device key and
// says so, the reloaded page and it take turns with it, and the browser stays
// one device of the person.
const second = await browser.tab();
await second.send('Emulation.setDeviceMetricsOverride', deskMetrics).catch(() => {});
await second.send('Page.addScriptToEvaluateOnNewDocument', {source: EARLY});
await second.navigate(site);
const WAITING = `document.documentElement.dataset.custody === 'waiting' && (document.getElementById('status')?.textContent || 'waiting')`;
const secondWaits = await until(second, WAITING, stepLimit(4));
console.log(`TIMING two-tabs second-tab-waits=${secondWaits ? 'yes' : 'no'}${secondWaits ? ` said="${secondWaits}"` : ''}`);
if (!secondWaits) console.error('FAIL: a second tab of the browser did not wait for the device key the first holds');
await tab.reload().catch(() => {});
const oneIn = await until(tab, `(${ARRIVED}) ? 'first' : document.documentElement.dataset.custody === 'waiting' ? 'waiting' : ''`, stepLimit(6));
const secondIn = await until(second, `(${ARRIVED}) ? 'in' : document.documentElement.dataset.custody === 'waiting' ? 'waiting' : ''`, stepLimit(6));
console.log(`TIMING two-tabs after-reload first=${oneIn ?? 'neither'} second=${secondIn ?? 'neither'}`);
const contended = !(oneIn && secondIn && [oneIn, secondIn].includes('waiting') && !(oneIn === 'waiting' && secondIn === 'waiting'));
if (contended) console.error('FAIL: after a reload under contention one tab should be in and the other waiting for the device key');
await second.close().catch(() => {});

// Every key this person's tabs and reloads made: one device, the first
// visit's.
const keys = JSON.parse(await tab.evaluate(DEVICE_KEYS).catch(() => '[]') ?? '[]');
console.log(`TIMING device-keys=${keys.length} (${keys.join(', ')})`);
if (keys.length > 1) {
  console.error(`FAIL: ${keys.length} device keys for one person in one tab: a reload found the key the page before it held still locked and became another device`);
}
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
const bad = counts.crashed + counts.neverArrived + counts.stopped + (keys.length > 1 ? 1 : 0) + (secondWaits ? 0 : 1) + (contended ? 1 : 0);
if (bad) console.error(`FAIL: ${bad} of ${times} reloads did not come back (${counts.crashed} crashed, ${counts.neverArrived} never arrived, ${counts.stopped} stopped)`);
process.exit(bad ? 1 : 0);

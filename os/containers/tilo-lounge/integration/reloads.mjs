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
//   RELOAD n=3 result=ok hide=12 handlers=4 unload=0 fetch=310 commit=420 arrived=5200 frame=6100
//
// all in ms: `hide` from the reload asked to the old page's first pagehide
// listener; `handlers` the time the old page's pagehide listeners took, from
// the first to one added after the page arrived (so after the runtime's own);
// `unload` the old document's unload event (the new document's navigation
// timing); `fetch` the new document's fetchStart, which waits for the old one
// to let go; `commit` the new document committed (Page.frameNavigated);
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
const EARLY = `(() => {
  try { localStorage.setItem('tilo-setup', JSON.stringify({step: 'done'})); } catch {}
  addEventListener('pagehide', () => {
    try { sessionStorage.setItem('tilo-reload-hide', String(Date.now())); } catch {}
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
    unload: nav ? nav.unloadEventEnd - nav.unloadEventStart : null,
    fetch: nav ? nav.fetchStart : null,
  };
})())`;
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
  const asked = Date.now();
  await tab.reload().catch(() => {});
  const came = await until(tab, ARRIVED, stepLimit(6));
  const result = crashed.has(tab.sessionId) ? 'crashed' : !came ? 'never-arrived' : came === 'stopped' ? 'stopped' : 'ok';
  const arrivedAt = Date.now();
  const drew = result === 'ok' ? await until(tab, `(${FRAMES}) > 0`, stepLimit(3)) : null;
  const before = JSON.parse(await tab.evaluate(BEFORE).catch(() => 'null') ?? 'null') ?? {};
  const line = {
    n, result,
    hide: before.hide ? before.hide - asked : null,
    handlers: before.hidden && before.hide ? before.hidden - before.hide : null,
    unload: before.unload == null ? null : Math.round(before.unload),
    fetch: before.fetch == null ? null : Math.round(before.origin + before.fetch - asked),
    commit: committed.has(tab.sessionId) ? committed.get(tab.sessionId) - asked : null,
    arrived: came ? arrivedAt - asked : null,
    frame: drew ? Date.now() - asked : null,
  };
  lines.push(line);
  counts[{ok: 'ok', crashed: 'crashed', 'never-arrived': 'neverArrived', stopped: 'stopped'}[result]] += 1;
  console.log(`RELOAD ${Object.entries(line).map(([k, v]) => `${k}=${v ?? '-'}`).join(' ')}`);
  if (result !== 'ok') {
    console.log(`  the page said: ${tab.log.slice(-12).join(' | ').slice(0, 2000)}`);
    console.log(`  the machine: ${machine().text}`);
    if (result === 'crashed') console.log(`  the browser said: ${browser.crashLines().split('\n').slice(-20).join(' | ')}`);
    await tab.close().catch(() => {});
    ({tab} = await open());
  }
}
const slowest = [...lines].sort((a, b) => (b.fetch ?? 0) - (a.fetch ?? 0)).slice(0, 3);
console.log(`TIMING reloads=${times} ok=${counts.ok} crashed=${counts.crashed} never-arrived=${counts.neverArrived} stopped=${counts.stopped}`);
console.log(`the old page held the navigation longest (fetch ms): ${slowest.map(l => `#${l.n} ${l.fetch}`).join(', ')}`);
writeFileSync(join(out, 'reloads.json'), JSON.stringify({counts, lines, machine: machine().text}, null, 2));
await browser.close().catch(() => {});
const bad = counts.crashed + counts.neverArrived + counts.stopped;
if (bad) console.error(`FAIL: ${bad} of ${times} reloads did not come back (${counts.crashed} crashed, ${counts.neverArrived} never arrived, ${counts.stopped} stopped)`);
process.exit(bad ? 1 : 0);

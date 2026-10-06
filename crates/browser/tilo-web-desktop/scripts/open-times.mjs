import {summarizeOpenTimes} from './open-times-summary.mjs';

// Open every application of a built desktop bundle in headless Chromium and
// time each open: from dispatching the desktop's `launch` command (what an
// app button press does) to the first presented frame after it, which holds
// the new window. Also reports load -> desktop presented.
//
// usage: node open-times.mjs BASE_URL [--pages N] [--rounds N] [--budget MS] [--metrics]
//   BASE_URL   where build.sh's output is served (any static server that
//              sends application/wasm), or a page that sets
//              globalThis.tiloMount like index.html does
//   Chromium   started with --headless=new --enable-unsafe-webgpu
//              --remote-debugging-port=${CDP_PORT:-9333}
//   --pages    pages opened side by side, each in a fresh browser context
//              (empty HTTP cache and storage), each opening every app (default 1)
//   --rounds   opens per app per page; round 1 is the first open (default 1)
//   --metrics  include the runtime's frame-stage timings for each open
//   VIEWPORT=1440x900 DPR=2 explicitly emulates a Retina desktop.
//   --reload   then reload: how soon the restored desktop, holding every
//              window opened, presents again
// Prints one JSON line per open and a summary; exits non-zero when an open
// fails or any requested open exceeds --budget (default 250 ms).
const args = process.argv.slice(2);
const base = args.shift();
const option = (name, fallback) => { const i = args.indexOf(name); return i < 0 ? fallback : args[i + 1]; };
const pages = Number(option('--pages', 1)), rounds = Number(option('--rounds', 1)), budget = Number(option('--budget', 250));
const metrics = args.includes('--metrics'), reload = args.includes('--reload');
const port = process.env.CDP_PORT ?? '9333';
const pageURL = /\.html(\?|$)/.test(base) ? base : `${base.replace(/\/$/, '')}/index.html`;
const apps = JSON.parse(await (await fetch(new URL('apps.json', pageURL.replace(/[^/]*$/, '')))).text()).applications;

const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, {once: true}));
let id = 0;
const pending = new Map(), listeners = new Set();
ws.addEventListener('message', ({data}) => {
  const msg = JSON.parse(data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); } else for (const l of listeners) l(msg);
});
const send = (method, params = {}, sessionId) => new Promise(resolve => {
  const n = ++id; pending.set(n, resolve); ws.send(JSON.stringify({id: n, method, params, sessionId}));
});
async function evaluate(sessionId, expression) {
  const r = await send('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true}, sessionId);
  if (r.result?.exceptionDetails) throw Error(r.result.exceptionDetails.exception?.description ?? r.result.exceptionDetails.text);
  return r.result?.result?.value;
}
const sleep = ms => new Promise(r => setTimeout(r, ms));

// Installed before the page's scripts: the first time the mount reports each phase.
const probe = `(() => {
  const times = globalThis.__tiloPhases = {};
  new MutationObserver(records => { for (const r of records) { const phase = r.target.dataset?.phase; if (phase && !(phase in times)) times[phase] = performance.now(); } })
    .observe(document, {subtree: true, attributes: true, attributeFilter: ['data-phase']});
})();`;

// In the page: dispatch the launch, resolve at the first status reporting a
// frame presented after it.
const open = (app, withMetrics) => `(async () => {
  const mount = globalThis.tiloMount, nav = mount.navigation, host = mount.canvas.parentElement;
  const runtime = await import(new URL('pkg/tilo_browser_runtime.js', document.baseURI).href);
  if (${withMetrics}) { runtime.configure_frame_metrics(true); runtime.take_frame_metrics(); }
  const snapshot = nav.getSnapshot(), before = snapshot.entries.length, frames = Number(host.dataset.frames);
  const presented = new Promise(resolve => {
    const observer = new MutationObserver(() => {
      if (host.dataset.phase === 'presented' && Number(host.dataset.frames) > frames) { observer.disconnect(); resolve(performance.now()); }
    });
    observer.observe(host, {attributes: true, attributeFilter: ['data-frames']});
  });
  const start = performance.now();
  await nav.dispatch({kind: 'launch', generation: snapshot.generation, app: ${JSON.stringify(app)}});
  const end = await Promise.race([presented, new Promise(r => setTimeout(() => r(NaN), 10000))]);
  const after = nav.getSnapshot();
  const opened = after.entries.length === before + 1 && after.entries.some(e => e.route.tab === after.active?.tab && e.label);
  let stages;
  if (${withMetrics}) { stages = JSON.parse(runtime.take_frame_metrics()); runtime.configure_frame_metrics(false); }
  return {ms: end - start, opened, windowsBefore: before, label: after.entries.find(e => e.route.tab === after.active?.tab)?.label, stages};
})()`;

async function page(index) {
  const {result: {browserContextId}} = await send('Target.createBrowserContext', {disposeOnDetach: true});
  const {result: {targetId}} = await send('Target.createTarget', {url: 'about:blank', browserContextId});
  const {result: {sessionId}} = await send('Target.attachToTarget', {targetId, flatten: true});
  const errors = [];
  const onMsg = msg => {
    if (msg.sessionId !== sessionId) return;
    if (msg.method === 'Runtime.exceptionThrown') errors.push((msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text).slice(0, 300));
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') errors.push(msg.params.args.map(a => a.value ?? a.description ?? '').join(' ').slice(0, 300));
  };
  listeners.add(onMsg);
  await send('Runtime.enable', {}, sessionId);
  await send('Page.enable', {}, sessionId);
  // Optional, explicit viewport for repeatable desktop/Retina acceptance.
  if (process.env.VIEWPORT) {
    const [width, height] = process.env.VIEWPORT.split('x').map(Number);
    await send('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor: Number(process.env.DPR ?? 1), mobile: false}, sessionId);
  }
  await send('Emulation.setFocusEmulationEnabled', {enabled: true}, sessionId);
  await send('Page.addScriptToEvaluateOnNewDocument', {source: probe}, sessionId);
  await send('Page.navigate', {url: pageURL}, sessionId);
  for (let i = 0; i < 600 && !(await evaluate(sessionId, 'Boolean(globalThis.tiloMount?.navigation)')); i++) {
    if (errors.some(e => /panic/i.test(e)) || /^Failed/.test(await evaluate(sessionId, "document.getElementById('status')?.textContent ?? ''"))) break;
    await sleep(100);
  }
  const ready = async () => {
    for (let i = 0; i < 600 && !(await evaluate(sessionId, 'Boolean(globalThis.tiloMount?.navigation)')); i++) await sleep(100);
    return Math.round((await evaluate(sessionId, 'globalThis.__tiloPhases'))?.presented ?? NaN);
  };
  const load = {page: index, desktopPresentedMs: await ready(), viewport: await evaluate(sessionId, '({width:innerWidth,height:innerHeight,dpr:devicePixelRatio})')};
  console.log(JSON.stringify(load));
  const opens = [];
  for (let round = 1; round <= rounds; round++) {
    for (const app of apps) {
      await sleep(600);
      let result;
      try { result = await evaluate(sessionId, open(app.id, metrics)); } catch (error) { result = {ms: NaN, opened: false, error: String(error.message ?? error)}; }
      const row = {page: index, round, app: app.id, ms: Math.round(result.ms * 10) / 10, opened: result.opened, windowsBefore: result.windowsBefore, ...(result.error ? {error: result.error} : {}), ...(result.stages ? {stages: result.stages} : {})};
      opens.push(row);
      console.log(JSON.stringify(row));
    }
  }
  if (reload) {
    const windows = await evaluate(sessionId, 'tiloMount.navigation.getSnapshot().entries.length');
    await sleep(1500);
    const origin = await evaluate(sessionId, 'performance.timeOrigin');
    await send('Page.reload', {}, sessionId);
    while (await evaluate(sessionId, 'performance.timeOrigin').catch(() => origin) === origin) await sleep(20);
    load.restored = {windows, presentedMs: await ready()};
    console.log(JSON.stringify({page: index, restored: load.restored}));
  }
  if (process.env.SHOTS_DIR) {
    const shot = await send('Page.captureScreenshot', {format: 'png'}, sessionId);
    (await import('node:fs')).writeFileSync(`${process.env.SHOTS_DIR}/page-${index}.png`, Buffer.from(shot.result.data, 'base64'));
  }
  listeners.delete(onMsg);
  await send('Target.closeTarget', {targetId});
  return {load, opens, errors};
}

const results = await Promise.all(Array.from({length: pages}, (_, i) => page(i + 1)));
ws.close();
const summary = summarizeOpenTimes(results, budget);
console.log(JSON.stringify({summary}));
process.exit(summary.passed ? 0 : 1);

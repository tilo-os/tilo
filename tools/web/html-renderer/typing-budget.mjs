// Typing latency on the Notes app, as HTML and on the canvas, against a
// plain-DOM page in the same browser: from each key's event to the end of the
// rendering update that shows its character. The budget (`--budget`): the
// app's p50 and p90 stay within one frame (16.7 ms) of the plain page's, on
// both paths.
//
// usage: node tools/web/html-renderer/typing-budget.mjs BUNDLE_DIR [--render dom,gpu] [--keys N]
//          [--key-gap MS] [--attempts N] [--budget] [--profile OUT] [--trace OUT] [--metrics]
//
// BUNDLE_DIR is a built desktop or Notes bundle; the measurement pages
// (app.html, baseline.html) are served beside it. Each key is held 35 ms and
// the next follows --key-gap ms after the press (80 by default, a fast
// typist's pace).
//
// A key's character is on screen once the rendering update after it has run.
// The page's own clock marks the change (the plain page's `input`, the HTML
// presenter's frame, the canvas's work submitted to the GPU) and then the end
// of the rendering update that carries it: a change made inside an animation
// frame is painted by that frame, one made in a task by the next. `--trace`
// adds Chrome's own EventLatency for each key press, which agrees with this
// clock within a millisecond or so; it ends at the next compositor frame even
// when the canvas drew nothing new, so it reads short for a canvas that held
// a frame.
//
// Without a GPU the canvas draws with SwiftShader (or lavapipe), whose raster
// takes hundreds of milliseconds per frame and holds every frame after it:
// what a person would see cannot be measured there, and a canvas run without
// one says nothing about the budget. Run it with the machine's GPU named in
// TILO_CHROME_ARGS (on Linux: `--enable-gpu --use-angle=vulkan
// --enable-features=Vulkan --ignore-gpu-blocklist`). The canvas's one frame
// per key is held without a GPU by the Notes test that types into an app
// mounted alone (apps/notes/tests/typing_frame_work.rs).
//
// A run on a busy machine can miss by noise: --attempts N measures up to N
// times and passes on the first run within the budget. A real regression
// misses on every one.
import {writeFileSync} from 'node:fs';
import {dirname, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {launch, serve, sleep} from '../chrome.mjs';

const arg = (name, fallback) => {
  const at = process.argv.indexOf(`--${name}`);
  return at > 0 ? process.argv[at + 1] : fallback;
};
const bundle = resolve(process.argv[2]);
const renders = arg('render', 'dom,gpu').split(',').filter(Boolean);
const keys = Number(arg('keys', 40));
const rounds = Number(arg('rounds', 1));
const keyGap = Number(arg('key-gap', 80));
const attempts = Number(arg('attempts', 1));
const profileOut = arg('profile');
const traceOut = arg('trace');
const budget = process.argv.includes('--budget');
const metrics = process.argv.includes('--metrics');
const RUNTIME = `(await import(new URL('pkg/tilo_browser_runtime.js', document.baseURI).href))`;
const FRAME_MS = 1000 / 60;

const pct = (values, p) => {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] * 10) / 10;
};

// In the page, before its scripts: every animation-frame callback runs with
// `inFrame` set, so a change knows whether the rendering update it is part
// of is the current one.
const CLOCK = `(() => {
  const clock = window.__typingClock = {keys: [], changes: [], inFrame: false};
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = callback => raf(time => {
    const outer = clock.inFrame; clock.inFrame = true;
    try { return callback(time); } finally { clock.inFrame = outer; }
  });
  const channel = new MessageChannel();
  const after = [];
  channel.port1.onmessage = () => { const now = performance.now(); for (const done of after.splice(0)) done(now); };
  const afterTask = done => { after.push(done); if (after.length === 1) channel.port2.postMessage(0); };
  // The change in hand ends when the rendering update that carries it has run.
  clock.changed = () => {
    const at = performance.now();
    const entry = [at, null];
    clock.changes.push(entry);
    if (clock.inFrame) afterTask(now => { entry[1] = now; });
    else raf(() => afterTask(now => { entry[1] = now; }));
  };
  addEventListener('keydown', event => { if (event.isTrusted) clock.keys.push(event.timeStamp); }, {capture: true});
  if (location.pathname.endsWith('baseline.html')) document.addEventListener('input', clock.changed, true);
})();`;

const MARKDOWN = Array.from({length: 60}, (_, i) => i % 10 === 0
  ? `## Section ${i / 10 + 1}`
  : `${i % 3 === 0 ? '- ' : ''}Paragraph ${i}: the quick brown fox jumps over the lazy dog while notes keep every thought close at hand, and the page keeps scrolling.`).join('\n\n');
const SEED = `(async () => {
  const actions = window.mount.actions();
  const importing = actions.find(a => /import/i.test(a.address.action) && /markdown/i.test(a.address.action));
  if (!importing) return 'no import action: ' + actions.map(a => a.address.action).join(',');
  const answer = await window.mount.invokeAction(importing.address.resource, importing.address.action,
    {title: 'Field notes', markdown: ${JSON.stringify(MARKDOWN)}, images: [], note: null});
  return JSON.stringify(answer);
})()`;
const FIND = text => `(() => {
  const root = window.mount.state()?.semantics?.root;
  const walk = n => { if (!n) return null;
    const s = n.semantics; const words = [s?.label, s?.value, s?.name].filter(Boolean).join(' ');
    if (words.includes(${JSON.stringify(text)}) && n.rect) return [n.rect[0] + Math.min(n.rect[2] / 2, 80), n.rect[1] + n.rect[3] / 2];
    for (const c of n.children ?? []) { const r = walk(c); if (r) return r; } return null; };
  return walk(root);
})()`;
// Each key against the first change after it: [to change, to painted].
const LATENCIES = `(() => {
  const clock = window.__typingClock;
  const out = clock.keys.map(at => {
    const change = clock.changes.find(([t, painted]) => t >= at && painted !== null);
    return change ? [change[0] - at, change[1] - at] : null;
  }).filter(Boolean);
  clock.keys = []; clock.changes = [];
  return out;
})()`;

async function click(tab, x, y) {
  for (const type of ['mousePressed', 'mouseReleased']) {
    await tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', clickCount: 1});
  }
}

async function run({page, render}) {
  const gpuArgs = render === 'gpu' && !process.env.TILO_CHROME_ARGS
    ? ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader'] : [];
  const browser = await launch({args: ['--window-size=1280,800', ...gpuArgs, ...(process.getuid?.() === 0 ? ['--no-sandbox'] : [])]});
  // The measurement pages are served beside the bundle's own files.
  const server = await serve(bundle, {also: [dirname(fileURLToPath(import.meta.url))]});
  const result = {page, render: render ?? 'plain'};
  try {
    const tab = await browser.tab();
    await tab.send('Emulation.setDeviceMetricsOverride', {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
    await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: CLOCK});
    await tab.navigate(`${server.url}/${page}${render ? `?render=${render}` : ''}`);
    await tab.waitFor('!!window.renderMarks', {timeout: 60000});
    let target;
    if (page === 'app.html') {
      await tab.waitFor('window.renderMarks.presented[0]', {timeout: 120000});
      let last = -1;
      for (let i = 0; i < 40; i++) {
        await sleep(250);
        const n = await tab.evaluate('window.renderMarks.presented.length');
        if (n === last) break;
        last = n;
      }
      result.seed = await tab.evaluate(SEED).catch(error => String(error));
      // The canvas publishes its semantics tree throttled: ask until it names the note.
      const item = await tab.waitFor(`(window.mount.requestSemantics?.(), ${FIND('Field notes')})`, {timeout: 20000})
        .catch(() => { throw new Error('the seeded note is not in the sidebar'); });
      await click(tab, item[0], item[1]);
      await sleep(1500);
      // A change is a frame handed over: the HTML presenter's, or the
      // canvas's work submitted to the GPU (its presented status is said
      // later, once the GPU is done, which is past the frame that shows it).
      await tab.evaluate(render === 'gpu'
        ? `(() => { const submit = GPUQueue.prototype.submit; GPUQueue.prototype.submit = function (...v) { window.__typingClock.changed(); return submit.apply(this, v); }; return true; })()`
        : `(() => { const m = window.renderMarks; const push = m.presented.push.bind(m.presented);
        m.presented.push = (...v) => { window.__typingClock.changed(); return push(...v); }; return true; })()`);
      target = {x: 700, y: 256};
    } else {
      target = {x: 600, y: 120};
    }
    await click(tab, target.x, target.y);
    await sleep(400);
    if (page === 'baseline.html') await tab.evaluate('(()=>{const s=getSelection();s.selectAllChildren(document.querySelector("#doc p"));s.collapseToEnd();return true})()');
    // A warm-up: the first keys compile and allocate what later ones reuse.
    const text = 'Typing into the page measures each frame. ';
    const type = async n => {
      for (let i = 0; i < n; i++) {
        const ch = text[i % text.length];
        // Held as a person holds a key: a release sent at once lands in
        // the frame the press is measured by.
        await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
        await sleep(35);
        await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
        await sleep(Math.max(0, keyGap - 35));
      }
      await sleep(300);
    };
    await type(10);
    await tab.evaluate(LATENCIES);
    if (profileOut) {
      await tab.send('Profiler.enable', {});
      await tab.send('Profiler.setSamplingInterval', {interval: 100});
      await tab.send('Profiler.start', {});
    }
    const events = [];
    let unlisten;
    if (traceOut) {
      unlisten = browser.on(message => {
        if (message.method === 'Tracing.dataCollected') events.push(...message.params.value);
      });
      await tab.send('Tracing.start', {transferMode: 'ReportEvents', traceConfig: {includedCategories: ['devtools.timeline', 'disabled-by-default-devtools.timeline', 'disabled-by-default-devtools.timeline.frame', 'input', 'latencyInfo', 'benchmark', 'blink.user_timing', 'gpu', 'viz', 'cc']}});
    }
    if (metrics && page === 'app.html') await tab.evaluate(`(async () => { const r = ${RUNTIME}; r.configure_frame_metrics(true); r.take_frame_metrics(); return true; })()`);
    await type(keys * rounds);
    if (metrics && page === 'app.html') {
      const taken = JSON.parse(await tab.evaluate(`(async () => { const r = ${RUNTIME}; const m = r.take_frame_metrics(); r.configure_frame_metrics(false); return m; })()`));
      result.metrics = taken.map(m => {
        const stages = {};
        for (const {stage, ms} of m.samples) (stages[stage] ??= []).push(ms);
        return {requesters: m.requesters, measuredBy: m.measuredBy, lastRelayout: m.lastRelayout,
          stages: Object.fromEntries(Object.entries(stages).map(([k, v]) => [k, {n: v.length, p50: pct(v, 0.5), p90: pct(v, 0.9)}]))};
      });
    }
    const latencies = await tab.evaluate(LATENCIES);
    result.keys = latencies.length;
    result.changeP50 = pct(latencies.map(l => l[0]), 0.5);
    result.p50 = pct(latencies.map(l => l[1]), 0.5);
    result.p90 = pct(latencies.map(l => l[1]), 0.9);
    result.max = pct(latencies.map(l => l[1]), 1);
    if (profileOut) {
      const {result: {profile}} = await tab.send('Profiler.stop', {});
      writeFileSync(`${profileOut}.${result.render}.cpuprofile`, JSON.stringify(profile));
      result.profile = summarize(profile, latencies.length);
    }
    if (traceOut) {
      const done = new Promise(resolve => { const off = browser.on(m => { if (m.method === 'Tracing.tracingComplete') { off(); resolve(); } }); });
      await tab.send('Tracing.end', {});
      await done;
      unlisten();
      writeFileSync(`${traceOut}.${result.render}.json`, JSON.stringify({traceEvents: events}));
      // Each key press's slice: its begin and the next end under the same id
      // (ids are reused).
      const spans = [], open = new Map();
      for (const e of events.filter(e => e.name === 'EventLatency').sort((a, b) => a.ts - b.ts)) {
        const id = `${e.pid}:${e.id2?.local ?? e.id}`;
        if (e.ph === 'b') open.set(id, e.args?.event_latency?.event_type === 'KEY_PRESSED' ? e.ts : null);
        else if (e.ph === 'e' && open.has(id)) { const begun = open.get(id); open.delete(id); if (begun !== null) spans.push((e.ts - begun) / 1000); }
      }
      result.eventLatency = {n: spans.length, p50: pct(spans, 0.5), p90: pct(spans, 0.9)};
    }
  } catch (error) {
    result.error = String(error?.stack ?? error);
  } finally {
    await server.close();
    await browser.close().catch(() => {});
  }
  return result;
}

// Per key: the self and total time of the functions a key spent most in.
function summarize(profile, perKeys) {
  const byId = new Map(profile.nodes.map(n => [n.id, n]));
  const parent = new Map();
  for (const n of profile.nodes) for (const c of n.children ?? []) parent.set(c, n.id);
  const self = new Map(), total = new Map();
  const deltas = profile.timeDeltas;
  const nameOf = n => n.callFrame.functionName || `(${n.callFrame.url.split('/').pop()}:${n.callFrame.lineNumber})`;
  profile.samples.forEach((id, i) => {
    const dt = (deltas[i + 1] ?? deltas[i]) / 1000;
    const node = byId.get(id);
    const name = nameOf(node);
    if (/^\((idle|program|garbage collector)\)$/.test(name) && name !== '(garbage collector)') return;
    self.set(name, (self.get(name) ?? 0) + dt);
    const seen = new Set();
    for (let at = id; at !== undefined; at = parent.get(at)) {
      const n = nameOf(byId.get(at));
      if (seen.has(n)) continue;
      seen.add(n);
      total.set(n, (total.get(n) ?? 0) + dt);
    }
  });
  const top = (map, n) => [...map].sort((a, b) => b[1] - a[1]).slice(0, n).map(([k, v]) => [k, Math.round(v / perKeys * 100) / 100]);
  return {selfMsPerKey: top(self, 40), totalMsPerKey: top(total, 80)};
}

// One measurement: the plain page, then each path, in the same browser build.
async function measure() {
  const results = [await run({page: 'baseline.html'})];
  for (const render of renders) results.push(await run({page: 'app.html', render}));
  const plain = results[0];
  const verdicts = results.slice(1).map(r => {
    return {render: r.render,
      p50: r.p50, p90: r.p90, plainP50: plain.p50, plainP90: plain.p90,
      within: r.error === undefined && plain.error === undefined && r.keys >= keys * rounds * 0.9
        && r.p50 - plain.p50 <= FRAME_MS && r.p90 - plain.p90 <= FRAME_MS,
      error: r.error ?? plain.error};
  });
  return {results, verdicts};
}

const runs = [];
for (let attempt = 0; attempt < attempts; attempt++) {
  runs.push(await measure());
  if (runs.at(-1).verdicts.every(v => v.within)) break;
}
console.log(JSON.stringify(runs.length === 1 ? runs[0] : {runs}, null, 1));
const last = runs.at(-1);
for (const v of last.verdicts) {
  const path = v.render === 'dom' ? 'HTML' : 'canvas';
  console.error(`typing on the ${path} path: p50 ${v.p50} ms, p90 ${v.p90} ms; the plain page's ${v.plainP50} and ${v.plainP90} ms; ${v.within ? 'within' : 'past'} one frame (${FRAME_MS.toFixed(1)} ms) of it${v.error ? `\n${v.error}` : ''}`);
}
if (budget && last.verdicts.some(v => !v.within)) process.exit(1);

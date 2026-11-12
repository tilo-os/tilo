// What one key costs: the frames it draws, the work in each (layout, encode,
// presenter) and where the time goes on the browser's main thread, against
// the plain-DOM page measured in the same run.
//
// A run whose plain page is slow is not measured: a busy machine stretches
// both sides, so the baseline is taken first and the run is rejected when its
// median is above REJECT_MS, rather than reporting a number that says more
// about the machine than the page.
//
// usage: node tools/web/html-renderer/typing.mjs BUNDLE_DIR OUT_DIR [--keys N] [--trace]
import {mkdirSync, readFileSync, writeFileSync, existsSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {serve} from '../chrome.mjs';
import {open as openBrowser, sleep} from './engines.mjs';

const bundle = resolve(process.argv[2]);
const out = resolve(process.argv[3]);
mkdirSync(out, {recursive: true});
const keys = Number(process.argv[process.argv.indexOf('--keys') + 1]) || 24;
const tracing = process.argv.includes('--trace');
// --phone: the same keys at a phone's width, where the note is one column and
// text is set at three device pixels per point.
const phone = process.argv.includes('--phone');
const [width, height] = phone ? [390, 844] : [1280, 800];
const sandbox = process.getuid?.() === 0 ? ['--no-sandbox'] : [];
// Which browser's build runs the page: Chromium is the only one here with the
// toolkit's canvas, but a key's cost is the HTML path's, so the other engines
// measure the same page.
const engine = (() => { const at = process.argv.indexOf('--browser'); return at < 0 ? 'chromium' : process.argv[at + 1]; })();
// A plain page slower than this says the machine is busy, not that the page is
// slow: the numbers of that run mean nothing.
const REJECT_MS = 8;
// How long after a key its frames are counted: a key draws in the tasks right
// after it, well inside one frame.
const WINDOW_MS = 120;
const TEXT = 'Typing into the page measures each frame. ';

const pct = (values, p) => {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] * 10) / 10;
};
const mean = values => values.length ? Math.round(values.reduce((a, b) => a + b, 0) / values.length * 100) / 100 : null;

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

// Every frame the mount hands over, with the stats the page keeps for it, and
// every key the page hears: the two together say how many frames a key costs.
const INSTRUMENT = `(() => {
  const marks = window.renderMarks ??= {presented: []};
  const log = window.__typing = {keys: [], frames: [], painted: []};
  addEventListener('keydown', e => log.keys.push([e.timeStamp, e.key]), {capture: true});
  // When the browser first paints what a frame wrote: the animation frame
  // that starts after the frame was handed over.
  let presented = marks.presented.length;
  const presentPush = marks.presented.push.bind(marks.presented);
  marks.presented.push = (...values) => {
    const result = presentPush(...values);
    if (marks.presented.length > presented) {
      presented = marks.presented.length;
      const at = performance.now();
      requestAnimationFrame(() => log.painted.push([at, performance.now()]));
    }
    return result;
  };
  // One entry per frame handed over, with what it wrote and spent: the page
  // records a frame's stats as it presents it.
  const frames = marks.domFrames ??= [];
  const framePush = frames.push.bind(frames);
  frames.push = (...rows) => {
    const result = framePush(...rows);
    for (const row of rows) log.frames.push([performance.now(), row]);
    return result;
  };
  // The plain page changes its own DOM: its paint follows its input directly.
  addEventListener('input', () => {
    const at = performance.now();
    requestAnimationFrame(() => log.painted.push([at, performance.now()]));
  }, {capture: true});
  return true;
})()`;
const TAKE = `(() => {
  // Drained in place: the listeners below hold this object.
  const log = window.__typing;
  const copy = {keys: [...log.keys], frames: [...log.frames], painted: [...log.painted]};
  log.keys.length = 0; log.frames.length = 0; log.painted.length = 0;
  return JSON.stringify(copy);
})()`;
// The runtime's own frame metrics: what asked for each presented frame, by
// widget path, and the toolkit's per-pass timings.
const RUNTIME = `(await import(new URL('pkg/tilo_browser_runtime.js', document.baseURI).href))`;
const METRICS_ON = `(async () => { const r = ${RUNTIME}; r.configure_frame_metrics(true); r.take_frame_metrics(); return true; })()`;
const METRICS_OFF = `(async () => { const r = ${RUNTIME}; const m = r.take_frame_metrics(); r.configure_frame_metrics(false); return m; })()`;
// The renderer's main thread: where style, layout and paint run, and where
// the page's own work runs.
const TRACE_CATEGORIES = ['devtools.timeline', 'v8', 'blink', 'disabled-by-default-devtools.timeline', 'disabled-by-default-v8.cpu_profiler'];
const STAGES = new Set(['UpdateLayoutTree', 'Layout', 'PrePaint', 'Paint', 'Layerize', 'Commit', 'FunctionCall', 'EvaluateScript', 'v8.run', 'RunTask', 'FireAnimationFrame', 'FireIdleCallback', 'TimerFire']);

async function trace(browser, seconds) {
  const events = [];
  const stop = browser.on(message => {
    if (message.method === 'Tracing.dataCollected') events.push(...message.params.value);
  });
  const ended = new Promise(done => {
    const off = browser.on(message => { if (message.method === 'Tracing.tracingComplete') { off(); done(); } });
  });
  await browser.send('Tracing.start', {categories: TRACE_CATEGORIES.join(','), transferMode: 'ReportEvents'});
  await sleep(seconds * 1000);
  await browser.send('Tracing.end');
  await ended;
  stop();
  return events;
}

// Main-thread CPU time per stage, between `from` and `to` (ms, performance.now
// of the page's own clock is not the trace clock; the caller aligns them by
// the key windows' order instead, so this returns stages of the whole trace).
function stages(events) {
  const names = new Map();
  for (const event of events) {
    if (event.ph === 'M' && event.name === 'thread_name') names.set(`${event.pid}:${event.tid}`, event.args?.name);
  }
  const main = new Set([...names].filter(([, name]) => /CrRendererMain/.test(name ?? '')).map(([id]) => id));
  const totals = {};
  const out = [];
  for (const event of events) {
    if (event.ph !== 'X' || !main.has(`${event.pid}:${event.tid}`) || !STAGES.has(event.name)) continue;
    out.push({name: event.name, at: event.ts / 1000, ms: event.dur / 1000});
    totals[event.name] = Math.round((totals[event.name] ?? 0) + event.dur / 1000);
  }
  return {totals, events: out};
}

async function measured({page, render, width, height, seed}) {
  const opened = await openBrowser({engine, width, height, scale: phone ? 3 : 1, mobile: phone, args: sandbox});
  const browser = opened.protocol;
  const tab = opened.tab;
  const server = await serve(bundle);
  const result = {page, render, width, engine};
  try {
    await tab.colourScheme('light');
    await tab.navigate(`${server.url}/${page}${render ? `?render=${render}` : ''}`);
    await tab.waitFor('!!window.renderMarks', {timeout: 60000});
    if (page === 'app.html') {
      await tab.waitFor('window.renderMarks.presented[0]', {timeout: 90000});
      for (let i = 0, last = -1; i < 40; i++) {
        await sleep(250);
        const n = await tab.evaluate('window.renderMarks.presented.length');
        if (n === last) break;
        last = n;
      }
      result.seed = await tab.evaluate(SEED).catch(error => String(error));
      await sleep(1500);
      const item = await tab.evaluate(FIND('Field notes'));
      if (item) {
        await tab.mouse.down({x: item[0], y: item[1]});
        await tab.mouse.up({x: item[0], y: item[1]});
        await sleep(1500);
      }
    }
    await tab.evaluate(INSTRUMENT);
    const target = page === 'baseline.html' ? {x: phone ? width / 2 : 600, y: 120} : {x: phone ? 200 : 700, y: phone ? 287 : 256};
    await tab.mouse.down({x: target.x, y: target.y});
    await tab.mouse.up({x: target.x, y: target.y});
    await sleep(400);
    if (page === 'baseline.html') await tab.evaluate('(()=>{const s=getSelection();s.selectAllChildren(document.querySelector("#doc p:nth-child(20)"));s.collapseToEnd();return true})()');
    await sleep(200);
    await tab.evaluate(TAKE);
    // A trace is the DevTools protocol's; the other engines report the frames
    // and their work without it.
    const recording = tracing && opened.cdp ? trace(browser, keys * 0.1 + 1) : null;
    await sleep(tracing ? 200 : 0);
    const at = await tab.evaluate('performance.now()');
    for (const ch of TEXT.slice(0, keys)) {
      await tab.key.down({key: ch, text: ch});
      await tab.key.up({key: ch});
      await sleep(100);
    }
    await sleep(400);
    const log = JSON.parse(await tab.evaluate(TAKE));
    result.since = at;
    result.log = log;
    result.trace = recording ? stages(await recording) : null;
    result.dom = await tab.evaluate('JSON.stringify(window.renderMarks.dom ?? null)').then(JSON.parse);
    result.log.tab = undefined;
    result.errors = tab.log.filter(line => /^(EXC|ERROR)/.test(line)).slice(0, 4);
    // Second pass, metrics on: what asked for the frames, and how the
    // toolkit's passes split the work. Profiling reads clocks per pass, so it
    // runs after the timings above, never during them.
    if (render === 'dom') {
      await tab.evaluate(METRICS_ON);
      for (const ch of TEXT.slice(0, Math.min(keys, 8))) {
        await tab.key.down({key: ch, text: ch});
        await tab.key.up({key: ch});
        await sleep(100);
      }
      await sleep(300);
      result.metrics = JSON.parse(await tab.evaluate(METRICS_OFF));
    }
  } catch (error) {
    result.error = String(error?.stack ?? error);
  } finally {
    await server.close();
    await browser.close().catch(() => {});
  }
  return result;
}

// Each key with the frames it cost: when each was handed over, and what it
// wrote and spent.
function perKey(log) {
  const out = [];
  for (const [at, key] of log.keys) {
    const frames = log.frames.filter(([when]) => when >= at && when < at + WINDOW_MS);
    if (!frames.length) continue;
    // The first paint after the key's first frame: what the person sees.
    const first = frames[0][0];
    const painted = log.painted.find(([, painted]) => painted >= first);
    out.push({
      key,
      paint: painted ? Math.round((painted[1] - at) * 10) / 10 : null,
      frames: frames.map(([when, stats]) => stats
        ? {at: Math.round((when - at) * 10) / 10, writes: stats[0], created: stats[1], present: stats[2], layout: Math.round(stats[3] * 10) / 10, encode: stats[4], draws: stats[5], texts: stats[6]}
        : {at: Math.round((when - at) * 10) / 10}),
    });
  }
  return out;
}

const results = {baseline: null, dom: null};
// The plain page first: it says whether this machine's numbers mean anything.
results.baseline = await measured({page: 'baseline.html', width, height});
const baselineLatency = (results.baseline.log.painted ?? []).map(([at, painted]) => painted - at);
results.baseline.latency = {n: baselineLatency.length, p50: pct(baselineLatency, 0.5), p90: pct(baselineLatency, 0.9)};
results.baseline.framesPerKey = null;
const usable = (results.baseline.latency.p50 ?? Infinity) <= REJECT_MS;
results.dom = await measured({page: 'app.html', render: 'dom', width, height, seed: true});
const per = perKey(results.dom.log);
const latencies = per.map(entry => entry.frames[0].at);
const counts = {};
for (const entry of per) counts[entry.frames.length] = (counts[entry.frames.length] ?? 0) + 1;
results.dom.typing = {n: latencies.length, p50: pct(latencies, 0.5), p90: pct(latencies, 0.9)};
const paints = per.map(entry => entry.paint).filter(Boolean);
results.dom.paint = {n: paints.length, p50: pct(paints, 0.5), p90: pct(paints, 0.9)};
results.dom.framesPerKey = counts;
results.dom.firstFrame = {
  writes: mean(per.map(entry => entry.frames[0].writes ?? 0)),
  layout: mean(per.map(entry => entry.frames[0].layout ?? 0)),
  present: mean(per.map(entry => entry.frames[0].present ?? 0)),
};
results.dom.laterFrames = per.flatMap(entry => entry.frames.slice(1)).map(frame => ({at: frame.at, writes: frame.writes, layout: frame.layout, present: frame.present}));
results.dom.perKey = per.slice(0, 12);
// What asked for each frame, and the passes each frame spent its time in.
const mount = (results.dom.metrics ?? [])[0] ?? {};
const requesters = Object.entries(mount.requesters ?? {}).sort((a, b) => b[1] - a[1]).slice(0, 20);
const passes = (mount.layoutPasses ?? []).slice(-8).map(sample => Object.fromEntries(
  Object.entries(sample).filter(([key]) => /(sync|layout|draw|event|total|us|counts)/i.test(key) && typeof sample[key] !== 'object')));
results.dom.requesters = requesters;
results.dom.layoutPasses = passes;
results.dom.measuredBy = Object.entries(mount.measuredBy ?? {}).sort((a, b) => b[1] - a[1]).slice(0, 8);
results.dom.lastRelayout = (mount.lastRelayout ?? '').slice(0, 600);
results.usable = usable;
results.verdict = !usable
  ? `rejected: the plain page's median was ${results.baseline.latency.p50} ms, above ${REJECT_MS}`
  : `the plain page's median was ${results.baseline.latency.p50} ms; a key reached its first frame at ${results.dom.typing.p50} ms and was painted at ${results.dom.paint.p50} ms (p50)`;
delete results.dom.log;
delete results.baseline.log;
console.log(JSON.stringify(results.verdict));
console.log(JSON.stringify({
  usable: results.usable,
  baseline: results.baseline.latency,
  typing: results.dom.typing,
  paint: results.dom.paint,
  framesPerKey: results.dom.framesPerKey,
  firstFrame: results.dom.firstFrame,
  laterFrames: results.dom.laterFrames.slice(0, 12),
  perKey: results.dom.perKey,
  requesters: results.dom.requesters,
  layoutPasses: results.dom.layoutPasses,
  measuredBy: results.dom.measuredBy,
  trace: results.dom.trace ? results.dom.trace.totals : results.baseline.trace?.totals,
}));
writeFileSync(join(out, 'typing.json'), JSON.stringify(results, null, 1));
if (!existsSync(join(out, 'typing.json'))) process.exitCode = 1;

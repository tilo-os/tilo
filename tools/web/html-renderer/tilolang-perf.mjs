// How fast a tilolang package feels as HTML, in headless Chromium: the page started, every
// sidebar place chosen, rows hovered, a field typed into, long lists scrolled, the sidebar
// dragged wider, the window resized, a menu and a dialog opened. For each, what a person
// waits for: from each input to the end of the rendering update after it (`p50`/`max`, ms),
// the long tasks it caused, and the work behind it: frames presented, the runtime's time in
// wasm (update and layout, then the composition encoded) and the presenter's, elements
// written and made, and the browser's style and layout passes.
//
// usage: node tools/web/html-renderer/tilolang-perf.mjs BUNDLE_DIR [OUT_DIR] [--package tl/<id>/]
//                                                      [--budget] [--pin] [--attempts N] [--cpu N]
//                                                      [--profile DIR]
//
// BUNDLE_DIR is build-tilolang-web.sh's output (a release build: a debug one is several times
// slower). Every interaction past a frame (25 ms from input to its paint, with a margin for a headless
// browser's own scheduling) or with a task over 50 ms is said. `--budget` fails when the numbers
// are past those pinned in tilolang-perf.pinned.json, which `--pin` writes from the measurement
// with a margin for noise; they are pinned on the builder (a cargo drop-in's `exec`, CHROME=chromium-browser),
// whose idle cores measure steadier than a shared laptop. `--attempts N` measures up to N times
// and passes on the first within them. `--profile DIR` keeps a CPU profile of the start and of
// each interaction there, as `<interaction>.cpuprofile` (DevTools opens it). `--cpu N` slows the
// page's CPU N times (Chromium's throttling), and the pinned times with it.
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {launch, serve, sleep} from '../chrome.mjs';

const option = name => { const at = process.argv.indexOf(name); return at < 0 ? null : process.argv[at + 1]; };
const bundle = resolve(process.argv[2]);
const out = process.argv[3] && !process.argv[3].startsWith('--') ? resolve(process.argv[3]) : null;
const pkg = option('--package') ?? 'tl/so.tilo.lang.gallery/';
const budget = process.argv.includes('--budget');
const attempts = Number(option('--attempts')) || 1;
const cpu = Number(option('--cpu')) || 1;
const profiles = option('--profile');
const WIDTH = 1280;
const HEIGHT = 800;
// What an interaction may take: a frame at 60 Hz, with half a frame for a headless browser's
// own scheduling, and what a task may take before the page stops answering.
const INPUT_BUDGET_MS = 25;
const LONG_TASK_MS = 50;
const PLACES = ['Basics', 'Controls', 'Feedback', 'Backlog', 'Release board', 'Data', 'Media',
  'Places', 'Shell parts', 'Charts and views'];

// In the page, before its scripts: each input's time to the end of the rendering update after
// it, the long tasks, and the presenter's frames as they come.
const CLOCK = `(() => {
  const perf = window.__perf = {inputs: [], long: []};
  const raf = requestAnimationFrame.bind(window);
  const tasks = new MessageChannel();
  const after = [];
  tasks.port1.onmessage = () => { const now = performance.now(); for (const done of after.splice(0)) done(now); };
  const afterTask = done => { after.push(done); if (after.length === 1) tasks.port2.postMessage(0); };
  for (const type of ['pointerdown', 'pointerup', 'pointermove', 'keydown', 'wheel']) {
    addEventListener(type, event => {
      if (!event.isTrusted) return;
      const entry = {type, at: event.timeStamp, painted: null};
      perf.inputs.push(entry);
      raf(() => afterTask(now => { entry.painted = now; }));
    }, {capture: true, passive: true});
  }
  new PerformanceObserver(list => {
    for (const entry of list.getEntries()) perf.long.push([Math.round(entry.startTime), Math.round(entry.duration)]);
  }).observe({type: 'longtask', buffered: true});
})()`;

// The smallest element of the mount that reads `name`.
const NAMED = name => `(() => {
  let best = null;
  for (const element of document.querySelectorAll('#app [data-path], #app [data-path] *')) {
    const said = (element.getAttribute('aria-label') ?? element.textContent ?? '').trim();
    if (said !== ${JSON.stringify(name)}) continue;
    const box = element.getBoundingClientRect();
    if (box.width < 4 || box.height < 4 || box.bottom < 0 || box.top > innerHeight) continue;
    if (!best || box.width * box.height < best.area) best = {x: box.left + box.width / 2, y: box.top + box.height / 2, area: box.width * box.height};
  }
  return JSON.stringify(best);
})()`;

// The first field a person types words into.
const FIELD = `(() => {
  for (const element of document.querySelectorAll('#app [data-path] input:not([type]), #app [data-path] input[type=text], #app [data-path] textarea')) {
    if (element.readOnly) continue;
    const box = element.getBoundingClientRect();
    if (box.width < 4 || box.height < 4 || box.bottom < 0 || box.top > innerHeight) continue;
    return JSON.stringify({x: box.left + Math.min(box.width / 2, 60), y: box.top + box.height / 2});
  }
  return 'null';
})()`;

// The largest scroller of the mount that can move.
const SCROLLER = `(() => {
  let best = null;
  for (const element of document.querySelectorAll('#app [data-path]')) {
    if (element.scrollHeight <= element.clientHeight + 4 || getComputedStyle(element).overflowY !== 'auto') continue;
    const box = element.getBoundingClientRect();
    if (!best || box.width * box.height > best.area) best = {x: box.left + box.width / 2, y: box.top + box.height / 2, area: box.width * box.height};
  }
  return JSON.stringify(best);
})()`;
// The sidebar's edge: its grip stands just past the column its rows fill.
const EDGE = `(() => {
  const list = document.querySelector('#app [data-widget=NavList]');
  if (!list) return 'null';
  const box = list.getBoundingClientRect();
  return JSON.stringify({x: box.right + 3, y: box.top + box.height / 2});
})()`;

const pct = (values, p) => {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] * 10) / 10;
};
const round = n => Math.round(n * 10) / 10;

async function measure() {
  const browser = await launch({args: [`--window-size=${WIDTH},${HEIGHT}`]});
  const server = await serve(bundle);
  const result = {cpu, interactions: {}};
  try {
    const tab = await browser.tab();
    await tab.send('Emulation.setDeviceMetricsOverride', {width: WIDTH, height: HEIGHT, deviceScaleFactor: 1, mobile: false});
    if (cpu > 1) await tab.send('Emulation.setCPUThrottlingRate', {rate: cpu});
    await tab.send('Emulation.setTimezoneOverride', {timezoneId: 'UTC'});
    await tab.send('Page.addScriptToEvaluateOnNewDocument', {source: CLOCK});
    await tab.send('Performance.enable', {timeDomain: 'timeTicks'});
    const metrics = async () => Object.fromEntries((await tab.send('Performance.getMetrics')).result.metrics.map(m => [m.name, m.value]));
    const frames = () => tab.evaluate('(window.renderMarks?.domFrames ?? []).length');
    // Settled once no frame came for half a second.
    const settle = async () => {
      for (let i = 0, last = -1, still = 0; i < 80; i++) {
        await sleep(100);
        const n = await frames();
        still = n === last ? still + 1 : 0;
        if (still >= 5) return;
        last = n;
      }
    };
    const profiler = {
      async start() {
        if (!profiles) return;
        await tab.send('Profiler.enable');
        await tab.send('Profiler.setSamplingInterval', {interval: 200});
        await tab.send('Profiler.start');
      },
      async stop(name) {
        if (!profiles) return;
        mkdirSync(profiles, {recursive: true});
        const {result: {profile}} = await tab.send('Profiler.stop');
        writeFileSync(join(profiles, `${name.replace(/[^a-z0-9]+/gi, '-').toLowerCase()}.cpuprofile`), JSON.stringify(profile));
      },
    };
    await profiler.start();
    await tab.navigate(`${server.url}/app.html?package=${encodeURIComponent(pkg)}&scenario=all&render=dom&theme=light`);
    await tab.waitFor('window.renderMarks?.presented?.length > 0', {timeout: 120000});
    await settle();
    await profiler.stop('startup');
    result.startup = JSON.parse(await tab.evaluate(`JSON.stringify((() => {
      const m = window.renderMarks;
      const paint = performance.getEntriesByName('first-contentful-paint')[0]?.startTime;
      const resources = performance.getEntriesByType('resource');
      const bytes = resources.reduce((sum, r) => sum + (r.encodedBodySize || 0), 0);
      return {firstFrameMs: Math.round(m.presented[0]), firstContentfulPaintMs: Math.round(paint ?? -1),
        settledFrames: (m.domFrames ?? []).length, elements: window.__tiloDom?.stats?.elements,
        transferredKiB: Math.round(bytes / 1024), longTasks: window.__perf.long.slice(),
        largest: resources.sort((a, b) => b.encodedBodySize - a.encodedBodySize).slice(0, 6)
          .map(r => [r.name.split('/').slice(-2).join('/'), Math.round(r.encodedBodySize / 1024), Math.round(r.responseEnd)])};
    })())`));

    const step = async (name, act) => {
      await settle();
      const before = await metrics();
      const startFrame = await frames();
      await tab.evaluate('window.__perf.inputs = []; window.__perf.long = []; true');
      await profiler.start();
      await act();
      await settle();
      await profiler.stop(name);
      const after = await metrics();
      const page = JSON.parse(await tab.evaluate(`JSON.stringify({
        inputs: window.__perf.inputs.filter(i => i.painted !== null).map(i => i.painted - i.at),
        long: window.__perf.long,
        frames: (window.renderMarks.domFrames ?? []).slice(${startFrame}),
        prior: (window.renderMarks.domFrames ?? [])[${startFrame - 1}] ?? [0, 0],
        elements: window.__tiloDom?.stats?.elements,
        bytes: window.renderMarks.dom?.meta?.bytes,
      })`));
      // The presenter counts its writes and new elements from the start.
      const sum = index => (page.frames.at(-1)?.[index] ?? page.prior[index]) - page.prior[index];
      const most = index => round(Math.max(0, ...page.frames.map(frame => frame[index] ?? 0)));
      const delta = key => round((after[key] ?? 0) - (before[key] ?? 0));
      result.interactions[name] = {
        inputs: page.inputs.length,
        p50: pct(page.inputs, 0.5),
        max: round(Math.max(0, ...page.inputs)),
        longTasks: page.long,
        frames: page.frames.length,
        // The slowest frame's parts: wasm's layout and encoding, and the presenter's elements.
        layoutMs: most(3), encodeMs: most(4), presentMs: most(2),
        writes: sum(0), created: sum(1),
        styleRecalcs: delta('RecalcStyleCount'), layouts: delta('LayoutCount'),
        scriptMs: round(delta('ScriptDuration') * 1000),
        elements: page.elements,
        kib: Math.round((page.bytes ?? 0) / 1024),
      };
    };
    const at = async name => JSON.parse(await tab.evaluate(NAMED(name)));
    const mouse = (type, x, y, extra = {}) => tab.send('Input.dispatchMouseEvent', {type, x, y, button: 'none', ...extra});
    const click = async point => {
      await mouse('mouseMoved', point.x, point.y);
      await mouse('mousePressed', point.x, point.y, {button: 'left', clickCount: 1});
      await mouse('mouseReleased', point.x, point.y, {button: 'left', clickCount: 1});
    };
    // A pointer moved at 60 Hz from `from` to `to`.
    const sweep = async (from, to, moves, extra) => {
      for (let i = 0; i <= moves; i++) {
        await mouse('mouseMoved', from.x + (to.x - from.x) * i / moves, from.y + (to.y - from.y) * i / moves, extra);
        await sleep(16);
      }
    };

    for (const place of PLACES) {
      const point = await at(place);
      if (!point) { result.interactions[`open ${place}`] = {missing: true}; continue; }
      await step(`open ${place}`, () => click(point));
      if (place === 'Basics') {
        // Rows of the page hovered, top to bottom, and its menu opened and closed.
        await step('hover Basics', () => sweep({x: 700, y: 120}, {x: 700, y: 760}, 40));
        const more = await at('More');
        if (more) {
          await step('open menu', () => click(more));
          await step('close menu', async () => {
            await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
            await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27});
          });
        }
      }
      if (place === 'Controls') {
        const field = JSON.parse(await tab.evaluate(FIELD));
        if (field) {
          await click(field);
          await step('type', async () => {
            for (const ch of 'Typing on the web') {
              await tab.send('Input.dispatchKeyEvent', {type: 'keyDown', key: ch, text: ch, unmodifiedText: ch});
              await sleep(35);
              await tab.send('Input.dispatchKeyEvent', {type: 'keyUp', key: ch});
              await sleep(45);
            }
          });
          // The words reached the field.
          result.interactions.type.typed = await tab.evaluate(
            `[...document.querySelectorAll('#app input')].some(e => e.value.includes('Typing on the web'))`);
        }
      }
      if (place === 'Backlog' || place === 'Data') {
        const scroller = JSON.parse(await tab.evaluate(SCROLLER));
        if (scroller) {
          await step(`hover ${place}`, () => sweep({x: scroller.x - 200, y: scroller.y - 250}, {x: scroller.x + 200, y: scroller.y + 250}, 40));
          await step(`scroll ${place}`, async () => {
            for (let i = 0; i < 30; i++) {
              await mouse('mouseWheel', scroller.x, scroller.y, {deltaX: 0, deltaY: i < 15 ? 100 : -100});
              await sleep(16);
            }
          });
        }
      }
      if (place === 'Media') {
        const ask = await at('Ask');
        if (ask) {
          await step('open dialog', () => click(ask));
          const cancel = await at('Cancel');
          if (cancel) await step('close dialog', () => click(cancel));
        }
      }
      if (place === 'Charts and views') {
        await step('hover Charts and views', () => sweep({x: 400, y: 150}, {x: 1200, y: 700}, 40));
      }
    }
    const edge = JSON.parse(await tab.evaluate(EDGE));
    if (edge) {
      await step('drag sidebar', async () => {
        await mouse('mouseMoved', edge.x, edge.y);
        await mouse('mousePressed', edge.x, edge.y, {button: 'left', clickCount: 1});
        await sweep(edge, {x: edge.x + 120, y: edge.y}, 30, {button: 'left', buttons: 1});
        await sweep({x: edge.x + 120, y: edge.y}, edge, 30, {button: 'left', buttons: 1});
        await mouse('mouseReleased', edge.x, edge.y, {button: 'left', clickCount: 1});
      });
    } else {
      result.interactions['drag sidebar'] = {missing: true};
    }
    // The window made narrower and wider again, a step a frame. A resize is no input event:
    // its frames and tasks say what it cost.
    await step('resize window', async () => {
      for (let i = 0; i <= 40; i++) {
        const width = WIDTH - Math.round(400 * Math.sin(Math.PI * i / 40));
        await tab.send('Emulation.setDeviceMetricsOverride', {width, height: HEIGHT, deviceScaleFactor: 1, mobile: false});
        await sleep(16);
      }
    });
    result.log = tab.log.filter(line => /^(EXC|ERROR)|panic/i.test(line)).slice(0, 12);
  } finally {
    await server.close();
    await browser.close();
  }
  return result;
}

// Where an interaction is past a frame, or the page stopped answering for a long task: what a
// person feels, said every time, failing only against the pinned numbers below.
function overFrame(result) {
  const said = [];
  for (const [name, step] of Object.entries(result.interactions)) {
    if (step.missing) continue;
    if (step.max > INPUT_BUDGET_MS * cpu) said.push(`${name}: an input took ${step.max} ms`);
    const longest = Math.max(0, ...step.longTasks.map(([, duration]) => duration));
    if (longest > LONG_TASK_MS * cpu) said.push(`${name}: a ${longest} ms task`);
  }
  return said;
}

// The numbers a measurement on the builder achieved (`--pin` writes them, with a margin for a
// machine's noise): how many frames each interaction presents, the elements it makes, the size of
// its composition, and its slowest input and longest task. More than that is a regression.
const PINNED = new URL('./tilolang-perf.pinned.json', import.meta.url);
const longestOf = step => Math.max(0, ...(step.longTasks ?? []).map(([, duration]) => duration));
function pin(result) {
  const widen = (n, by, plus) => Math.ceil(n * by + plus);
  const pinned = {startup: {firstFrameMs: widen(result.startup.firstFrameMs, 2, 0), transferredKiB: widen(result.startup.transferredKiB, 1.05, 0)}, interactions: {}};
  for (const [name, step] of Object.entries(result.interactions)) {
    if (step.missing) continue;
    pinned.interactions[name] = {frames: widen(step.frames, 1.25, 2), created: widen(step.created, 1.1, 4),
      kib: widen(step.kib, 1.1, 2), maxMs: widen(step.max, 2, 10), longestTaskMs: widen(longestOf(step), 2, 50)};
  }
  return pinned;
}
function regressions(result, pinned) {
  const said = [];
  if (result.startup.firstFrameMs > pinned.startup.firstFrameMs * cpu) said.push(`startup: first frame at ${result.startup.firstFrameMs} ms`);
  if (result.startup.transferredKiB > pinned.startup.transferredKiB) said.push(`startup: ${result.startup.transferredKiB} KiB loaded`);
  for (const [name, limit] of Object.entries(pinned.interactions)) {
    const step = result.interactions[name];
    if (!step || step.missing) { said.push(`${name}: nothing to act on`); continue; }
    const measured = {frames: step.frames, created: step.created, kib: step.kib, maxMs: step.max, longestTaskMs: longestOf(step)};
    for (const [key, value] of Object.entries(measured)) {
      const ceiling = key.endsWith('Ms') ? limit[key] * cpu : limit[key];
      if (value > ceiling) said.push(`${name}: ${key} ${value}, pinned at ${ceiling}`);
    }
  }
  return said;
}

let result;
let missed = [];
for (let attempt = 1; attempt <= attempts; attempt++) {
  result = await measure();
  missed = budget ? regressions(result, JSON.parse(readFileSync(PINNED, 'utf8'))) : [];
  if (!missed.length) break;
}
if (process.argv.includes('--pin')) writeFileSync(PINNED, `${JSON.stringify(pin(result), null, 2)}\n`);
if (out) {
  mkdirSync(out, {recursive: true});
  writeFileSync(join(out, 'perf.json'), JSON.stringify(result, null, 2));
}
console.log(`startup: first frame ${result.startup.firstFrameMs} ms, first paint ${result.startup.firstContentfulPaintMs} ms, `
  + `${result.startup.transferredKiB} KiB, ${result.startup.elements} elements; largest (KiB, loaded at ms): `
  + result.startup.largest.map(([name, kib, at]) => `${name} ${kib} @${at}`).join(', '));
const columns = ['inputs', 'p50', 'max', 'frames', 'layoutMs', 'encodeMs', 'presentMs', 'writes', 'created', 'styleRecalcs', 'layouts', 'scriptMs', 'elements', 'kib'];
console.log(['interaction'.padEnd(24), ...columns.map(c => c.padStart(9)), '  long tasks'].join(''));
for (const [name, step] of Object.entries(result.interactions)) {
  console.log([name.padEnd(24), ...columns.map(c => String(step[c] ?? '-').padStart(9)),
    `  ${(step.longTasks ?? []).map(([, d]) => d).join(',')}`].join(''));
}
for (const line of result.log ?? []) console.log(line);
for (const line of overFrame(result)) console.log(`over a frame: ${line}`);
if (missed.length) {
  console.error(`past the pinned numbers:\n  ${missed.join('\n  ')}`);
  process.exit(1);
}

// How fast a tilolang package feels as HTML, in headless Chromium: the page started, every
// sidebar place chosen, rows hovered, a field typed into, long lists scrolled, the sidebar
// dragged wider, the window resized, a menu and a dialog opened. For each, what a person
// waits for: from each input to the end of the rendering update after it (`p50`/`max`, ms),
// the long tasks it caused, and the work behind it: frames presented, the runtime's time in
// wasm (update and layout, then the composition encoded) and the presenter's, elements
// written and made, and the browser's style and layout passes and the time they took. A click on a place of the
// sidebar that leaves another page showing (the sidebar marking another, or the page beside it
// titled otherwise) fails, whatever the numbers.
//
// usage: node tools/web/html-renderer/tilolang-perf.mjs BUNDLE_DIR [OUT_DIR] [--package tl/<id>/]
//                                                      [--budget] [--pin] [--attempts N] [--cpu N]
//                                                      [--profile DIR]
//
// BUNDLE_DIR is build-tilolang-web.sh's output (a release build: a debug one is several times
// slower). Every interaction past a frame (25 ms from input to its paint, with a margin for a headless
// browser's own scheduling) or with a task over 50 ms is said.
//
// The times are the page's own: the CPU time its main thread spent (Chromium's trace counts each
// task's thread time) from each input to the paint after it, in each task, and from the start to
// the first frame. A machine whose
// other work keeps the page waiting for a core stretches the clock on the wall, not these, so a
// loaded builder measures what an idle one does, and only more work from the page fails.
// `wallP50`/`wallMax` keep the times on the wall beside them. `--budget` fails when the numbers
// are past those pinned in tilolang-perf.pinned.json, which `--pin` writes from the measurement
// with a margin for noise; they are pinned on the builder (a cargo drop-in's `exec`, CHROME=chromium-browser),
// whose idle cores measure steadier than a shared laptop. `--attempts N` measures up to N times
// and passes on the first within them. `--profile DIR` keeps a CPU profile of the start and of
// each interaction there, as `<interaction>.cpuprofile` (DevTools opens it). `--cpu N` slows the
// page's CPU N times (Chromium's throttling, which holds the page back, so it shows in the times on
// the wall and not in the CPU times the budget reads), and the pinned times with it.
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
// it, and the presenter's frames as they come.
const CLOCK = `(() => {
  const perf = window.__perf = {inputs: []};
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
})()`;

// The renderer's tasks, traced: each top-level task on a thread with its time on the wall and
// the CPU time the thread spent in it, and the marks that place the page's clock on the trace's.
const TRACE_CATEGORIES = ['toplevel', 'devtools.timeline', 'disabled-by-default-devtools.timeline', 'blink.user_timing'];
const SYNC_MARK = 'tilo-perf:sync';
// Chromium's name for a top-level task in its trace.
const TASK_EVENT = 'RunTask';

// `act` returns the name of a mark the page made and its time in the page's clock, which place
// the page's clock on the trace's.
async function traced(browser, tab, act) {
  const events = [];
  const off = browser.on(message => {
    if (message.method === 'Tracing.dataCollected') events.push(...message.params.value);
  });
  await tab.send('Tracing.start', {transferMode: 'ReportEvents', traceConfig: {includedCategories: TRACE_CATEGORIES}});
  let anchor;
  try {
    anchor = await act();
  } finally {
    const done = new Promise(resolve => {
      const stop = browser.on(message => { if (message.method === 'Tracing.tracingComplete') { stop(); resolve(); } });
    });
    await tab.send('Tracing.end');
    await done;
    off();
  }
  return mainThreadTasks(events, anchor);
}

// The page's clock at a mark the trace also holds, made now.
const synced = async tab => [SYNC_MARK, await tab.evaluate(`performance.mark(${JSON.stringify(SYNC_MARK)}).startTime`)];

// The page's main thread's tasks, `[start, end, cpu]` in the page's clock (ms), from a trace.
// Chromium leaves the thread time off a few tasks of a few microseconds, which count their wall
// time; a trace with none at all (a platform that keeps none) counts the wall time throughout.
function mainThreadTasks(events, [anchor, at]) {
  const names = new Map();
  for (const event of events) {
    if (event.ph === 'M' && event.name === 'thread_name') names.set(`${event.pid}:${event.tid}`, event.args?.name);
  }
  // The tab's renderer's main thread: the one that made the mark.
  const mark = events.find(event => event.name === anchor);
  const main = mark && `${mark.pid}:${mark.tid}`;
  if (!mark || !/CrRendererMain/.test(names.get(main) ?? '')) return {tasks: [], threadTime: false};
  const offset = mark.ts / 1000 - at;
  let threadTime = false;
  const tasks = [];
  for (const event of events) {
    if (event.ph !== 'X' || event.name !== TASK_EVENT || `${event.pid}:${event.tid}` !== main) continue;
    if (event.tdur !== undefined) threadTime = true;
    const start = event.ts / 1000 - offset;
    tasks.push([start, start + event.dur / 1000, (event.tdur ?? event.dur) / 1000]);
  }
  return {tasks, threadTime};
}

// The main thread's CPU time between `from` and `to`, in the page's clock: each task's share of
// its own CPU time, by how much of it falls between them.
function cpuBetween(tasks, from, to) {
  let spent = 0;
  for (const [start, end, cpu] of tasks) {
    const within = Math.min(end, to) - Math.max(start, from);
    if (within > 0) spent += end > start ? cpu * within / (end - start) : 0;
  }
  return spent;
}

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
// What the sidebar shows picked: the words of its selected row.
const PICKED = `(() => {
  const row = document.querySelector('#app [data-widget=NavList] [aria-selected=true], #app [data-widget=NavList] [aria-current]');
  return (row?.getAttribute('aria-label') ?? row?.textContent ?? '').trim();
})()`;
// The page's titles: the headings beside the sidebar, the window's and the page's own.
const TITLES = `JSON.stringify([...document.querySelectorAll('#app h1, #app h2, #app [role=heading]')]
  .filter(element => !element.closest('[data-widget=NavList]')).map(element => element.textContent.trim()))`;
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
    // The start traced, placed on the page's clock by the runtime's own mark of its first frame.
    const FIRST_FRAME = 'tilo:first-frame';
    const start = await traced(browser, tab, async () => {
      await tab.navigate(`${server.url}/app.html?package=${encodeURIComponent(pkg)}&scenario=all&render=dom&theme=light`);
      await tab.waitFor('window.renderMarks?.presented?.length > 0', {timeout: 120000});
      await settle();
      return [FIRST_FRAME, await tab.evaluate(`performance.getEntriesByName(${JSON.stringify(FIRST_FRAME)})[0]?.startTime ?? -1`)];
    });
    await profiler.stop('startup');
    result.startup = JSON.parse(await tab.evaluate(`JSON.stringify((() => {
      const m = window.renderMarks;
      const paint = performance.getEntriesByName('first-contentful-paint')[0]?.startTime;
      const resources = performance.getEntriesByType('resource');
      const bytes = resources.reduce((sum, r) => sum + (r.encodedBodySize || 0), 0);
      return {firstFrameMs: Math.round(m.presented[0]), firstContentfulPaintMs: Math.round(paint ?? -1),
        settledFrames: (m.domFrames ?? []).length, elements: window.__tiloDom?.stats?.elements,
        transferredKiB: Math.round(bytes / 1024),
        // The runtime's own startup timeline (its \`tilo:\` marks), so a slow phase names itself.
        marks: Object.fromEntries(performance.getEntriesByType('mark').filter(m => m.name.startsWith('tilo:'))
          .map(m => [m.name.slice(5), Math.round(m.startTime)])),
        largest: resources.sort((a, b) => b.encodedBodySize - a.encodedBodySize).slice(0, 6)
          .map(r => [r.name.split('/').slice(-2).join('/'), Math.round(r.encodedBodySize / 1024), Math.round(r.responseEnd)])};
    })())`));
    // What the start cost the page's main thread up to its first frame.
    result.startup.firstFrameCpuMs = Math.round(cpuBetween(start.tasks, -Infinity, result.startup.firstFrameMs));
    result.startup.threadTime = start.threadTime;

    const step = async (name, act) => {
      await settle();
      const before = await metrics();
      const startFrame = await frames();
      await tab.evaluate('window.__perf.inputs = []; true');
      await profiler.start();
      const trace = await traced(browser, tab, async () => {
        const anchor = await synced(tab);
        await act();
        await settle();
        return anchor;
      });
      await profiler.stop(name);
      const after = await metrics();
      const page = JSON.parse(await tab.evaluate(`JSON.stringify({
        inputs: window.__perf.inputs.filter(i => i.painted !== null).map(i => [i.at, i.painted]),
        frames: (window.renderMarks.domFrames ?? []).slice(${startFrame}),
        prior: (window.renderMarks.domFrames ?? [])[${startFrame - 1}] ?? [0, 0],
        elements: window.__tiloDom?.stats?.elements,
        bytes: window.renderMarks.dom?.meta?.bytes,
      })`));
      // The presenter counts its writes and new elements from the start.
      const sum = index => (page.frames.at(-1)?.[index] ?? page.prior[index]) - page.prior[index];
      const most = index => round(Math.max(0, ...page.frames.map(frame => frame[index] ?? 0)));
      const delta = key => round((after[key] ?? 0) - (before[key] ?? 0));
      // What each input cost the page, and what it waited on the wall.
      const spent = page.inputs.map(([from, to]) => cpuBetween(trace.tasks, from, to));
      const waited = page.inputs.map(([from, to]) => to - from);
      result.interactions[name] = {
        inputs: page.inputs.length,
        p50: pct(spent, 0.5),
        max: round(Math.max(0, ...spent)),
        wallP50: pct(waited, 0.5),
        wallMax: round(Math.max(0, ...waited)),
        threadTime: trace.threadTime,
        longTasks: trace.tasks.filter(([, , cpu]) => cpu > LONG_TASK_MS)
          .map(([start, , cpu]) => [Math.round(start), Math.round(cpu)]),
        frames: page.frames.length,
        // The slowest frame's parts: wasm's layout and encoding, and the presenter's elements.
        layoutMs: most(3), encodeMs: most(4), presentMs: most(2),
        writes: sum(0), created: sum(1),
        styleRecalcs: delta('RecalcStyleCount'), layouts: delta('LayoutCount'),
        // The browser's own time on the elements: their styles, then their boxes.
        styleMs: round(delta('RecalcStyleDuration') * 1000), boxesMs: round(delta('LayoutDuration') * 1000),
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
    // The next input waits a 60 Hz frame, and for the page's frame after the last one: a page
    // the machine keeps waiting gets each input in a frame of its own, as an idle one does, so
    // what the inputs make does not hang on how many of them a loaded machine lands in one frame.
    const nextInput = () => Promise.all([sleep(16),
      tab.evaluate('new Promise(done => requestAnimationFrame(() => setTimeout(done, 0)))')]);
    // A pointer moved at 60 Hz from `from` to `to`.
    const sweep = async (from, to, moves, extra) => {
      for (let i = 0; i <= moves; i++) {
        await mouse('mouseMoved', from.x + (to.x - from.x) * i / moves, from.y + (to.y - from.y) * i / moves, extra);
        await nextInput();
      }
    };

    for (const place of PLACES) {
      const point = await at(place);
      if (!point) { result.interactions[`open ${place}`] = {missing: true}; continue; }
      await step(`open ${place}`, () => click(point));
      // The page a click picked is the one shown once its frames settled, not the one before:
      // the sidebar marks it, and the page beside it is titled with it.
      result.interactions[`open ${place}`].picked = await tab.evaluate(PICKED);
      result.interactions[`open ${place}`].titled = JSON.parse(await tab.evaluate(TITLES)).includes(place);
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
              await nextInput();
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
        await nextInput();
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

// Where a click on a place in the sidebar left another page showing: a wrong page is no
// matter of speed, and fails every measurement, pinned or not.
function wrongPages(result) {
  const said = [];
  for (const [name, step] of Object.entries(result.interactions)) {
    if (step.picked === undefined) continue;
    const place = name.replace(/^open /, '');
    if (step.picked !== place) said.push(`${name}: the sidebar shows ${JSON.stringify(step.picked)} picked`);
    if (!step.titled) said.push(`${name}: the page beside the sidebar is not ${JSON.stringify(place)}`);
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
  const pinned = {startup: {firstFrameMs: widen(result.startup.firstFrameCpuMs, 2, 0), transferredKiB: widen(result.startup.transferredKiB, 1.05, 0)}, interactions: {}};
  for (const [name, step] of Object.entries(result.interactions)) {
    if (step.missing) continue;
    pinned.interactions[name] = {frames: widen(step.frames, 1.25, 2), created: widen(step.created, 1.1, 4),
      kib: widen(step.kib, 1.1, 2), maxMs: widen(step.max, 2, 10), longestTaskMs: widen(longestOf(step), 2, 50)};
  }
  return pinned;
}
function regressions(result, pinned) {
  const said = [];
  if (result.startup.firstFrameCpuMs > pinned.startup.firstFrameMs * cpu) said.push(`startup: ${result.startup.firstFrameCpuMs} ms of work to the first frame, pinned at ${pinned.startup.firstFrameMs * cpu}`);
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
  missed = [...wrongPages(result), ...(budget ? regressions(result, JSON.parse(readFileSync(PINNED, 'utf8'))) : [])];
  if (!missed.length) break;
}
if (process.argv.includes('--pin')) writeFileSync(PINNED, `${JSON.stringify(pin(result), null, 2)}\n`);
if (out) {
  mkdirSync(out, {recursive: true});
  writeFileSync(join(out, 'perf.json'), JSON.stringify(result, null, 2));
}
console.log(`startup: first frame ${result.startup.firstFrameMs} ms (${result.startup.firstFrameCpuMs} ms of the page's work), first paint ${result.startup.firstContentfulPaintMs} ms, `
  + `${result.startup.transferredKiB} KiB, ${result.startup.elements} elements; largest (KiB, loaded at ms): `
  + result.startup.largest.map(([name, kib, at]) => `${name} ${kib} @${at}`).join(', '));
console.log(`startup marks (ms): ${Object.entries(result.startup.marks ?? {}).map(([name, at]) => `${name} ${at}`).join(', ')}`);
const columns = ['inputs', 'p50', 'max', 'wallP50', 'wallMax', 'frames', 'layoutMs', 'encodeMs', 'presentMs', 'writes', 'created', 'styleRecalcs', 'layouts', 'styleMs', 'boxesMs', 'scriptMs', 'elements', 'kib'];
console.log(['interaction'.padEnd(24), ...columns.map(c => c.padStart(9)), '  long tasks'].join(''));
for (const [name, step] of Object.entries(result.interactions)) {
  console.log([name.padEnd(24), ...columns.map(c => String(step[c] ?? '-').padStart(9)),
    `  ${(step.longTasks ?? []).map(([, d]) => d).join(',')}`].join(''));
}
for (const line of result.log ?? []) console.log(line);
if (!result.startup.threadTime || Object.values(result.interactions).some(step => step.threadTime === false)) {
  console.log('this browser traced no thread time: the times are on the wall, and a loaded machine stretches them');
}
for (const line of overFrame(result)) console.log(`over a frame: ${line}`);
if (missed.length) {
  console.error(`failed:\n  ${missed.join('\n  ')}`);
  process.exit(1);
}

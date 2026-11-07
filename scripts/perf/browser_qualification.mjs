#!/usr/bin/env node
// The browser runtime's performance qualification: one release desktop bundle
// (crates/browser/tilo-browser-runtime/scripts/build.sh OUT --bundle desktop)
// in headless Chrome, measured the way a person uses it, and checked against
// the runtime's documented goals. It writes one report; `tools/perf/adapters.py
// browser-qualification` turns that into the `browser` KPI group's records
// (t's perf-browser, tools/perf/kpi-budgets.toml).
//
// usage: node scripts/perf/browser_qualification.mjs BUNDLE_DIR --out REPORT_JSON
//          --renderer gpu|swiftshader [--rounds N]
//          [--activations N] [--demo-seconds S] [--idle-seconds S]
//          [--typing-keys N] [--expect-revision SHA] [--allow-dev] [--max-load L]
//          [--cpu-throttle F] [--frame-timeout S] [--viewport WxH] [--enforce-goals]
//
// Each round is a fresh browser context (empty HTTP cache and storage) at the
// workload's viewport (FHD, 1920x1080 at DPR 1, unless --viewport), and
// measures, in order:
//   load      navigation to the desktop's first presented frame
//   open      every demo app launched once: launch command to the first
//             presented frame holding its new window, and to the first frame
//             after the app's own code (its split-off module) was fetched and
//             instantiated, as on a first open (`ready`)
//   activate  an open window selected: select command to the first presented
//             frame with it active (--activations per round)
//   demo      the demo workload: a strip of the demo windows panned back and
//             forth by trackpad-rate (120 Hz) wheel input for --demo-seconds;
//             the interval between consecutive presented frames, and the
//             runtime's CPU time for each presented frame (`draw_total`),
//             after the same pan once unmeasured, which compiles its pipelines
//   idle     frames presented per second by the same desktop left alone
//   typing   keys typed into a note at a fast typist's rate (--typing-keys,
//             off by default): each key's time in the page to the first
//             presented frame after it, the runtime's CPU time per typing
//             frame and what each of those frames repainted
//
// --frame-timeout S is how long one awaited frame may take before the run
// is invalid: 10 s on a GPU, 120 s on a software rasterizer, which compiles
// each new pipeline on the CPU when it is first used.
//
// --cpu-throttle F slows the page's CPU F times (DevTools CPU throttling), a
// stand-in for older hardware on a fast host; it is part of the report's
// workload, and its records are a different series.
//
// Chrome runs with its frame-rate limit and vsync off, so a frame is presented
// as soon as the runtime has one: the 120 Hz input, not a 60 Hz headless
// display, bounds the demo's frame rate, and the 120 fps goal is measurable.
//
// Goals (reported in the summary; enforced only with --enforce-goals, which a
// hardware qualification uses — budgets on the scheduled lane are
// compare.py's):
//   demo minimum   30 fps: p95 frame interval <= 33.3 ms
//   demo goal      120 fps: p50 frame interval and p95 frame work <= 8.33 ms
//   activation     p99 <= 50 ms
//   open           p99 <= 250 ms (crates/browser/tilo-web-desktop/scripts/open-times.mjs),
//                  the app's own code drawn within 1 s
//   load           p99 <= 2.5 s (the web's "good" Largest Contentful Paint)
//   idle           <= 2 presented frames per second
//   typing         key to presented frame p95 <= 50 ms (a key that reaches the
//                  screen within three frames at 60 Hz), judged when the
//                  typing workload ran
// Beside load's time, the report records what the first load transferred and
// how many requests it took (Performance Resource Timing: every resource that
// started before the desktop's first presented frame), which the browser
// group's budgets hold as `browser.load.transfer_bytes` and
// `browser.load.request_count`.
// The demo's runtime stages are attributed per presented frame in the report
// (`attribution`), so a missed goal names where its frame time went.
//
// The run is invalid, and exits 2 without a report, when it cannot observe its
// subject: the bundle's bytes differ from its build.json, it is not a clean
// release build (or not --expect-revision), the page loaded a module the
// bundle does not list, the WebGPU adapter is not --renderer, the host was
// busier than --max-load (load average per CPU) before or after, a page threw,
// or a measurement produced no samples.
import {createHash} from 'node:crypto';
import {existsSync, readFileSync, writeFileSync} from 'node:fs';
import os from 'node:os';
import {join, resolve} from 'node:path';
import {verb} from '../../crates/browser/tilo-browser-runtime/web/desktop-navigation.mjs';
import {launch, serve, sleep} from '../../tools/web/chrome.mjs';

export const GOALS = {
  demoMinimumIntervalMs: 1000 / 30,
  demoGoalIntervalMs: 1000 / 120,
  activationMs: 50,
  openMs: 250,
  openReadyMs: 1000,
  loadMs: 2500,
  idlePerS: 2,
  typingMs: 50,
};
// Desktop windows of the demo: the apps of the launch demo that open without
// an account or a network, in the order they are opened.
export const DEMO_APPS = ['so.tilo.notes', 'so.tilo.sheets', 'so.tilo.word-processor', 'so.tilo.calendar',
  'so.tilo.calculator', 'so.tilo.slides'];
export const RENDERERS = ['gpu', 'swiftshader'];
// FHD, the older-hardware target; --viewport overrides it.
const VIEWPORT = {width: 1920, height: 1080, deviceScaleFactor: 1};
const USAGE = 'usage: browser_qualification.mjs BUNDLE_DIR --out REPORT_JSON --renderer gpu|swiftshader '
  + '[--rounds N] [--activations N] [--demo-seconds S] [--idle-seconds S] [--typing-keys N] [--expect-revision SHA] '
  + '[--allow-dev] [--max-load L] [--cpu-throttle F] [--frame-timeout S] [--viewport WxH] [--enforce-goals]';

export class Invalid extends Error {}

/** `WIDTHxHEIGHT` in CSS pixels at DPR 1. */
export function parseViewport(text) {
  const match = /^(\d{3,4})x(\d{3,4})$/.exec(text ?? '');
  if (!match) throw Error('--viewport is WIDTHxHEIGHT, for example 1920x1080');
  return {width: Number(match[1]), height: Number(match[2]), deviceScaleFactor: 1};
}

/** `promise`, or an Invalid run after `ms`: a step that never answers must fail, not hang. */
export function within(promise, ms, what) {
  let timer;
  const deadline = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Invalid(`${what} did not answer within ${Math.round(ms / 1000)} s`)), ms);
  });
  return Promise.race([promise, deadline]).finally(() => clearTimeout(timer));
}

export function parseArgs(argv) {
  const options = {rounds: 3, activations: 20, demoSeconds: 5, idleSeconds: 3, typingKeys: 0, cpuThrottle: 1,
    allowDev: false, enforceGoals: false};
  const positionals = [];
  const numbers = {'--rounds': 'rounds', '--activations': 'activations', '--demo-seconds': 'demoSeconds',
    '--idle-seconds': 'idleSeconds', '--typing-keys': 'typingKeys', '--max-load': 'maxLoad',
    '--cpu-throttle': 'cpuThrottle', '--frame-timeout': 'frameTimeout'};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg in numbers) {
      const value = Number(argv[++i]);
      // `--demo-seconds 0` and `--typing-keys 0` leave that workload out.
      const least = ['--demo-seconds', '--typing-keys'].includes(arg) ? 0 : Number.MIN_VALUE;
      if (!Number.isFinite(value) || value < least) throw Error(`${arg} needs a positive number`);
      options[numbers[arg]] = value;
    } else if (arg === '--out') options.out = argv[++i];
    else if (arg === '--renderer') options.renderer = argv[++i];
    else if (arg === '--viewport') options.viewport = parseViewport(argv[++i]);
    else if (arg === '--expect-revision') options.expectRevision = argv[++i];
    else if (arg === '--allow-dev') options.allowDev = true;
    else if (arg === '--enforce-goals') options.enforceGoals = true;
    else if (arg.startsWith('--')) throw Error(`unknown option ${arg}\n${USAGE}`);
    else positionals.push(arg);
  }
  if (positionals.length !== 1 || !options.out || !options.renderer) throw Error(USAGE);
  if (!RENDERERS.includes(options.renderer)) throw Error(`--renderer must be one of ${RENDERERS.join(', ')}`);
  if (options.cpuThrottle < 1) throw Error('--cpu-throttle is a slowdown factor of at least 1');
  // A software rasterizer compiles each new pipeline on the CPU when it is
  // first used; one frame can then take tens of seconds.
  options.frameTimeout ??= options.renderer === 'gpu' ? 10 : 120;
  options.viewport ??= VIEWPORT;
  for (const name of ['rounds', 'activations', 'typingKeys']) {
    if (!Number.isInteger(options[name])) throw Error(`--${name} must be a whole number`);
  }
  return {...options, bundle: resolve(positionals[0])};
}

/** Nearest-rank percentile of ascending `sorted` (tools/perf/record.py's rule). */
export function percentile(sorted, p) {
  if (!sorted.length) return NaN;
  return sorted[Math.max(0, Math.ceil(p / 100 * sorted.length) - 1)];
}

export function stats(samples) {
  const sorted = samples.filter(Number.isFinite).sort((a, b) => a - b);
  const round = value => Math.round(value * 1000) / 1000;
  return {n: sorted.length, p50: round(percentile(sorted, 50)), p95: round(percentile(sorted, 95)),
    p99: round(percentile(sorted, 99)), max: round(sorted.at(-1) ?? NaN)};
}

/** The bundle's identity: build.json checked against every byte it lists. */
export function verifyBundle(dir, {allowDev = false, expectRevision} = {}) {
  const path = join(dir, 'build.json');
  if (!existsSync(path)) throw new Invalid(`${dir} has no build.json: not a build.sh bundle`);
  const build = JSON.parse(readFileSync(path, 'utf8'));
  const files = build.files ?? {};
  const problems = [];
  if (build.bundle !== 'desktop') problems.push(`bundle is ${build.bundle}, not desktop`);
  if (!allowDev && build.profile !== 'release') problems.push(`profile is ${build.profile}, not release`);
  if (!allowDev && build.dirty) problems.push(`built from a dirty tree at ${build.revision}`);
  if (expectRevision && build.revision !== expectRevision) {
    problems.push(`built from ${build.revision}, not ${expectRevision}`);
  }
  if (!files['pkg/tilo_browser_runtime_bg.wasm']) problems.push('lists no runtime module');
  const mismatched = [];
  for (const [name, hash] of Object.entries(files)) {
    const file = join(dir, name);
    if (!existsSync(file) || createHash('sha256').update(readFileSync(file)).digest('hex') !== hash) mismatched.push(name);
  }
  if (mismatched.length) problems.push(`${mismatched.length} files differ from build.json: ${mismatched.slice(0, 5).join(', ')}`);
  if (problems.length) throw new Invalid(`bundle ${dir}: ${problems.join('; ')}`);
  const digest = createHash('sha256').update(Object.keys(files).sort().map(name => `${name} ${files[name]}\n`).join('')).digest('hex');
  return {revision: build.revision, dirty: Boolean(build.dirty), profile: build.profile, cargoProfile: build.cargoProfile,
    wasmOpt: Boolean(build.wasmOpt), rustc: build.rustc, wasmBindgen: build.wasmBindgen, files: Object.keys(files).length,
    runtimeSha256: files['pkg/tilo_browser_runtime_bg.wasm'], digest};
}

/** Same-origin files the page received (`{name, status}` resource entries) that the bundle does not list. */
export function unlistedResources(entries, origin, files) {
  const listed = new Set(files);
  return entries.filter(entry => entry.status >= 200 && entry.status < 300).map(entry => new URL(entry.name))
    .filter(url => url.origin === origin)
    .map(url => decodeURIComponent(url.pathname).replace(/^\//, '') || 'index.html')
    .map(name => name.endsWith('/') ? `${name}index.html` : name)
    .filter(name => !listed.has(name));
}

/**
 * What the page transferred until `until` (the desktop's first presented frame),
 * from the Performance Resource Timing API: every same-origin resource's
 * encoded body bytes, and how many requests it took. A resource the page has
 * no entry for yet (a worker's, a stream) is not in either.
 */
export function transfer(entries, origin, until) {
  const resources = entries.filter(entry => entry.startTime <= until);
  const bytes = resources.reduce((sum, entry) => sum + (Number(entry.encodedBodySize) || 0), 0);
  return {bytes, requests: resources.length, crossOrigin: resources.filter(entry => entry.origin !== origin).length};
}

/** The renderer a WebGPU adapter's info names. */
export function classifyAdapter(info) {
  const text = Object.values(info ?? {}).join(' ').toLowerCase();
  if (!text.trim()) return 'unknown';
  if (text.includes('swiftshader')) return 'swiftshader';
  // Mesa's Vulkan rasterizer names its device after llvmpipe, its core.
  if (text.includes('lavapipe') || text.includes('llvmpipe')) return 'lavapipe';
  return 'gpu';
}

/**
 * Chrome's flags for a run on `renderer` by user `uid`: no frame-rate limit
 * or vsync; SwiftShader's Vulkan and ANGLE on SwiftShader (as
 * crates/browser/tilo-web-desktop/scripts/navigation.mjs starts it), or, for a
 * hardware run, the vulkan backend a machine's own driver serves WebGPU from
 * (headless Chrome settles on SwiftShader without it); no sandbox for root (a
 * CI container), which Chrome refuses otherwise. The adapter check still
 * proves which renderer ran: a hardware run on a machine whose Vulkan driver
 * offers WebGPU no adapter is reported invalid, never measured as a GPU.
 */
export function chromeFlags(renderer, uid) {
  return ['--disable-frame-rate-limit', '--disable-gpu-vsync',
    ...(renderer === 'swiftshader'
      ? ['--enable-features=Vulkan', '--use-vulkan=swiftshader', '--use-angle=swiftshader']
      : ['--enable-features=Vulkan', '--use-vulkan=native', '--use-angle=vulkan',
        '--ignore-gpu-blocklist', '--disable-gpu-driver-bug-workarounds']),
    ...(uid === 0 ? ['--no-sandbox'] : [])];
}

/** Intervals between consecutive presented frames, from their timestamps. */
export function intervals(times) {
  const out = [];
  for (let i = 1; i < times.length; i++) out.push(times[i] - times[i - 1]);
  return out;
}

/** Each goal with its measured value, verdict and remaining gap. */
export function judgeGoals(summary) {
  const goal = (name, measured, target, extra = {}) => ({name, measured, target, met: measured <= target,
    gap: Math.max(0, Math.round((measured - target) * 1000) / 1000), ...extra});
  const fps = interval => Math.round(1000 / interval * 10) / 10;
  // A goal whose workload did not run (no demo) is not judged.
  return [
    goal('demo minimum 30 fps (p95 frame interval, ms)', summary.demo_interval_ms.p95, GOALS.demoMinimumIntervalMs,
      {fps: fps(summary.demo_interval_ms.p95)}),
    goal('demo goal 120 fps (p50 frame interval, ms)', summary.demo_interval_ms.p50, GOALS.demoGoalIntervalMs,
      {fps: fps(summary.demo_interval_ms.p50)}),
    goal('demo goal 120 fps (p95 frame work, ms)', summary.demo_work_ms.p95, GOALS.demoGoalIntervalMs,
      {capacityFps: fps(summary.demo_work_ms.p95)}),
    goal('activation (p99, ms)', summary.activate_ms.p99, GOALS.activationMs),
    goal('open (p99, ms)', summary.open_ms.p99, GOALS.openMs),
    goal("open, app's code drawn (p99, ms)", summary.open_ready_ms.p99, GOALS.openReadyMs),
    goal('load (p99, ms)', summary.load_ms.p99, GOALS.loadMs),
    goal('idle (presented frames per second, max)', summary.idle_per_s.max, GOALS.idlePerS),
    goal('typing, key to presented frame (p95, ms)', summary.typing_ms.p95, GOALS.typingMs),
  ].filter(entry => Number.isFinite(entry.measured));
}

// A round without the demo (`--demo-seconds 0`).
const NO_DEMO = {intervals: [], work: [], stages: {}, frames: 0, wheels: 0, seconds: 0, fps: NaN, distinctOffsets: 0};
// A round without typing (`--typing-keys 0`).
const NO_TYPING = {keys: 0, latencies: [], work: [], stages: {}, damage: {}, requesters: {}, text: null};

export function hostState() {
  const [one, five] = os.loadavg();
  const cpus = os.cpus();
  return {load1: Math.round(one * 100) / 100, load5: Math.round(five * 100) / 100, cpus: cpus.length,
    cpu: cpus[0]?.model?.trim() ?? 'unknown', memoryGiB: Math.round(os.totalmem() / 2 ** 30)};
}

function checkHost(state, maxLoad, when) {
  if (maxLoad !== undefined && state.load1 / state.cpus > maxLoad) {
    throw new Invalid(`host contended ${when}: load ${state.load1} on ${state.cpus} CPUs exceeds ${maxLoad} per CPU`);
  }
}

// --- in the page --------------------------------------------------------------

// Installed before the page's scripts: when the mount first reports each
// phase, and when each split-off module (pkg/split.json) is instantiated.
const PHASE_PROBE = `(() => {
  const times = globalThis.__tiloPhases = {};
  new MutationObserver(records => { for (const r of records) { const phase = r.target.dataset?.phase;
    if (phase && !(phase in times)) times[phase] = performance.now(); } })
    .observe(document, {subtree: true, attributes: true, attributeFilter: ['data-phase']});
  const modules = globalThis.__tiloModules = {}, urls = new WeakMap();
  const file = url => new URL(url).pathname.split('/').pop();
  const instantiateStreaming = WebAssembly.instantiateStreaming;
  WebAssembly.instantiateStreaming = async (source, imports) => {
    const response = await source, result = await instantiateStreaming(response, imports);
    modules[file(response.url)] = performance.now();
    return result;
  };
  // The split loader compiles a module ahead of its first use (a prefetch)
  // and instantiates the compiled module when the app first needs it.
  const compileStreaming = WebAssembly.compileStreaming, instantiate = WebAssembly.instantiate;
  WebAssembly.compileStreaming = async source => {
    const response = await source, module = await compileStreaming(response);
    urls.set(module, response.url);
    return module;
  };
  WebAssembly.instantiate = async (source, imports) => {
    const result = await instantiate(source, imports);
    if (urls.has(source)) modules[file(urls.get(source))] = performance.now();
    return result;
  };
})();`;

const RUNTIME = `(await import(new URL('pkg/tilo_browser_runtime.js', document.baseURI).href))`;

// Every resource the page fetched, with what it cost and when it started: the
// load KPI's bytes and request count are those that started before the
// desktop's first presented frame (`globalThis.__tiloPhases.presented`).
const RESOURCES = `performance.getEntriesByType('resource').map(e => ({name: e.name, status: e.responseStatus ?? 200,
  origin: new URL(e.name).origin, startTime: e.startTime, encodedBodySize: e.encodedBodySize}))`;

// Resolves with the time of the first presented frame after `frames` for
// which `ready()` holds, or NaN after `timeoutMs`.
const presentedHelper = timeoutMs => `
  const host = tiloMount.canvas.parentElement;
  const presentedAfter = (frames, ready) => new Promise(done => {
    const timer = setTimeout(() => { observer.disconnect(); done(NaN); }, ${timeoutMs});
    const observer = new MutationObserver(() => {
      if (host.dataset.phase === 'presented' && Number(host.dataset.frames) > frames && ready()) {
        observer.disconnect(); clearTimeout(timer); done(performance.now());
      }
    });
    observer.observe(host, {attributes: true, attributeFilter: ['data-frames']});
  });`;

// `files`: the app's split-off modules; ready once each is instantiated.
const openExpression = (app, files, timeoutMs) => `(async () => {
  ${presentedHelper(timeoutMs)}
  const nav = tiloMount.navigation, snapshot = nav.getSnapshot(), before = snapshot.entries.length;
  const frames = Number(host.dataset.frames), files = ${JSON.stringify(files)};
  const opened = () => nav.getSnapshot().entries.length === before + 1;
  const loaded = () => files.every(file => file in globalThis.__tiloModules);
  const presented = presentedAfter(frames, opened);
  const start = performance.now();
  await nav.dispatch(${JSON.stringify(verb.openApp(app))});
  const end = await presented;
  // The frame the app's own code first drew: presented after its modules.
  let ready = end;
  if (!loaded() || files.some(file => globalThis.__tiloModules[file] > end)) {
    const after = Number(host.dataset.frames);
    ready = await presentedAfter(after, () => loaded() && files.every(file => globalThis.__tiloModules[file] < performance.now()));
  }
  return {ms: end - start, readyMs: ready - start, windows: nav.getSnapshot().entries.length};
})()`;

/** The split-off module files an app's code is in (pkg/split.json), its shared ones first. */
export function appModules(split, app) {
  const name = app.replace(/^so\.tilo\./, '').replaceAll('-', '_');
  const modules = new Map((split?.modules ?? []).map(entry => [entry.module, entry]));
  const own = modules.get(name);
  if (!own) return [];
  // The splitter names a shared chunk by its module or, since it writes
  // chunks of its own, by the chunk's file.
  const file = shared => shared.endsWith('.wasm') ? shared
    : modules.get(shared)?.file ?? `tilo_browser_runtime.${shared}.wasm`;
  return [...(own.shared ?? []).map(file), own.file];
}

const activateExpression = (route, timeoutMs) => `(async () => {
  ${presentedHelper(timeoutMs)}
  const nav = tiloMount.navigation, route = ${JSON.stringify(route)};
  const frames = Number(host.dataset.frames);
  const presented = presentedAfter(frames, () => nav.getSnapshot().active?.tab === route.tab);
  const start = performance.now();
  await nav.dispatch(${JSON.stringify(verb.focus(route))});
  return {ms: (await presented) - start};
})()`;

// Wheel the canvas from inside the page at 120 Hz, as a trackpad reports,
// turning back at the strip's ends, and time every presented frame.
const demoExpression = seconds => `(async () => {
  const runtime = ${RUNTIME};
  const host = tiloMount.canvas.parentElement, canvas = tiloMount.canvas, nav = tiloMount.navigation;
  const rect = canvas.getBoundingClientRect(), x = rect.left + rect.width / 2, y = rect.top + rect.height / 2;
  runtime.configure_frame_metrics(true); runtime.take_frame_metrics();
  const times = [], offsets = new Set();
  const observer = new MutationObserver(() => { if (host.dataset.phase === 'presented') times.push(performance.now()); });
  observer.observe(host, {attributes: true, attributeFilter: ['data-frames']});
  // The pan is read back every 15 wheels (8 times a second), not per wheel:
  // a navigation snapshot is page work the measurement must not add to every
  // frame. Between reads the expected offset is tracked from the deltas sent.
  let direction = 1, sent = 0, offset = nav.getSnapshot().desktopPan?.offset ?? 0;
  const max = nav.getSnapshot().desktopPan?.max ?? 0, step = 6;
  const start = performance.now();
  await new Promise(done => {
    const timer = setInterval(() => {
      if (sent % 15 === 0) { offset = nav.getSnapshot().desktopPan?.offset ?? 0; offsets.add(offset.toFixed(1)); }
      if (offset >= max - step) direction = -1; else if (offset <= step) direction = 1;
      canvas.dispatchEvent(new WheelEvent('wheel', {deltaX: step * direction, deltaY: 0, deltaMode: 0, clientX: x, clientY: y,
        bubbles: true, cancelable: true}));
      offset += step * direction; sent++;
      if (performance.now() - start >= ${seconds * 1000}) { clearInterval(timer); done(); }
    }, 1000 / 120);
  });
  const inputEnd = performance.now();
  await new Promise(done => setTimeout(done, 250));
  observer.disconnect();
  offsets.add((nav.getSnapshot().desktopPan?.offset ?? 0).toFixed(1));
  const metrics = JSON.parse(runtime.take_frame_metrics()); runtime.configure_frame_metrics(false);
  const samples = metrics.flatMap(mount => mount.samples);
  const work = samples.filter(s => s.stage === 'draw_total').map(s => s.ms);
  // Each runtime stage's share, for attribution: count and total ms.
  const stages = {};
  for (const {stage, ms} of samples) { const entry = stages[stage] ??= {n: 0, ms: 0}; entry.n++; entry.ms += ms; }
  const dropped = metrics.reduce((sum, mount) => sum + (mount.dropped ?? 0), 0);
  // Layout passes that re-solved sizes, by the widget that asked: a pan
  // moves laid-out windows, so any entry here is a widget to fix.
  // A runtime that does not report it leaves it null, not empty.
  const measuredBy = metrics.some(mount => mount.measuredBy) ? {} : null;
  for (const mount of metrics) {
    for (const [widget, n] of Object.entries(mount.measuredBy ?? {})) measuredBy[widget] = (measuredBy[widget] ?? 0) + n;
  }
  const during = times.filter(t => t <= inputEnd + 50);
  const requesters = {};
  for (const mount of metrics) for (const [k, n] of Object.entries(mount.requesters ?? {})) requesters[k] = (requesters[k] ?? 0) + n;
  return {requesters, times: during, work, stages, measuredBy, dropped, wheels: sent, seconds: (inputEnd - start) / 1000,
    distinctOffsets: offsets.size, max};
})()`;

// Frames the settled desktop presents per second, and — from the runtime's
// own metrics — what each of them repainted and what asked for it: an idle
// desktop that presents a frame names the widget to fix.
const idleExpression = seconds => `(async () => {
  const runtime = ${RUNTIME};
  runtime.configure_frame_metrics(true); runtime.take_frame_metrics();
  const frames = () => tiloMount.state()?.frames ?? 0, start = performance.now(), from = frames();
  await new Promise(done => setTimeout(done, ${seconds * 1000}));
  const perS = (frames() - from) / ((performance.now() - start) / 1000);
  const metrics = JSON.parse(runtime.take_frame_metrics()); runtime.configure_frame_metrics(false);
  const damage = {}, requesters = {};
  for (const mount of metrics) {
    for (const what of mount.damage ?? []) damage[what] = (damage[what] ?? 0) + 1;
    for (const [k, n] of Object.entries(mount.requesters ?? {})) requesters[k] = (requesters[k] ?? 0) + n;
  }
  return {perS, damage, requesters};
})()`;

// Where the notes window's empty state offers New note, or the block editor
// of the note it already holds, in page pixels. The semantics tree is asked
// for and waited on, as a reader of the window would see it.
const noteTargetExpression = kind => `(async () => {
  const stale = tiloMount.state().semantics;
  tiloMount.requestSemantics();
  for (let i = 0; i < 60 && tiloMount.state().semantics === stale; i++) await new Promise(done => setTimeout(done, 50));
  const canvas = tiloMount.canvas.getBoundingClientRect(), found = {};
  const page = b => b && (Array.isArray(b)
    ? {x0: b[0] + canvas.left, y0: b[1] + canvas.top, x1: b[0] + b[2] + canvas.left, y1: b[1] + b[3] + canvas.top}
    : {x0: b.x0 + canvas.left, y0: b.y0 + canvas.top, x1: b.x1 + canvas.left, y1: b.y1 + canvas.top});
  const walk = (node, rect) => {
    if (!node || typeof node !== 'object') return;
    const access = node.semantics ?? node;
    const bounds = access.bounds ?? node.rect ?? (rect && {x0: rect[0], y0: rect[1], x1: rect[0] + rect[2], y1: rect[1] + rect[3]});
    if (${JSON.stringify(kind)} === 'new-note') {
      if (access.role === 'Button' && access.name === 'New note' && bounds && !found.target) found.target = page(bounds);
    } else if ((node.widget === 'BlockEditor' || /^notes\\.block-text:/.test(node.semantic_key ?? ''))
        && bounds && !found.target) {
      found.target = page(bounds);
    }
    for (const child of node.children ?? []) walk(child, child.rect);
    for (const child of node.semantics?.children ?? []) walk(child);
  };
  walk(tiloMount.state()?.semantics?.root);
  return found.target ?? null;
})()`;

// Every key the person presses in the page, and every frame it presents
// while the typing workload runs; the runtime's per-frame stage samples.
const typingRecorder = `(() => {
  const host = tiloMount.canvas.parentElement;
  const typing = globalThis.__typing = {keys: [], frames: []};
  document.addEventListener('keydown', event => { typing.keys.push(event.timeStamp); }, {capture: true, passive: true});
  typing.observer = new MutationObserver(() => {
    if (host.dataset.phase === 'presented') typing.frames.push(performance.now());
  });
  typing.observer.observe(host, {attributes: true, attributeFilter: ['data-frames']});
  return true;
})()`;
const typingFinish = `(async () => {
  const runtime = ${RUNTIME};
  const typing = globalThis.__typing ?? {keys: [], frames: []};
  typing.observer?.disconnect();
  const metrics = JSON.parse(runtime.take_frame_metrics()); runtime.configure_frame_metrics(false);
  const latency = typing.keys.map(at => {
    const frame = typing.frames.find(time => time > at);
    return frame === undefined ? null : frame - at;
  });
  const samples = metrics.flatMap(mount => mount.samples);
  const stages = {};
  for (const {stage, ms} of samples) { const entry = stages[stage] ??= {n: 0, ms: 0}; entry.n++; entry.ms += ms; }
  const damages = {}, requesters = {};
  for (const mount of metrics) {
    for (const what of mount.damage ?? []) damages[what] = (damages[what] ?? 0) + 1;
  }
  for (const mount of metrics) for (const [k, n] of Object.entries(mount.requesters ?? {})) requesters[k] = (requesters[k] ?? 0) + n;
  // What the note now holds, read from the semantics tree, proves the keys
  // reached the document and not only the page.
  const values = [];
  const walk = node => {
    if (!node || typeof node !== 'object') return;
    if (/^notes\\.block-text:/.test(node.semantic_key ?? '') && node.value) values.push(node.value);
    for (const child of node.children ?? []) walk(child);
    for (const child of node.semantics?.children ?? []) walk(child);
  };
  tiloMount.requestSemantics();
  for (let i = 0; i < 40; i++) {
    walk(tiloMount.state()?.semantics?.root);
    if (values.length) break;
    await new Promise(done => setTimeout(done, 50));
  }
  return {keys: typing.keys.length, latency, work: samples.filter(s => s.stage === 'draw_total').map(s => s.ms),
    stages, damages, requesters, text: values.join(' ').slice(0, 200)};
})()`;

// A key event as Chrome sends it for a printable character.
export function keyEvent(character) {
  if (character === ' ') return {key: ' ', code: 'Space', windowsVirtualKeyCode: 32};
  const upper = character.toUpperCase();
  return {key: character, code: `Key${upper}`, windowsVirtualKeyCode: upper.charCodeAt(0)};
}

// --- one round ----------------------------------------------------------------

async function freshPage(browser, {cpuThrottle, frameTimeout, viewport}) {
  const {result: {browserContextId}} = await browser.send('Target.createBrowserContext', {disposeOnDetach: true});
  const {result: {targetId}} = await browser.send('Target.createTarget', {url: 'about:blank', browserContextId});
  const {result: {sessionId}} = await browser.send('Target.attachToTarget', {targetId, flatten: true});
  const errors = [];
  const off = browser.on(message => {
    if (message.sessionId !== sessionId) return;
    if (message.method === 'Runtime.exceptionThrown') {
      const details = message.params.exceptionDetails;
      errors.push((details.exception?.description ?? details.text).slice(0, 400));
    }
  });
  const send = (method, params = {}) => browser.send(method, params, sessionId);
  const evaluate = async expression => {
    // The page's own timeout does not cover a page blocked by its renderer.
    const limit = (2 * frameTimeout + 60) * 1000;
    const reply = await within(send('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true, timeout: limit}),
      limit + 30_000, 'the page');
    if (reply.error) throw Error(reply.error.message);
    if (reply.result.exceptionDetails) {
      const details = reply.result.exceptionDetails;
      throw Error(details.exception?.description ?? details.text);
    }
    return reply.result.result.value;
  };
  await send('Runtime.enable');
  await send('Page.enable');
  await send('Emulation.setDeviceMetricsOverride', {...viewport, mobile: false});
  await send('Emulation.setFocusEmulationEnabled', {enabled: true});
  if (cpuThrottle > 1) await send('Emulation.setCPUThrottlingRate', {rate: cpuThrottle});
  await send('Page.addScriptToEvaluateOnNewDocument', {source: PHASE_PROBE});
  return {send, evaluate, errors, close: async () => { off(); await browser.send('Target.closeTarget', {targetId}); }};
}

/** What Chrome's GPU process saw, so a renderer failure says why: devices, feature status, Vulkan. */
async function gpuDiagnostics(browser) {
  const reply = await browser.send('SystemInfo.getInfo').catch(() => null);
  const gpu = reply?.result?.gpu;
  if (!gpu) return 'unavailable';
  const aux = Object.entries(gpu.auxAttributes ?? {}).filter(([key]) => /vulkan|webgpu|dawn|skia|angle/i.test(key));
  return JSON.stringify({devices: gpu.devices, featureStatus: gpu.featureStatus, aux: Object.fromEntries(aux)}).slice(0, 4000);
}

/** The demo workload on the strip of `windows`, after one unmeasured pass. */
async function measureDemo(page, windows, options, index) {
  await page.evaluate(`tiloMount.navigation.dispatch(${JSON.stringify(verb.focus(windows[0].route))})`);
  await sleep(1000);
  // Warm up with the same workload, unmeasured: the first pan frames over
  // each window compile the renderer's pipelines for it. On a GPU that is
  // milliseconds; on SwiftShader one frame took a minute and left a
  // five-second demo with a single frame.
  await page.evaluate(demoExpression(options.demoSeconds));
  await sleep(500);
  const demo = await page.evaluate(demoExpression(options.demoSeconds));
  if (demo.max <= 0) throw new Invalid(`round ${index}: the demo strip does not overflow the viewport; nothing to pan`);
  // Pan offsets are read back eight times a second and once at the end.
  // At least two intervals, and a strip that moved. A software rasterizer
  // can block the page for seconds at a time, so neither is a rate.
  if (demo.distinctOffsets < 2 || demo.times.length < 3) {
    throw new Invalid(`round ${index}: the demo presented ${demo.times.length} frames over ${demo.distinctOffsets} pan offsets`);
  }
  if (!demo.work.length || demo.dropped) {
    throw new Invalid(`round ${index}: frame metrics incomplete (${demo.work.length} samples, ${demo.dropped} dropped)`);
  }
  return {requesters: demo.requesters, intervals: intervals(demo.times), work: demo.work, stages: demo.stages, frames: demo.times.length,
    wheels: demo.wheels, seconds: demo.seconds, fps: demo.times.length / demo.seconds, distinctOffsets: demo.distinctOffsets,
    measuredBy: demo.measuredBy};
}

// How fast a person typing quickly sends keys: 20 a second, a fast typist's
// burst, so a frame that arrives late shows up in the p95.
const TYPING_INTERVAL_MS = 50;
const TYPING_TEXT = 'the quick brown fox jumps over the lazy dog';

// A note typed into at TYPING_INTERVAL_MS, each key timed from the page's own
// key event to the first presented frame after it. The editor is reached the
// way a person reaches it: New note, then a press in the note's body.
async function measureTyping(page, windows, options, index) {
  const notes = windows.find(entry => entry.app === 'so.tilo.notes') ?? windows[0];
  await page.evaluate(`tiloMount.navigation.dispatch(${JSON.stringify(verb.focus(notes.route))})`);
  await sleep(1000);
  const click = async (rect, dx = 0.5, dy = 0.5) => {
    const x = Math.round(rect.x0 + (rect.x1 - rect.x0) * dx), y = Math.round(rect.y0 + (rect.y1 - rect.y0) * dy);
    for (const [type, buttons] of [['mousePressed', 1], ['mouseReleased', 0]]) {
      await page.send('Input.dispatchMouseEvent', {type, x, y, button: 'left', buttons, clickCount: 1});
    }
  };
  const fresh = await page.evaluate(noteTargetExpression('new-note'));
  if (fresh) {
    await click(fresh);
    await sleep(1500);
  }
  const editor = await page.evaluate(noteTargetExpression('editor'));
  if (!editor) throw new Invalid(`round ${index}: the notes window shows no editor to type into`);
  // A press in the body, on its first line just past the text's left edge:
  // the caret goes where a person's would.
  await click({...editor, x0: editor.x0 + 40, y0: editor.y0 + 12}, 0, 0);
  await sleep(500);
  await page.evaluate(`(async () => { const runtime = ${RUNTIME}; runtime.configure_frame_metrics(true); runtime.take_frame_metrics(); return true; })()`);
  await page.evaluate(typingRecorder);
  const typed = TYPING_TEXT.repeat(Math.ceil(options.typingKeys / TYPING_TEXT.length)).slice(0, options.typingKeys);
  for (const character of typed) {
    const event = keyEvent(character);
    await page.send('Input.dispatchKeyEvent', {type: 'keyDown', text: character, unmodifiedText: character, ...event});
    await page.send('Input.dispatchKeyEvent', {type: 'keyUp', ...event});
    await sleep(TYPING_INTERVAL_MS);
  }
  await sleep(300);
  const typing = await page.evaluate(typingFinish);
  if (typing.keys !== typed.length) {
    throw new Invalid(`round ${index}: the page saw ${typing.keys} of the ${typed.length} keys typed`);
  }
  const missing = typing.latency.filter(value => value === null).length;
  if (missing) throw new Invalid(`round ${index}: ${missing} of ${typed.length} keys presented no frame after them`);
  const text = (typing.text ?? '').replace(/\s+/g, ' ').trim();
  if (!text.includes(typed.replace(/\s+/g, ' ').trim())) {
    throw new Invalid(`round ${index}: the note holds "${text}", not what was typed ("${typed}")`);
  }
  return {keys: typing.keys, latencies: typing.latency, work: typing.work, stages: typing.stages, damage: typing.damages,
    requesters: typing.requesters, text};
}

async function round(browser, url, options, index) {
  const page = await freshPage(browser, options);
  const frameTimeoutMs = options.frameTimeout * 1000;
  try {
    await page.send('Page.navigate', {url: `${url}/`});
    // SwiftShader compiles the renderer's pipelines on the CPU before the
    // first frame: 30 s on a fast laptop, so allow three minutes.
    const deadline = Date.now() + 180_000;
    while (!(await page.evaluate('Boolean(globalThis.tiloMount?.navigation)').catch(() => false))) {
      if (Date.now() > deadline || page.errors.some(e => /panic/i.test(e))) {
        const status = await page.evaluate("document.getElementById('status')?.textContent ?? ''").catch(() => '');
        throw new Invalid(`round ${index}: the desktop never presented: ${status} ${page.errors.slice(0, 2).join(' | ')}; `
          + `Chrome's GPU: ${await gpuDiagnostics(browser)}`);
      }
      await sleep(100);
    }
    const load = await page.evaluate('globalThis.__tiloPhases?.presented ?? NaN');
    const adapter = await page.evaluate(`(async () => { const a = await navigator.gpu?.requestAdapter();
      if (!a) return null; const i = a.info ?? {}; return {vendor: i.vendor, architecture: i.architecture, device: i.device, description: i.description}; })()`);
    // Before measuring anything: a run on another renderer is not this one.
    if (classifyAdapter(adapter) !== options.renderer) {
      throw new Invalid(`round ${index}: WebGPU adapter is ${classifyAdapter(adapter)}, not ${options.renderer}: `
        + `${JSON.stringify(adapter)}; Chrome's GPU: ${await gpuDiagnostics(browser)}`);
    }
    const available = (await page.evaluate('tiloMount.navigation.getSnapshot()')).applications.map(app => app.id);
    const apps = DEMO_APPS.filter(app => available.includes(app));
    if (apps.length < 4) throw new Invalid(`round ${index}: the bundle has only ${apps.join(', ')} of the demo apps`);
    await sleep(1000);

    const opens = [];
    for (const app of apps) {
      const result = await page.evaluate(openExpression(app, appModules(options.split, app), frameTimeoutMs))
        .catch(error => {
          throw new Invalid(`round ${index}: opening ${app} failed: ${error.message}`
            + `; the page said: ${page.errors.slice(0, 2).join(' | ') || 'nothing'}`);
        });
      if (!Number.isFinite(result.ms)) throw new Invalid(`round ${index}: ${app} never presented its window`);
      if (!Number.isFinite(result.readyMs)) throw new Invalid(`round ${index}: ${app}'s code never presented a frame`);
      opens.push({app, ms: result.ms, readyMs: result.readyMs});
      await sleep(1500);
    }

    const snapshot = await page.evaluate('tiloMount.navigation.getSnapshot()');
    const windows = snapshot.entries.filter(e => e.route.workspace === snapshot.active.workspace
      && e.route.strip === snapshot.active.strip);
    if (windows.length < 2) throw new Invalid(`round ${index}: the demo strip holds ${windows.length} windows`);
    const activations = [];
    for (let i = 0; i < options.activations; i++) {
      const active = (await page.evaluate('tiloMount.navigation.getSnapshot()')).active;
      const others = windows.filter(w => w.route.tab !== active?.tab);
      const target = others[i % others.length].route;
      const {ms} = await page.evaluate(activateExpression(target, frameTimeoutMs));
      if (!Number.isFinite(ms)) throw new Invalid(`round ${index}: selecting a window presented no frame`);
      activations.push(ms);
      await sleep(400);
    }

    const demo = options.demoSeconds > 0 ? await measureDemo(page, windows, options, index) : NO_DEMO;

    await sleep(2000);
    const idle = await page.evaluate(idleExpression(options.idleSeconds));
    // Typing comes last: a field left focused blinks its caret, which is a
    // frame a desktop no one is touching is right to present, and the idle
    // workload is about the frames nothing asks for.
    const typing = options.typingKeys > 0 ? await measureTyping(page, windows, options, index) : NO_TYPING;
    const resources = await page.evaluate(RESOURCES);
    if (page.errors.length) throw new Invalid(`round ${index}: page exceptions: ${page.errors.slice(0, 3).join(' | ')}`);
    return {round: index, loadMs: load, adapter, apps, opens, activations, typing, demo, idle, resources};
  } finally {
    await page.close().catch(() => {});
  }
}

// --- the run --------------------------------------------------------------------

export function summarize(rounds, load = {}) {
  return {
    load_ms: stats(rounds.map(r => r.loadMs)),
    transfer_bytes: stats(load.transferBytes ?? []),
    request_count: stats(load.requests ?? []),
    open_ms: stats(rounds.flatMap(r => r.opens.map(o => o.ms))),
    open_ready_ms: stats(rounds.flatMap(r => r.opens.map(o => o.readyMs))),
    activate_ms: stats(rounds.flatMap(r => r.activations)),
    demo_interval_ms: stats(rounds.flatMap(r => r.demo.intervals)),
    demo_work_ms: stats(rounds.flatMap(r => r.demo.work)),
    demo_fps: stats(rounds.map(r => r.demo.fps)),
    idle_per_s: stats(rounds.map(r => r.idle.perS)),
    typing_ms: stats(rounds.flatMap(r => r.typing.latencies)),
    typing_work_ms: stats(rounds.flatMap(r => r.typing.work)),
  };
}

/** Where the demo's frame time went: each runtime stage's ms per presented frame, largest first. */
export function attribute(rounds) {
  const frames = rounds.reduce((sum, r) => sum + r.demo.frames, 0);
  const totals = {};
  for (const r of rounds) {
    for (const [stage, {n, ms}] of Object.entries(r.demo.stages)) {
      const entry = totals[stage] ??= {n: 0, ms: 0};
      entry.n += n; entry.ms += ms;
    }
  }
  const seconds = rounds.reduce((sum, r) => sum + r.demo.seconds, 0);
  return Object.entries(totals).sort((a, b) => b[1].ms - a[1].ms).map(([stage, {n, ms}]) => ({stage, n,
    msPerFrame: frames ? Math.round(ms / frames * 1000) / 1000 : NaN,
    busyPercent: seconds ? Math.round(ms / (seconds * 1000) * 1000) / 10 : NaN}));
}

/** The typing workload's own frame time: its stages per key, and what its frames repainted. */
export function attributeTyping(rounds) {
  const keys = rounds.reduce((sum, r) => sum + r.typing.keys, 0);
  const totals = {}, damages = {};
  for (const r of rounds) {
    for (const [stage, {n, ms}] of Object.entries(r.typing.stages)) {
      const entry = totals[stage] ??= {n: 0, ms: 0};
      entry.n += n; entry.ms += ms;
    }
    for (const [what, n] of Object.entries(r.typing.damage)) damages[what] = (damages[what] ?? 0) + n;
  }
  return {keys,
    stages: Object.entries(totals).sort((a, b) => b[1].ms - a[1].ms).map(([stage, {n, ms}]) => ({stage, n,
      msPerKey: keys ? Math.round(ms / keys * 1000) / 1000 : NaN})),
    damage: Object.entries(damages).sort((a, b) => b[1] - a[1]).slice(0, 12).map(([what, n]) => ({what, n}))};
}

/** What an idle desktop's presented frames repainted, and what asked for them. */
export function attributeIdle(rounds) {
  const damage = {}, requesters = {};
  for (const r of rounds) {
    for (const [what, n] of Object.entries(r.idle.damage)) damage[what] = (damage[what] ?? 0) + n;
    for (const [k, n] of Object.entries(r.idle.requesters)) requesters[k] = (requesters[k] ?? 0) + n;
  }
  const top = counts => Object.entries(counts).sort((a, b) => b[1] - a[1]).slice(0, 12)
    .map(([what, n]) => ({what, n}));
  return {damage: top(damage), requesters: top(requesters)};
}

export function formatSummary(report) {
  const s = report.summary;
  const row = (name, stat) => `  ${name.padEnd(18)} n ${String(stat.n).padStart(4)}  p50 ${stat.p50}  p95 ${stat.p95}  p99 ${stat.p99}  max ${stat.max}`;
  const lines = [
    `browser qualification: bundle ${report.bundle.revision.slice(0, 12)} (${report.bundle.profile}, digest ${report.bundle.digest.slice(0, 12)}, runtime ${report.bundle.runtimeSha256.slice(0, 12)})`,
    `  ${report.browser.product} | adapter ${report.renderer}: ${Object.values(report.adapter ?? {}).filter(Boolean).join(' ')} | ${report.viewport.width}x${report.viewport.height}@${report.viewport.deviceScaleFactor}`,
    `  host ${report.host.cpu} x${report.host.cpus}, load ${report.host.before.load1} before, ${report.host.after.load1} after`
      + `${report.workload.cpuThrottle > 1 ? `, CPU throttled ${report.workload.cpuThrottle}x` : ''}`,
    row('load (ms)', s.load_ms), row('load bytes (B)', s.transfer_bytes), row('load requests', s.request_count),
    row('open (ms)', s.open_ms), row('open ready (ms)', s.open_ready_ms),
    row('activate (ms)', s.activate_ms),
    row('demo interval (ms)', s.demo_interval_ms), row('demo work (ms)', s.demo_work_ms), row('demo fps', s.demo_fps),
    row('idle (frames/s)', s.idle_per_s),
    ...(report.workload.typingKeys > 0
      ? [row('typing (ms)', s.typing_ms), row('typing work (ms)', s.typing_work_ms)]
      : []),
    report.attribution.length ? `  demo stages (ms per presented frame, share of wall time): ${report.attribution.slice(0, 8)
      .map(a => `${a.stage} ${a.msPerFrame} (${a.busyPercent}%)`).join(', ')}` : '  demo not run',
  ];
  if (report.typing && report.typing.stages.length) {
    lines.push(`  typing stages (ms per key): ${report.typing.stages.slice(0, 8)
      .map(a => `${a.stage} ${a.msPerKey}`).join(', ')}`);
    lines.push(`  typing frames repainted: ${report.typing.damage.slice(0, 6)
      .map(d => `${d.what} (${d.n})`).join(', ')}`);
  }
  if (report.idle && (report.idle.damage.length || report.idle.requesters.length)) {
    lines.push(`  idle frames repainted: ${(report.idle.damage.slice(0, 5).map(d => `${d.what} (${d.n})`).join(', ')) || 'none'}`
      + `; asked for by ${report.idle.requesters.slice(0, 5).map(d => `${d.what} (${d.n})`).join(', ') || 'nothing'}`);
  }
  if (report.rounds.some(r => r.demo.measuredBy)) {
    const measuredBy = {};
    for (const r of report.rounds) {
      for (const [widget, n] of Object.entries(r.demo.measuredBy ?? {})) measuredBy[widget] = (measuredBy[widget] ?? 0) + n;
    }
    const askers = Object.entries(measuredBy).sort((a, b) => b[1] - a[1]);
    lines.push(askers.length
      ? `  demo layout re-measured for: ${askers.slice(0, 5).map(([widget, n]) => `${widget} (${n})`).join(', ')}`
      : '  demo layout re-measured for: nothing (pans only moved laid-out windows)');
  }
  for (const goal of report.goals) {
    const extra = goal.fps ? ` (${goal.fps} fps)` : goal.capacityFps ? ` (capacity ${goal.capacityFps} fps)` : '';
    lines.push(`  ${goal.met ? 'MET ' : 'MISS'} ${goal.name}: ${goal.measured} vs ${Math.round(goal.target * 100) / 100}${extra}${goal.met ? '' : `, gap ${goal.gap}`}`);
  }
  return lines.join('\n');
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const before = hostState();
  checkHost(before, options.maxLoad, 'before the run');
  const bundle = verifyBundle(options.bundle, options);
  const split = join(options.bundle, 'pkg/split.json');
  options.split = existsSync(split) ? JSON.parse(readFileSync(split, 'utf8')) : null;
  const listed = Object.keys(JSON.parse(readFileSync(join(options.bundle, 'build.json'), 'utf8')).files);
  const server = await serve(options.bundle);
  const flags = chromeFlags(options.renderer, process.getuid?.());
  const browser = await launch({args: flags});
  try {
    const version = (await browser.send('Browser.getVersion')).result;
    const rounds = [];
    for (let i = 1; i <= options.rounds; i++) {
      rounds.push(await round(browser, server.url, options, i));
      console.error(`round ${i}/${options.rounds} measured`);
    }
    const renderers = new Set(rounds.map(r => classifyAdapter(r.adapter)));
    if (renderers.size !== 1 || !renderers.has(options.renderer)) {
      throw new Invalid(`WebGPU adapter is ${[...renderers].join(', ')}, not ${options.renderer}: ${JSON.stringify(rounds[0].adapter)}`);
    }
    const origin = new URL(server.url).origin;
    const unlisted = [...new Set(rounds.flatMap(r => unlistedResources(r.resources, origin, listed)))];
    if (unlisted.length) throw new Invalid(`the page loaded files the bundle does not list: ${unlisted.slice(0, 5).join(', ')}`);
    const loadedRuntime = rounds.every(r => r.resources.some(entry => entry.status === 200 && entry.name.endsWith('/pkg/tilo_browser_runtime_bg.wasm')));
    if (!loadedRuntime) throw new Invalid('the page never loaded the bundle\'s runtime module');
    const loadBytes = rounds.map(r => transfer(r.resources, origin, r.loadMs));
    if (loadBytes.some(t => t.bytes <= 0 || t.requests <= 0)) {
      throw new Invalid('the page reported no resource entries before its desktop presented');
    }
    const after = hostState();
    checkHost(after, options.maxLoad, 'after the run');
    const summary = summarize(rounds, {transferBytes: loadBytes.map(t => t.bytes), requests: loadBytes.map(t => t.requests)});
    const host = {...before, before, after, maxLoad: options.maxLoad ?? null, platform: `${os.platform()} ${os.release()} ${os.arch()}`};
    const report = {schema: 1, measuredAt: new Date().toISOString(), bundle, renderer: options.renderer,
      adapter: rounds[0].adapter, browser: {product: version.product, userAgent: version.userAgent},
      viewport: options.viewport, host,
      load: {bytes: loadBytes.map(t => t.bytes), requests: loadBytes.map(t => t.requests),
        crossOrigin: loadBytes.map(t => t.crossOrigin)},
      workload: {rounds: options.rounds, activations: options.activations, demoSeconds: options.demoSeconds,
        idleSeconds: options.idleSeconds, typingKeys: options.typingKeys, typingIntervalMs: TYPING_INTERVAL_MS,
        cpuThrottle: options.cpuThrottle, apps: rounds[0].apps, inputHz: 120, chromeFlags: flags},
      summary, goals: judgeGoals(summary), attribution: attribute(rounds), typing: attributeTyping(rounds),
      idle: attributeIdle(rounds),
      samples: {
        load_ms: rounds.map(r => r.loadMs), open_ms: rounds.flatMap(r => r.opens.map(o => o.ms)),
        open_ready_ms: rounds.flatMap(r => r.opens.map(o => o.readyMs)),
        activate_ms: rounds.flatMap(r => r.activations), demo_interval_ms: rounds.flatMap(r => r.demo.intervals),
        demo_work_ms: rounds.flatMap(r => r.demo.work), idle_per_s: rounds.map(r => r.idle.perS),
        typing_ms: rounds.flatMap(r => r.typing.latencies), typing_work_ms: rounds.flatMap(r => r.typing.work),
        transfer_bytes: loadBytes.map(t => t.bytes), request_count: loadBytes.map(t => t.requests),
      },
      rounds: rounds.map(({resources, ...r}) => ({...r, demo: {frames: r.demo.frames, wheels: r.demo.wheels,
        seconds: r.demo.seconds, fps: r.demo.fps, distinctOffsets: r.demo.distinctOffsets, stages: r.demo.stages,
        measuredBy: r.demo.measuredBy, requesters: r.demo.requesters}})),
    };
    writeFileSync(options.out, JSON.stringify(report, null, 1) + '\n');
    console.log(formatSummary(report));
    if (options.enforceGoals && report.goals.some(goal => !goal.met)) process.exitCode = 1;
  } finally {
    await browser.close().catch(error => console.error(String(error)));
    await server.close();
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch(error => {
    console.error(error instanceof Invalid ? `invalid run: ${error.message}` : error.stack ?? String(error));
    process.exit(2);
  });
}

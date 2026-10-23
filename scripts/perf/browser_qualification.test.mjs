// Tests of the browser runtime qualification's checks and arithmetic
// (scripts/perf/browser_qualification.mjs); the measurement itself needs a
// built bundle and Chrome, which t's perf-browser check has.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdirSync, mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import test from 'node:test';
import {
  GOALS, Invalid, appModules, chromeFlags, classifyAdapter, parseViewport, within, intervals, judgeGoals, parseArgs, percentile, stats, summarize, unlistedResources,
  verifyBundle,
} from './browser_qualification.mjs';

const sha = bytes => createHash('sha256').update(bytes).digest('hex');

function bundle(overrides = {}, {tamper} = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'browser-qualification-'));
  mkdirSync(join(dir, 'pkg'));
  const files = {'index.html': '<!doctype html>', 'pkg/tilo_browser_runtime_bg.wasm': 'wasm', 'pkg/tilo_browser_runtime.js': 'js'};
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text);
  const build = {revision: 'c'.repeat(40), dirty: false, profile: 'release', bundle: 'desktop', cargoProfile: 'wasm-split',
    files: Object.fromEntries(Object.entries(files).map(([name, text]) => [name, sha(text)])), ...overrides};
  writeFileSync(join(dir, 'build.json'), JSON.stringify(build));
  if (tamper) writeFileSync(join(dir, tamper), 'changed');
  return dir;
}

test('a clean release bundle verifies, with a digest of its listed bytes', () => {
  const dir = bundle();
  try {
    const identity = verifyBundle(dir, {expectRevision: 'c'.repeat(40)});
    assert.equal(identity.revision, 'c'.repeat(40));
    assert.equal(identity.files, 3);
    assert.equal(identity.runtimeSha256, sha('wasm'));
    assert.match(identity.digest, /^[0-9a-f]{64}$/);
    assert.equal(verifyBundle(dir).digest, identity.digest);
  } finally { rmSync(dir, {recursive: true}); }
});

test('a bundle that is not what its build.json says is refused', () => {
  for (const [overrides, options, reason] of [
    [{}, {tamper: 'pkg/tilo_browser_runtime_bg.wasm'}, /differ from build.json/],
    [{profile: 'dev'}, {}, /not release/],
    [{dirty: true}, {}, /dirty tree/],
    [{bundle: 'runtime'}, {}, /not desktop/],
  ]) {
    const dir = bundle(overrides, options);
    try { assert.throws(() => verifyBundle(dir), error => error instanceof Invalid && reason.test(error.message)); } finally {
      rmSync(dir, {recursive: true});
    }
  }
  const dir = bundle();
  try {
    assert.throws(() => verifyBundle(dir, {expectRevision: 'd'.repeat(40)}), /not d{40}/);
    assert.doesNotThrow(() => verifyBundle(bundle({profile: 'dev', dirty: true}), {allowDev: true}));
  } finally { rmSync(dir, {recursive: true}); }
});

test('a missing build.json is not a bundle', () => {
  const dir = mkdtempSync(join(tmpdir(), 'browser-qualification-'));
  try { assert.throws(() => verifyBundle(dir), /no build.json/); } finally { rmSync(dir, {recursive: true}); }
});

test('only same-origin files received that the bundle does not list are unlisted', () => {
  const origin = 'http://127.0.0.1:4000';
  const loaded = [`${origin}/`, `${origin}/pkg/tilo_browser_runtime_bg.wasm`, `${origin}/pkg/app-notes.wasm`,
    'https://tiles.example/1.png'].map(name => ({name, status: 200}))
    .concat([{name: `${origin}/favicon.ico`, status: 404}]);
  assert.deepEqual(unlistedResources(loaded, origin, ['index.html', 'pkg/tilo_browser_runtime_bg.wasm']), ['pkg/app-notes.wasm']);
});

test("an app's code is its split-off module, after the shared modules it needs", () => {
  const split = {modules: [
    {module: 'shared0', file: 'tilo_browser_runtime.shared0.wasm', shared: []},
    {module: 'word_processor', file: 'tilo_browser_runtime.word_processor.wasm', shared: ['shared0']},
    {module: 'notes', file: 'tilo_browser_runtime.notes.wasm', shared: []},
  ]};
  assert.deepEqual(appModules(split, 'so.tilo.word-processor'),
    ['tilo_browser_runtime.shared0.wasm', 'tilo_browser_runtime.word_processor.wasm']);
  assert.deepEqual(appModules(split, 'so.tilo.notes'), ['tilo_browser_runtime.notes.wasm']);
  assert.deepEqual(appModules(split, 'so.tilo.clocks'), [], 'an app linked into the start module has no module to wait for');
  assert.deepEqual(appModules(null, 'so.tilo.notes'), []);
  const chunked = {modules: [{module: 'notes', file: 'tilo_browser_runtime.notes.wasm',
    shared: ['tilo_browser_runtime.shared0.wasm', 'tilo_browser_runtime.shared2.wasm']}]};
  assert.deepEqual(appModules(chunked, 'so.tilo.notes'), ['tilo_browser_runtime.shared0.wasm',
    'tilo_browser_runtime.shared2.wasm', 'tilo_browser_runtime.notes.wasm'], 'shared chunks listed by file');
});

test("Chrome's flags follow the renderer and whether the run is root's", () => {
  assert.deepEqual(chromeFlags('gpu', 501), ['--disable-frame-rate-limit', '--disable-gpu-vsync']);
  const ci = chromeFlags('swiftshader', 0);
  assert.ok(ci.includes('--use-vulkan=swiftshader') && ci.includes('--use-angle=swiftshader'));
  assert.ok(ci.includes('--no-sandbox'), 'Chrome refuses to run as root with its sandbox');
  assert.ok(!chromeFlags('swiftshader', 1000).includes('--no-sandbox'));
  assert.ok(!chromeFlags('gpu', undefined).includes('--no-sandbox'));
  assert.throws(() => parseArgs(['bundle', '--out', 'r.json', '--renderer', 'lavapipe']), /--renderer/,
    "Chrome never offers a system CPU driver to WebGPU");
});

test('the adapter names its renderer, and an empty info is unknown', () => {
  assert.equal(classifyAdapter({vendor: 'google', architecture: 'swiftshader', device: '', description: ''}), 'swiftshader');
  assert.equal(classifyAdapter({vendor: 'mesa', description: 'llvmpipe (LLVM 19, 256 bits)'}), 'lavapipe');
  assert.equal(classifyAdapter({vendor: 'apple', architecture: 'metal-3'}), 'gpu');
  assert.equal(classifyAdapter({vendor: '', architecture: ''}), 'unknown');
  assert.equal(classifyAdapter(null), 'unknown');
});

test('percentiles are nearest-rank, as tools/perf/record.py computes them', () => {
  const sorted = Array.from({length: 100}, (_, i) => i + 1);
  assert.equal(percentile(sorted, 50), 50);
  assert.equal(percentile(sorted, 99), 99);
  assert.equal(percentile([7], 99), 7);
  assert.ok(Number.isNaN(percentile([], 50)));
  assert.deepEqual(stats([3, 1, 2, NaN]), {n: 3, p50: 2, p95: 3, p99: 3, max: 3});
  assert.deepEqual(intervals([0, 8, 20, 21]), [8, 12, 1]);
});

function round(scale) {
  return {loadMs: 900 * scale, opens: [{app: 'so.tilo.notes', ms: 120 * scale, readyMs: 160 * scale}], activations: [20 * scale, 30 * scale],
    demo: {intervals: [8 * scale, 9 * scale], work: [4 * scale, 5 * scale], fps: 110 / scale}, idlePerS: 0.5};
}

test('goals report the value, the verdict and the remaining gap', () => {
  const met = judgeGoals(summarize([round(1)]));
  assert.ok(met.find(g => g.name.startsWith('demo minimum')).met);
  const goal = met.find(g => g.name === 'demo goal 120 fps (p50 frame interval, ms)');
  assert.equal(goal.measured, 8);
  assert.equal(goal.fps, 125);
  assert.ok(goal.met);
  const missed = judgeGoals(summarize([round(3)]));
  const interval = missed.find(g => g.name === 'demo goal 120 fps (p50 frame interval, ms)');
  assert.equal(interval.met, false);
  assert.equal(interval.gap, Math.round((24 - GOALS.demoGoalIntervalMs) * 1000) / 1000);
  assert.equal(missed.find(g => g.name === 'activation (p99, ms)').met, false);
  assert.ok(missed.find(g => g.name.startsWith('demo minimum')).met);
});

test('a viewport is WIDTHxHEIGHT at DPR 1, FHD unless given', () => {
  assert.deepEqual(parseViewport('960x540'), {width: 960, height: 540, deviceScaleFactor: 1});
  assert.throws(() => parseViewport('960'), /WIDTHxHEIGHT/);
  assert.deepEqual(parseArgs(['bundle', '--out', 'r.json', '--renderer', 'gpu']).viewport,
    {width: 1920, height: 1080, deviceScaleFactor: 1});
  assert.equal(parseArgs(['bundle', '--out', 'r.json', '--renderer', 'gpu', '--viewport', '1280x720']).viewport.width, 1280);
});

test('a step that never answers fails the run instead of hanging it', async () => {
  assert.equal(await within(Promise.resolve(7), 1000, 'quick'), 7);
  await assert.rejects(within(new Promise(() => {}), 20, 'the page'),
    error => error instanceof Invalid && /the page did not answer/.test(error.message));
});

test('a run without the demo judges no demo goal', () => {
  const noDemo = {...round(1), demo: {intervals: [], work: [], stages: {}, frames: 0, wheels: 0, seconds: 0, fps: NaN}};
  const goals = judgeGoals(summarize([noDemo]));
  assert.ok(goals.length > 0);
  assert.ok(goals.every(g => !g.name.startsWith('demo')), 'no demo goal without a demo');
  assert.ok(goals.find(g => g.name === 'activation (p99, ms)').met);
  assert.equal(parseArgs(['bundle', '--out', 'r.json', '--renderer', 'swiftshader', '--demo-seconds', '0']).demoSeconds, 0);
  assert.throws(() => parseArgs(['bundle', '--out', 'r.json', '--renderer', 'gpu', '--rounds', '0']), /positive/);
});

test('arguments need a bundle, a report path and a known renderer', () => {
  const parsed = parseArgs(['bundle', '--out', 'r.json', '--renderer', 'swiftshader', '--rounds', '2', '--max-load', '0.5']);
  assert.equal(parsed.rounds, 2);
  assert.equal(parsed.maxLoad, 0.5);
  assert.equal(parsed.activations, 20);
  assert.equal(parsed.frameTimeout, 120, 'a software rasterizer may take long over a first pipeline');
  assert.equal(parseArgs(['bundle', '--out', 'r.json', '--renderer', 'gpu']).frameTimeout, 10);
  assert.equal(parseArgs(['bundle', '--out', 'r.json', '--renderer', 'gpu', '--frame-timeout', '30']).frameTimeout, 30);
  assert.throws(() => parseArgs(['bundle', '--out', 'r.json']), /usage/);
  assert.throws(() => parseArgs(['bundle', '--out', 'r.json', '--renderer', 'opengl']), /--renderer/);
  assert.throws(() => parseArgs(['bundle', '--out', 'r.json', '--renderer', 'gpu', '--rounds', '1.5']), /whole number/);
  assert.throws(() => parseArgs(['bundle', '--out', 'r.json', '--renderer', 'gpu', '--frames']), /unknown option/);
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';

// `host.mjs` imports the wasm bindings, which a plain Node run has no build
// of: only that import is replaced, so `prefetchApplication` is the real one.
async function load() {
  let source = (await readFile(new URL('./host.mjs', import.meta.url), 'utf8'))
    .replaceAll('import.meta.url', JSON.stringify(new URL('./host.mjs', import.meta.url).href));
  source = source
    .replace("import init,* as runtime from './pkg/tilo_browser_runtime.js';", 'const init=async()=>{};const runtime={};')
    .replace(/from '(\.\/[^']+)'/g, (_all, path) => `from '${new URL(path, import.meta.url).href}'`);
  // A fresh URL per call: the module remembers the listing it fetched.
  return import(`data:text/javascript;base64,${Buffer.from(`${source}\n// ${Math.random()}`).toString('base64')}`);
}

/** `prefetchApplication` with a fake network and loader, so the listing and
 * the module's compilation are observed, not fetched. */
function harness({listed = [{id: 'so.tilo.chat', module: 'chat.js'}], listingOk = true, failListing = false} = {}) {
  const steps = [];
  const fetch = async url => {
    steps.push(`fetch ${String(url).split('/').pop()}`);
    if (failListing) throw new Error('offline');
    return {ok: listingOk, json: async () => ({applications: listed})};
  };
  const prefetched = [];
  const load = async () => ({prefetch: module => prefetched.push(module)});
  return {steps, prefetched, options: {fetch, load}};
}

const settle = async () => { for (let i = 0; i < 8; i += 1) await Promise.resolve(); };

test("an app's module is fetched by name, after the runtime, and the listing is fetched beside it", async () => {
  const {prefetchApplication} = await load();
  const {steps, prefetched, options} = harness();
  let releaseRuntime;
  const after = () => new Promise(resolve => {
    releaseRuntime = () => { steps.push('runtime'); resolve(); };
  });
  prefetchApplication('so.tilo.chat', {...options, after});
  await settle();
  // The few-kilobyte listing does not wait for the runtime.
  assert.deepEqual(steps, ['fetch apps.json']);
  assert.deepEqual(prefetched, []);
  releaseRuntime();
  await settle();
  assert.deepEqual(steps, ['fetch apps.json', 'runtime']);
  assert.deepEqual(prefetched, ['chat.js']);
});

test('the listing is requested once however many apps ask, and an unknown app prefetches nothing', async () => {
  const {prefetchApplication} = await load();
  const {steps, prefetched, options} = harness();
  prefetchApplication('so.tilo.chat', options);
  prefetchApplication('so.tilo.chat', options);
  prefetchApplication('so.tilo.notes', options);
  await settle();
  assert.deepEqual(steps, ['fetch apps.json'], 'one listing for every ask');
  // The same app asked for twice asks the loader twice, which compiles once
  // (`__wasm_split.js`), and the app the listing does not name asks nothing.
  assert.deepEqual(prefetched, ['chat.js', 'chat.js']);
});

test('a bundle without a listing, or a refused one, leaves the app to its first open', async () => {
  const {prefetchApplication} = await load();
  const missing = harness({listingOk: false});
  prefetchApplication('so.tilo.chat', missing.options);
  await settle();
  assert.deepEqual(missing.prefetched, [], 'no listing, no prefetch');

  const offline = harness({failListing: true});
  prefetchApplication('so.tilo.chat', offline.options);
  await settle();
  assert.deepEqual(offline.prefetched, [], 'no listing, no prefetch');
});

test('a failed load leaves the app to its first open, not an unhandled rejection', async () => {
  const {prefetchApplication} = await load();
  const {prefetched, options} = harness();
  const brokenLoader = async () => { throw new Error('module gone'); };
  prefetchApplication('so.tilo.chat', {...options, load: brokenLoader});
  await settle();
  assert.deepEqual(prefetched, []);
});

/** A loader whose prefetch records the modules it was asked for and settles
 * at once, as the splitter's does (`__wasm_split.js`). */
function offeredHarness(listed) {
  const prefetched = [];
  const loader = async () => ({prefetch: module => { prefetched.push(module); return Promise.resolve(true); }});
  const fetch = async () => ({ok: true, json: async () => ({applications: listed})});
  return {prefetched, options: {fetch, load: loader, idle: run => run({timeRemaining: () => 4})}};
}
const flush = async () => { for (let i = 0; i < 40; i += 1) await Promise.resolve(); };

test('every app the desktop offers is prefetched, so opening one needs no fetch', async () => {
  const {prefetchOfferedApplications} = await load();
  const listed = [
    {id: 'so.tilo.activity', module: 'activity'},
    {id: 'so.tilo.chat', module: 'chat'},
    {id: 'so.tilo.notes', module: 'notes'},
    {id: 'so.tilo.typst-editor', module: 'typst_editor'},
  ];
  const {prefetched, options} = offeredHarness(listed);
  await prefetchOfferedApplications(options);
  await flush();
  assert.deepEqual(
    [...prefetched].sort(),
    listed.map(entry => entry.module).sort(),
    'every offered app module was prefetched',
  );
});

test('the prefetch runs a bounded few at a time, each in an idle period', async () => {
  const {prefetchOfferedApplications} = await load();
  const listed = Array.from({length: 8}, (_, i) => ({id: `so.tilo.app${i}`, module: `app${i}`}));
  const prefetched = [];
  let inFlight = 0;
  let peak = 0;
  const idled = [];
  const loader = async () => ({prefetch: async module => {
    inFlight += 1;
    peak = Math.max(peak, inFlight);
    await Promise.resolve();
    prefetched.push(module);
    inFlight -= 1;
    return true;
  }});
  const fetch = async () => ({ok: true, json: async () => ({applications: listed})});
  await prefetchOfferedApplications({fetch, load: loader, idle: run => { idled.push(1); run({timeRemaining: () => 4}); }});
  await flush();
  assert.equal(prefetched.length, listed.length, 'every app was prefetched');
  assert.ok(peak <= 2, `at most two modules compiled at once, saw ${peak}`);
  assert.ok(idled.length >= listed.length, 'each fetch waited for the page to be idle first');
});

test('an app listing the bundle does not name is left to its first open', async () => {
  const {prefetchOfferedApplications} = await load();
  const {prefetched, options} = offeredHarness([{id: 'so.tilo.chat'}]);
  await prefetchOfferedApplications(options);
  await flush();
  assert.deepEqual(prefetched, [], 'an entry with no module prefetches nothing');
});

test('a refused listing, or a failed compile, leaves apps to their first open', async () => {
  const {prefetchOfferedApplications} = await load();
  const missing = {fetch: async () => ({ok: false, json: async () => ({})}),
    load: async () => ({prefetch: () => { throw new Error('no'); }}), idle: run => run()};
  await prefetchOfferedApplications(missing);
  await flush();
  const broken = {fetch: async () => ({ok: true, json: async () => ({applications: [{id: 'so.tilo.chat', module: 'chat'}]})}),
    load: async () => ({prefetch: async () => { throw new Error('module gone'); }}), idle: run => run()};
  await prefetchOfferedApplications(broken);
  await flush();
  // Nothing thrown: a failed prefetch is the app's first open to report.
});

test('the prefetch asks once', async () => {
  const {prefetchOfferedApplications} = await load();
  let fetches = 0;
  const fetch = async () => { fetches += 1; return {ok: true, json: async () => ({applications: [{id: 'so.tilo.chat', module: 'chat'}]})}; };
  const prefetched = [];
  const loader = async () => ({prefetch: module => { prefetched.push(module); return Promise.resolve(true); }});
  const options = {fetch, load: loader, idle: run => run()};
  await prefetchOfferedApplications(options);
  await prefetchOfferedApplications(options);
  await flush();
  assert.equal(fetches, 1, 'one listing for the page');
  assert.equal(prefetched.length, 1, 'one prefetch for the page');
});

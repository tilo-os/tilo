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

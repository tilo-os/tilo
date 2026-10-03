// End-to-end check of a running registry (tools/web/local-registry.sh --check):
// the browser-facing HTTP contract, then the core and every app resolved
// through registry-client.mjs and the real verifier, then a returning visit.
//
//   node check-registry.mjs REGISTRY_JSON VERIFIER_DIR PAGE_ORIGIN
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import path from 'node:path';
import {pathToFileURL} from 'node:url';

import {createRegistryClient} from './registry-client.mjs';
import {createVerifier} from './registry-verifier.mjs';

const [configPath, verifierDir, origin] = process.argv.slice(2);
const config = JSON.parse(await readFile(configPath, 'utf8'));
const bindings = await import(pathToFileURL(path.join(verifierDir, 'registry_verifier.js')).href);
await bindings.default({module_or_path: await readFile(path.join(verifierDir, 'registry_verifier_bg.wasm'))});
const verifier = createVerifier(bindings);
const entries = `${config.baseUrl}/entries`;

const first = await fetch(entries, {headers: {origin}});
assert.equal(first.status, 200);
assert.equal(first.headers.get('access-control-allow-origin'), origin);
assert.match(first.headers.get('vary') ?? '', /origin/i);
assert.equal(first.headers.get('cache-control'), 'no-cache');
const etag = first.headers.get('etag');
assert.ok(etag, 'the snapshot carries an ETag');
await first.arrayBuffer();
const revalidated = await fetch(entries, {headers: {origin, 'if-none-match': etag}});
assert.equal(revalidated.status, 304, 'an unchanged snapshot revalidates to 304');
const stranger = await fetch(entries, {headers: {origin: 'https://elsewhere.example'}});
assert.equal(stranger.headers.get('access-control-allow-origin'), null);
await stranger.arrayBuffer();
const preflight = await fetch(entries, {method: 'OPTIONS', headers: {origin, 'access-control-request-method': 'POST'}});
assert.equal(preflight.headers.get('access-control-allow-origin'), null, 'no cross-origin POST');
await preflight.arrayBuffer();
console.log(`http   CORS for ${origin} on GET only, ETag ${etag}, 304 on revalidation`);

/** Cache Storage in memory, keeping each response's URL like the real one. */
function memoryCaches() {
  const store = new Map();
  const located = (response, url) => Object.defineProperty(response, 'url', {value: url});
  return {
    async open() {
      return {
        async match(key) {
          const hit = store.get(String(key));
          return hit && located(new Response(hit.bytes, {headers: hit.headers}), hit.url);
        },
        async put(key, response) {
          store.set(String(key), {bytes: new Uint8Array(await response.arrayBuffer()), headers: [...response.headers], url: String(key)});
        },
      };
    },
  };
}

/** Node's fetch has no HTTP cache, so the client's cache modes are dropped. */
function loggingFetch(log) {
  return (url, {cache: _mode, ...init} = {}) => {
    log.push(new URL(url).pathname);
    return fetch(url, init);
  };
}

const caches = memoryCaches();
const log = [];
const options = {baseUrl: config.baseUrl, ownerKeyHex: config.ownerKeyHex, verifier, caches, runtimeAbi: config.runtimeAbi};
const client = createRegistryClient({...options, fetch: loggingFetch(log)});

const started = performance.now();
const core = await client.resolveRuntime();
const coreModule = await WebAssembly.compileStreaming(core.module);
const coreAbi = new TextDecoder().decode(WebAssembly.Module.customSections(coreModule, 'tilo.runtime_abi')[0]);
assert.equal(core.abi, coreAbi);
if (config.runtimeAbi) assert.equal(core.abi, config.runtimeAbi);
console.log(`core   ${core.abi}, ${core.assets.size} assets: ${[...core.assets.keys()].join(', ')}`);

const apps = await client.resolveApps(config.apps);
for (const [index, resolved] of apps.entries()) {
  const module = await WebAssembly.compileStreaming(resolved.module);
  const abi = new TextDecoder().decode(WebAssembly.Module.customSections(module, 'tilo.runtime_abi')[0]);
  assert.equal(resolved.abi, core.abi, `${config.apps[index]} was selected for the core's ABI`);
  assert.equal(abi, resolved.abi, `${config.apps[index]}'s module carries the ABI it was published for`);
  console.log(`app    ${config.apps[index]} verified and compiled`);
}
const firstVisit = [...log];
console.log(`visit  ${firstVisit.length} requests in ${Math.round(performance.now() - started)} ms: ${firstVisit.filter(p => p === '/entries').length} snapshot, ${firstVisit.filter(p => p.startsWith('/thin/')).length} thin tapps, ${firstVisit.filter(p => p.startsWith('/blobs/')).length} blobs`);

log.length = 0;
const returning = createRegistryClient({...options, fetch: loggingFetch(log)});
await returning.resolveRuntime();
await returning.resolveApps(config.apps);
assert.deepEqual(log, ['/entries'], 'a returning visit costs one snapshot request');
console.log('again  1 request (the snapshot); everything else from Cache Storage');

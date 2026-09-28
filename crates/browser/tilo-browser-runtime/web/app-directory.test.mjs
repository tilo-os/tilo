import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {createDirectoryResolver} from './app-directory.mjs';

function directory(files) {
  const requested = [];
  const fetch = async url => {
    requested.push(String(url));
    const body = files[String(url)];
    return body === undefined ? new Response('missing', {status: 404}) : new Response(body);
  };
  return {fetch, requested};
}
const module = Uint8Array.of(0, 0x61, 0x73, 0x6d, 1, 0, 0, 0);
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const manifest = (overrides = {}) => JSON.stringify({
  schema: 1, id: 'so.tilo.ebook', abi: 'tilo-runtime/2026.07+wasm.aa',
  module: {path: 'app.wasm', bytes: module.length, sha256: sha256(module)},
  assets: {'covers/lantern.png': 'assets/0f.png'}, ...overrides,
});
const base = 'http://dev.test/runtime/';
const files = extra => ({
  [`${base}apps/index.json`]: JSON.stringify({schema: 1, apps: {'so.tilo.ebook': 'apps/so.tilo.ebook/app.json'}}),
  [`${base}apps/so.tilo.ebook/app.json`]: manifest(),
  [`${base}apps/so.tilo.ebook/app.wasm`]: module,
  ...extra,
});

test('an app resolves from the directory with its ABI, verified module bytes and asset URLs', async () => {
  const {fetch, requested} = directory(files());
  const resolver = createDirectoryResolver({baseURL: base, fetch});
  const resolved = await resolver.resolveApp('so.tilo.ebook');
  assert.deepEqual([...resolved.module], [...module]);
  assert.equal(resolved.abi, 'tilo-runtime/2026.07+wasm.aa');
  assert.deepEqual([...resolved.assets], [['covers/lantern.png', `${base}apps/so.tilo.ebook/assets/0f.png`]]);
  await resolver.resolveApps(['so.tilo.ebook', 'so.tilo.ebook']);
  assert.equal(requested.filter(url => url.endsWith('index.json')).length, 1, 'the index is read once');
});

test('an unlisted app, a mislabelled manifest or altered bytes are refused', async () => {
  const unlisted = createDirectoryResolver({baseURL: base, fetch: directory(files()).fetch});
  await assert.rejects(unlisted.resolveApp('so.tilo.nope'), /so\.tilo\.nope is not in http:\/\/dev\.test\/runtime\/apps\/index\.json/);
  const mislabelled = createDirectoryResolver({baseURL: base, fetch: directory(files({[`${base}apps/so.tilo.ebook/app.json`]: manifest({id: 'so.tilo.other'})})).fetch});
  await assert.rejects(mislabelled.resolveApp('so.tilo.ebook'), /does not describe so\.tilo\.ebook/);
  const altered = createDirectoryResolver({baseURL: base, fetch: directory(files({[`${base}apps/so.tilo.ebook/app.wasm`]: Uint8Array.of(0, 0x61, 0x73, 0x6d, 1, 0, 0, 1)})).fetch});
  await assert.rejects(altered.resolveApp('so.tilo.ebook'), /does not match its manifest/);
});

test('a missing index is retried on the next resolution', async () => {
  const served = files();
  const index = served[`${base}apps/index.json`];
  delete served[`${base}apps/index.json`];
  const resolver = createDirectoryResolver({baseURL: base, fetch: directory(served).fetch});
  await assert.rejects(resolver.resolveApp('so.tilo.ebook'), /index\.json HTTP 404/);
  served[`${base}apps/index.json`] = index;
  assert.equal((await resolver.resolveApp('so.tilo.ebook')).abi, 'tilo-runtime/2026.07+wasm.aa');
});

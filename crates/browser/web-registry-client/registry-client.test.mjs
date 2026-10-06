import assert from 'node:assert/strict';
import {resolveObjectURL} from 'node:buffer';
import {createHash} from 'node:crypto';
import {test} from 'node:test';

import {createRegistryClient} from './registry-client.mjs';

const BASE = 'https://apps.example';
const OWNER = 'ab'.repeat(32);
const ABI = 'tilo-runtime/2026.07+wasm.fe8836616de68a65d59c02eb73caa005';
const OLD_ABI = 'tilo-runtime/2026.07+wasm.00112233445566778899aabbccddeeff';

const text = value => new TextEncoder().encode(value);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const wasm = label => new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0, ...text(label)]);

/** A response that, like a fetched one, keeps its URL through clone(). */
function located(response, url) {
  Object.defineProperty(response, 'url', {value: url});
  const clone = response.clone.bind(response);
  response.clone = () => located(clone(), url);
  return response;
}

/**
 * A fake verifier with the real one's interface: a snapshot is JSON
 * prefixed with `SIGNED:`, a thin tapp is the JSON of its package, its root
 * is the SHA-256 of those bytes, and SHA-256 stands in for BLAKE3.
 */
const verifier = {
  verifySnapshot({bytes, ownerKeyHex, checkpoint, nowUnix}) {
    const body = new TextDecoder().decode(bytes);
    if (!body.startsWith('SIGNED:') || ownerKeyHex !== OWNER || !(nowUnix > 0)) throw Error('bad signature');
    const snapshot = JSON.parse(body.slice(7));
    const previous = checkpoint ? JSON.parse(new TextDecoder().decode(checkpoint)) : null;
    if (previous && snapshot.sequence < previous.sequence) throw Error('rollback');
    return {
      sequence: snapshot.sequence,
      checkpoint: text(JSON.stringify({sequence: snapshot.sequence})),
      select(packageId, {features, runtime}) {
        assert.ok(features.includes('sign-ext'));
        const versions = (snapshot.packages[packageId] ?? []).filter(v => runtime === undefined || v.runtime === runtime);
        if (!versions.length) throw Error(`${packageId} has no version for ${runtime}`);
        const chosen = versions.at(-1);
        return {packageId, version: chosen.version, packageRoot: chosen.root, runtime: chosen.runtime};
      },
    };
  },
  verifyPackage({bytes, selection}) {
    if (hash(bytes) !== selection.packageRoot) throw Error('root mismatch');
    return JSON.parse(new TextDecoder().decode(bytes));
  },
  blake3Hex: bytes => hash(bytes),
};

/** A registry: packages, their thin tapps and blobs, served by a logging fake fetch. */
function registry() {
  const files = new Map();
  const packages = {};
  let sequence = 1;
  const server = {
    log: [],
    held: null,
    tamper: new Map(),
    add({id, version = '1.0.0', kind = 'app', runtime = ABI, module, assets = {}}) {
      const blob = bytes => {
        const address = hash(bytes);
        files.set(`/blobs/${address}`, {bytes, type: bytes[1] === 0x61 ? 'application/wasm' : 'application/octet-stream'});
        return address;
      };
      const pkg = {
        packageId: id, version, kind, runtime,
        module: {path: 'app.wasm', blake3: blob(module)},
        assets: Object.entries(assets).map(([name, bytes]) => ({name, blake3: blob(bytes)})),
      };
      const thin = text(JSON.stringify(pkg));
      const root = hash(thin);
      files.set(`/thin/${root}.tapp`, {bytes: thin, type: 'application/octet-stream'});
      (packages[id] ??= []).push({version, root, runtime});
      return pkg;
    },
    publish(next = sequence + 1) {sequence = next;},
    hold() {
      let release;
      server.held = new Promise(resolve => {release = resolve;});
      return release;
    },
    async fetch(url, init = {}) {
      server.log.push({path: new URL(url).pathname, search: new URL(url).search, cache: init.cache, credentials: init.credentials});
      if (server.held) await server.held;
      const path = new URL(url).pathname;
      if (path === '/entries') {
        return new Response(text(`SIGNED:${JSON.stringify({sequence, packages})}`), {headers: {'content-type': 'application/json'}});
      }
      const file = files.get(path);
      if (!file) return new Response('not found', {status: 404});
      const bytes = server.tamper.get(path) ?? file.bytes;
      return located(new Response(bytes, {headers: {'content-type': file.type}}), url);
    },
  };
  return server;
}

/** Cache Storage in memory, keeping each response's URL like the real one. */
function memoryCaches() {
  const buckets = new Map();
  return {
    buckets,
    async open(name) {
      if (!buckets.has(name)) buckets.set(name, new Map());
      const store = buckets.get(name);
      return {
        async match(key) {
          const hit = store.get(String(key));
          if (!hit) return undefined;
          return located(new Response(hit.bytes, {headers: hit.headers}), hit.url);
        },
        async put(key, response) {
          const bytes = new Uint8Array(await response.arrayBuffer());
          store.set(String(key), {bytes, headers: [...response.headers], url: response.url || ''});
        },
      };
    },
  };
}

function client(server, caches, options = {}) {
  return createRegistryClient({
    baseUrl: `${BASE}/`,
    ownerKeyHex: OWNER,
    verifier,
    fetch: server.fetch,
    caches,
    now: () => 1_000_000_000,
    ...options,
  });
}

const bytesOf = async response => new Uint8Array(await response.arrayBuffer());
const paths = server => server.log.map(entry => entry.path);

test('resolveApps fetches one snapshot, then every thin tapp, then every missing blob, each once', async () => {
  const server = registry();
  const font = text('shared font');
  const books = server.add({id: 'so.tilo.ebook', module: wasm('books'), assets: {'fonts/body.ttf': font}});
  const writer = server.add({id: 'so.tilo.word-processor', module: wasm('writer'), assets: {'fonts/body.ttf': font}});
  const caches = memoryCaches();
  const [first, second] = await client(server, caches).resolveApps(['so.tilo.ebook', 'so.tilo.word-processor']);

  assert.deepEqual(paths(server).slice(0, 3), ['/entries', `/thin/${hash(text(JSON.stringify(books)))}.tapp`, `/thin/${hash(text(JSON.stringify(writer)))}.tapp`]);
  const blobs = paths(server).filter(path => path.startsWith('/blobs/'));
  assert.deepEqual(blobs.sort(), [books.module.blake3, writer.module.blake3, hash(font)].map(h => `/blobs/${h}`).sort(), 'the shared font once');
  assert.equal(server.log[0].cache, 'no-cache', 'the snapshot revalidates');
  assert.equal(server.log[0].search, '?format=compact', 'the snapshot is asked for in its compact form');
  assert.ok(server.log.every(entry => entry.credentials === 'omit'));

  assert.equal(first.abi, ABI);
  assert.ok(first.module instanceof Response);
  assert.equal(first.module.headers.get('content-type'), 'application/wasm');
  assert.equal(first.module.url, `${BASE}/blobs/${books.module.blake3}`, 'code caching keys on this URL');
  assert.deepEqual(await bytesOf(first.module), wasm('books'));
  assert.deepEqual([...second.assets.keys()], ['fonts/body.ttf']);
  const assetUrl = second.assets.get('fonts/body.ttf');
  assert.match(assetUrl, /^blob:/, 'assets are handed over as the verified bytes');
  assert.deepEqual(new Uint8Array(await resolveObjectURL(assetUrl).arrayBuffer()), font);
});

test('a returning visit costs one snapshot request and serves modules from Cache Storage', async () => {
  const server = registry();
  server.add({id: 'so.tilo.ebook', module: wasm('books')});
  const caches = memoryCaches();
  await client(server, caches).resolveApp('so.tilo.ebook');
  server.log.length = 0;

  const again = await client(server, caches).resolveApp('so.tilo.ebook');
  assert.deepEqual(paths(server), ['/entries']);
  assert.deepEqual(await bytesOf(again.module), wasm('books'));
  assert.match(again.module.url, /\/blobs\/[0-9a-f]{64}$/);
  assert.equal(globalThis.localStorage, undefined);
});

test('resolveApp calls made together share one batch and start every download before any finishes', async () => {
  const server = registry();
  const ids = ['so.tilo.a', 'so.tilo.b', 'so.tilo.c'];
  for (const id of ids) server.add({id, module: wasm(id)});
  const registryClient = client(server, memoryCaches());
  const release = server.hold();
  const results = Promise.all(ids.map(id => registryClient.resolveApp(id)));
  await new Promise(resolve => setTimeout(resolve, 5));
  assert.deepEqual(paths(server), ['/entries'], 'one snapshot for the batch');
  release();
  server.held = null;
  await results;
  const thin = paths(server).filter(path => path.startsWith('/thin/'));
  const blobs = paths(server).filter(path => path.startsWith('/blobs/'));
  assert.equal(thin.length, 3);
  assert.equal(blobs.length, 3);
  assert.ok(paths(server).lastIndexOf(thin.at(-1)) < paths(server).indexOf(blobs[0]), 'blobs after all thin tapps');
});

test('a blob that does not match its hash is refused and never cached', async () => {
  const server = registry();
  const books = server.add({id: 'so.tilo.ebook', module: wasm('books')});
  const caches = memoryCaches();
  server.tamper.set(`/blobs/${books.module.blake3}`, wasm('evil'));
  await assert.rejects(client(server, caches).resolveApp('so.tilo.ebook'), {code: 'TILO_REGISTRY_UNVERIFIED'});
  const bucket = caches.buckets.get('tilo-registry');
  assert.ok(![...bucket.keys()].some(key => key.includes('/blobs/')));

  server.tamper.clear();
  const fixed = await client(server, caches).resolveApp('so.tilo.ebook');
  assert.deepEqual(await bytesOf(fixed.module), wasm('books'));
});

test('the anti-rollback checkpoint lives in Cache Storage and refuses an older snapshot', async () => {
  const server = registry();
  server.add({id: 'so.tilo.ebook', module: wasm('books')});
  const caches = memoryCaches();
  server.publish(5);
  await client(server, caches).resolveApp('so.tilo.ebook');
  const checkpoint = caches.buckets.get('tilo-registry').get(`${BASE}/.tilo/checkpoint/${OWNER}`);
  assert.deepEqual(JSON.parse(new TextDecoder().decode(checkpoint.bytes)), {sequence: 5});

  server.publish(4);
  await assert.rejects(client(server, caches).resolveApp('so.tilo.ebook'), {code: 'TILO_REGISTRY_UNVERIFIED'});
  await assert.rejects(client(server, caches, {ownerKeyHex: 'cd'.repeat(32)}).resolveApp('so.tilo.ebook'), {code: 'TILO_REGISTRY_UNVERIFIED'});
});

test('an aborted caller rejects at once while others still get the shared result', async () => {
  const server = registry();
  server.add({id: 'so.tilo.ebook', module: wasm('books')});
  const registryClient = client(server, memoryCaches());
  const release = server.hold();
  const controller = new AbortController();
  const aborted = registryClient.resolveApp('so.tilo.ebook', {signal: controller.signal});
  const other = registryClient.resolveApp('so.tilo.ebook');
  controller.abort(Error('closed'));
  await assert.rejects(aborted, {message: 'closed'});
  release();
  server.held = null;
  assert.deepEqual(await bytesOf((await other).module), wasm('books'));
});

test('the core resolves by ABI and apps are then selected for it', async () => {
  const server = registry();
  server.add({id: 'so.tilo.runtime.web', kind: 'runtime', runtime: ABI, module: wasm('core')});
  server.add({id: 'so.tilo.ebook', version: '1.0.0', runtime: ABI, module: wasm('books for the core')});
  server.add({id: 'so.tilo.ebook', version: '0.9.0', runtime: OLD_ABI, module: wasm('books for an older core')});
  const registryClient = client(server, memoryCaches());
  const core = await registryClient.resolveRuntime({abi: ABI});
  assert.equal(core.abi, ABI);
  assert.deepEqual(await bytesOf(core.module), wasm('core'));
  const books = await registryClient.resolveApp('so.tilo.ebook');
  assert.equal(books.abi, ABI);
  assert.deepEqual(await bytesOf(books.module), wasm('books for the core'));

  const pinned = client(server, memoryCaches(), {runtimeAbi: OLD_ABI});
  assert.deepEqual(await bytesOf((await pinned.resolveApp('so.tilo.ebook')).module), wasm('books for an older core'));
  await assert.rejects(pinned.resolveApp('so.tilo.runtime.web'), {code: 'TILO_APP_UNAVAILABLE'}, 'a core is not an app');
});

test('resolveApps reports which apps failed and still resolves the rest', async () => {
  const server = registry();
  server.add({id: 'so.tilo.ebook', module: wasm('books')});
  const failure = await client(server, memoryCaches()).resolveApps(['so.tilo.ebook', 'so.tilo.missing']).catch(error => error);
  assert.ok(failure instanceof AggregateError);
  assert.deepEqual([...failure.failed.keys()], ['so.tilo.missing']);
  assert.equal(failure.failed.get('so.tilo.missing').code, 'TILO_APP_UNAVAILABLE');
  assert.deepEqual(await bytesOf(failure.resolved.get('so.tilo.ebook').module), wasm('books'));
});

test('without Cache Storage the verified bytes are still returned', async () => {
  const server = registry();
  server.add({id: 'so.tilo.ebook', module: wasm('books'), assets: {'icon.svg': text('<svg/>')}});
  const resolved = await client(server, undefined, {caches: undefined}).resolveApp('so.tilo.ebook');
  assert.equal(resolved.module.headers.get('content-type'), 'application/wasm');
  assert.deepEqual(await bytesOf(resolved.module), wasm('books'));
});

test('options are checked', () => {
  assert.throws(() => createRegistryClient({baseUrl: BASE, ownerKeyHex: 'nope', verifier}), TypeError);
});

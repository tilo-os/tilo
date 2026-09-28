import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {APP_ALIASES, createAppInstaller, localSeed} from './app-installer.mjs';

const emptyModule = () => new WebAssembly.Module(Uint8Array.of(0, 0x61, 0x73, 0x6d, 1, 0, 0, 0));

/** A core whose catalogue holds `compiled`; linking a module registers the app it was resolved for. */
function fakeCore({compiled = [], abi = 'abi-1', register = name => name} = {}) {
  const installed = new Set(compiled);
  let releaseCore;
  const coreLoaded = new Promise(resolve => {releaseCore = resolve;});
  const events = [];
  const installer = createAppInstaller({
    ready: () => coreLoaded,
    installed: app => installed.has(app),
    linker: () => ({abi, instantiate: async (_module, name) => {
      events.push(`link ${name}`);
      return {tilo_app_register: () => {const id = register(name); if (installed.has(id)) return 1; installed.add(id); return 0;}};
    }}),
  });
  return {installer, installed, events, releaseCore};
}

test('an app the core lacks is resolved while the core loads, then linked and registered', async () => {
  const core = fakeCore();
  const calls = [];
  const statuses = [];
  const assets = new Map([['cover.png', 'https://registry.example/blobs/ab']]);
  const job = core.installer.ensure('so.tilo.ebook', {
    resolveApp: async (appId, {signal}) => {calls.push([appId, signal]); return {module: emptyModule(), abi: 'abi-1', assets};},
    onStatus: status => statuses.push(status),
  });
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.deepEqual(calls.map(([id]) => id), ['so.tilo.ebook'], 'resolution does not wait for the core');
  assert.deepEqual(core.events, []);
  core.releaseCore();
  await job;
  assert(core.installed.has('so.tilo.ebook'));
  assert.deepEqual(statuses.map(s => s.phase), ['resolving', 'linking', 'linked']);
  assert.equal(core.installer.assets('so.tilo.ebook').get('cover.png'), 'https://registry.example/blobs/ab');
});

test('a compiled-in app ignores the resolver, even a failing one', async () => {
  const core = fakeCore({compiled: ['so.tilo.word-processor']});
  core.releaseCore();
  await core.installer.ensure('writer', {resolveApp: async () => {throw Error('registry offline');}});
  assert.deepEqual(core.events, []);
});

test('aliases are resolved as full app ids, as catalog.rs spells them', async () => {
  const core = fakeCore();
  core.releaseCore();
  const asked = [];
  await core.installer.ensure('writer', {resolveApp: async appId => {asked.push(appId); return {module: emptyModule()};}});
  assert.deepEqual(asked, ['so.tilo.word-processor']);
  const catalog = readFileSync(new URL('../src/catalog.rs', import.meta.url), 'utf8');
  const rust = Object.fromEntries([...catalog.matchAll(/\("(\w+)", "([\w.-]+)"\)/g)].map(([, alias, id]) => [alias, id]));
  assert.deepEqual(rust, {...APP_ALIASES});
});

test('an unknown app without a resolver, or one the resolver cannot find, is unavailable', async () => {
  const core = fakeCore();
  core.releaseCore();
  await assert.rejects(core.installer.ensure('so.tilo.nope'), error => {
    assert.equal(error.code, 'TILO_APP_UNAVAILABLE');
    assert.equal(error.message, 'application "so.tilo.nope" is not available');
    return true;
  });
  await assert.rejects(core.installer.ensure('so.tilo.nope', {resolveApp: async () => {throw Error('404 from registry');}}),
    error => error.code === 'TILO_APP_UNAVAILABLE' && /not available: 404 from registry/.test(error.message) && error.cause.message === '404 from registry');
  await assert.rejects(core.installer.ensure('so.tilo.nope', {resolveApp: async () => ({})}), /the resolver returned no module/);
});

test('a manifest for another runtime ABI fails before the module is linked', async () => {
  const core = fakeCore({abi: 'tilo-runtime/2026.07+wasm.new'});
  core.releaseCore();
  await assert.rejects(core.installer.ensure('so.tilo.ebook', {resolveApp: async () => ({module: emptyModule(), abi: 'tilo-runtime/2026.07+wasm.old'})}),
    error => error.code === 'TILO_RUNTIME_ABI_MISMATCH' && error.actual === 'tilo-runtime/2026.07+wasm.old');
  assert.deepEqual(core.events, []);
});

test('a module that registers another app does not satisfy the mount', async () => {
  const core = fakeCore({register: () => 'so.tilo.other'});
  core.releaseCore();
  await assert.rejects(core.installer.ensure('so.tilo.ebook', {resolveApp: async () => ({module: emptyModule()})}), /registered a different app/);
});

test('concurrent requests share one resolution, and a failed one can be retried', async () => {
  const core = fakeCore();
  core.releaseCore();
  let calls = 0, online = false;
  const resolveApp = async () => {calls++; if (!online) throw Error('offline'); return {module: emptyModule()};};
  core.installer.setResolver(resolveApp);
  const results = await Promise.allSettled([core.installer.ensure('so.tilo.ebook'), core.installer.ensure('so.tilo.ebook')]);
  assert.deepEqual(results.map(r => r.status), ['rejected', 'rejected']);
  assert.equal(calls, 1);
  online = true;
  await core.installer.ensure('so.tilo.ebook');
  assert.equal(calls, 2);
  await core.installer.ensure('so.tilo.ebook');
  assert.equal(calls, 2, 'an installed app is not resolved again');
});

test('a caller that aborts stops waiting without cancelling the shared install', async () => {
  const core = fakeCore();
  let finish;
  core.installer.setResolver(() => new Promise(resolve => {finish = () => resolve({module: emptyModule()});}));
  const controller = new AbortController();
  const aborted = core.installer.ensure('so.tilo.ebook', {signal: controller.signal});
  const other = core.installer.ensure('so.tilo.ebook');
  controller.abort(Error('mount closed'));
  await assert.rejects(aborted, /mount closed/);
  core.releaseCore();
  await new Promise(resolve => setTimeout(resolve, 0));
  finish();
  await other;
  assert(core.installed.has('so.tilo.ebook'));
});

test('preloadApps installs a session\'s apps in one concurrent batch and reports each failure', async () => {
  const core = fakeCore({compiled: ['so.tilo.notes']});
  const started = [];
  core.installer.setResolver(async appId => {
    started.push(appId);
    if (appId === 'so.tilo.gone') throw Error('not in the registry');
    return {module: emptyModule()};
  });
  const batch = core.installer.preload(['so.tilo.ebook', 'writer', 'so.tilo.gone', 'so.tilo.notes', 'so.tilo.ebook']);
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.deepEqual(started.sort(), ['so.tilo.ebook', 'so.tilo.gone', 'so.tilo.notes', 'so.tilo.word-processor'], 'all requested at once, before the core is ready');
  core.releaseCore();
  await assert.rejects(batch, error => {
    assert(error instanceof AggregateError);
    assert.deepEqual([...error.failed.keys()], ['so.tilo.gone']);
    assert.deepEqual(error.installed, ['so.tilo.ebook', 'so.tilo.word-processor', 'so.tilo.notes']);
    return true;
  });
  assert.deepEqual(await core.installer.preload(['so.tilo.ebook', 'so.tilo.word-processor']), ['so.tilo.ebook', 'so.tilo.word-processor']);
});

test('a local desktop seed names every app by its full id and lists what to link', () => {
  const {seed, apps} = localSeed({
    apps: ['writer'],
    workspaces: [
      {title: 'Reading', strips: [{title: 'Odyssey', windows: [{app: 'so.tilo.ebook', record: 'r1', focused: true}, {app: 'chat'}]}]},
      {title: 'Writing'},
    ],
  });
  assert.deepEqual(apps, ['so.tilo.word-processor', 'so.tilo.ebook', 'so.tilo.chat']);
  assert.deepEqual(seed.apps, apps);
  assert.deepEqual(seed.workspaces[0].strips[0].windows, [
    {app: 'so.tilo.ebook', record: 'r1', focused: true},
    {app: 'so.tilo.chat'},
  ]);
  assert.deepEqual(seed.workspaces[1], {title: 'Writing', strips: []});
  assert.deepEqual(localSeed({}), {seed: {apps: [], workspaces: []}, apps: []});
  assert.throws(() => localSeed(null), /local must be an object/);
});

test('a desktop finds an asset among every linked app', async () => {
  const core = fakeCore();
  core.releaseCore();
  const resolveApp = async appId => ({module: emptyModule(), assets: new Map([[`${appId}.png`, `https://registry.example/${appId}`]])});
  await core.installer.preload(['so.tilo.ebook', 'so.tilo.word-processor'], {resolveApp});
  assert.equal(core.installer.asset('so.tilo.word-processor.png'), 'https://registry.example/so.tilo.word-processor');
  assert.equal(core.installer.asset('missing.png'), undefined);
});

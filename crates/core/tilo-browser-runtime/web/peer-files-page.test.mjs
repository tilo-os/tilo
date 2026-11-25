// The page's peer store in a real browser: the runtime's store (`src/peer_store.rs`) over
// IndexedDB through `peer-files.mjs` and its worker, reloaded the way a person reloads the page.
// Needs the runtime built for the page first (`cargo build --locked --lib -p
// tilo-browser-runtime --target wasm32-unknown-unknown --profile wasm-dev`), wasm-bindgen at the
// crate's pinned version and Chrome (`CHROME`); t's `web-peer-store` check runs it with all three.
import test from 'node:test';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdtempSync, rmSync, statSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {launch, serve} from '../../../../tools/web/chrome.mjs';

const web = dirname(fileURLToPath(import.meta.url));
const repo = join(web, '../../../..');

/** The runtime's module for the page, beside a page that opens profile stores. */
function page() {
  const out = mkdtempSync(join(tmpdir(), 'peer-files-page-'));
  const target = JSON.parse(execFileSync(process.env.CARGO ?? 'cargo',
    ['metadata', '--locked', '--format-version', '1', '--no-deps', '--offline'],
    {cwd: repo, stdio: ['ignore', 'pipe', 'inherit'], maxBuffer: 64 * 1024 * 1024})).target_directory;
  execFileSync(process.env.WASM_BINDGEN ?? 'wasm-bindgen', [
    join(target, 'wasm32-unknown-unknown/wasm-dev/tilo_browser_runtime.wasm'),
    '--target', 'web', '--out-dir', out, '--out-name', 'runtime',
  ], {stdio: 'inherit'});
  console.log(`# the runtime's module: ${(statSync(join(out, 'runtime_bg.wasm')).size / 2 ** 20).toFixed(1)} MiB`);
  writeFileSync(join(out, 'elsewhere.html'), '<!doctype html><p>another page</p>');
  writeFileSync(join(out, 'index.html'), `<!doctype html><script type="module">
try {
globalThis.stage = 'importing the runtime';
const runtime = await import('./runtime.js');
globalThis.stage = 'importing peer-files.mjs';
const {createProfileStorage} = await import('./peer-files.mjs');
const {readFile} = await import('./peer-files-db.mjs');
globalThis.stage = 'starting the runtime';
await runtime.default();
globalThis.stage = 'started';
globalThis.runtime = runtime;
// Each flush the page asks of a store is counted where the next page can read it, and when a
// store hands over a commit or lets the page run is noted.
globalThis.timeline = [];
const noted = (what, promise) => { timeline.push([what, performance.now()]); return promise.finally(() => timeline.push([what + ' done', performance.now()])); };
const counted = {...runtime, flush_peer_store(profile, device) {
  try { localStorage.setItem('flushes', String(Number(localStorage.getItem('flushes') ?? 0) + 1)); }
  catch { globalThis.uncounted = true; }
  return runtime.flush_peer_store(profile, device);
}, open_peer_store(profile, device, rows, commit, pause) {
  return runtime.open_peer_store(profile, device, rows, packed => noted('commit', commit(packed)), () => noted('pause', pause()));
}};
// The storage worker, as the page starts it; while \`refuseCommits\` is set, it answers each commit
// the way it answers one IndexedDB refused for want of room.
const createWorker = () => {
  const worker = new Worker(new URL('./peer-files-worker.mjs', location.href), {type: 'module'});
  const listeners = new Set();
  worker.addEventListener('message', event => { for (const listener of listeners) listener(event); });
  return {
    postMessage(message, transfer) {
      if (message.type !== 'commit' || !globalThis.refuseCommits) return worker.postMessage(message, transfer);
      const data = {id: message.id, ok: false, error: {name: 'QuotaExceededError', message: 'the disk is full'}};
      setTimeout(() => { for (const listener of listeners) listener({data}); });
    },
    addEventListener(type, listener) { if (type === 'message') listeners.add(listener); else worker.addEventListener(type, listener); },
    start() {},
  };
};
globalThis.storage = createProfileStorage({runtime: counted, createWorker});
globalThis.heard = [];
globalThis.handles = new Map();
globalThis.openProfile = async (profile, device) => {
  const handle = await storage.open(profile, device);
  handle.subscribe(status => heard.push(status.persistence));
  handles.set(profile + '/' + device, handle);
};
// \`kept(namespace, path)\`: the file IndexedDB holds, as text, read past the runtime.
globalThis.kept = (namespace, path) => new Promise((resolve, reject) => {
  const request = indexedDB.open('tilo-peer-files:' + namespace);
  // A namespace nothing kept yet stays so: reading it must not make its database.
  request.onupgradeneeded = () => request.transaction.abort();
  request.onerror = () => request.error?.name === 'AbortError' ? resolve(null) : reject(request.error);
  request.onsuccess = () => {
    const db = request.result;
    readFile(db, path).then(bytes => resolve(bytes && new TextDecoder().decode(bytes)), reject).finally(() => db.close());
  };
});
// Whether \`kept\` reads \`want\` within a few seconds: a commit the store started on its own
// tells the page nothing when it lands, so this reads IndexedDB again until it holds it.
globalThis.lands = async (namespace, path, want) => {
  for (const until = Date.now() + 5000; Date.now() < until; await new Promise(next => setTimeout(next, 20))) {
    if (await kept(namespace, path) === want) return true;
  }
  return false;
};
// The page thread's time a commit of \`count\` changed records takes, as the runs of the store's
// own work between handing the page back: from the changes to its first pause, from each pause or
// the worker's answer to what the store does next, and from the last to the flush resolving. The
// longest the page waited for a task meanwhile is said too: it also holds what is not the store's,
// the browser collecting garbage or the machine running something else.
globalThis.measureCommit = async (profile, device, count) => {
  const record = new Uint8Array(96).fill(7);
  // What opening the store wrote is committed first: the commit measured holds the records alone.
  await handles.get(profile + '/' + device).flush();
  const writing = performance.now();
  for (let n = 0; n < count; n++) runtime.set_profile_value(profile, device, 'record-' + n, record);
  const changed = performance.now();
  timeline.length = 0;
  await new Promise(resolve => queueMicrotask(resolve));
  const packed = performance.now() - changed;
  let longest = 0, longestAt = 0, last = performance.now(), going = true, ticks = 0;
  const channel = new MessageChannel();
  channel.port1.onmessage = () => {
    const at = performance.now();
    if (at - last > longest) { longest = at - last; longestAt = at; }
    last = at; ticks++;
    if (going) channel.port2.postMessage(0);
  };
  channel.port2.postMessage(0);
  const started = performance.now();
  await handles.get(profile + '/' + device).flush();
  const flushed = performance.now();
  going = false;
  channel.port1.close();
  const marks = [['changed', changed], ...timeline, ['flushed', flushed]];
  const slices = marks.slice(0, -1).flatMap(([what, at], n) => what === 'changed' || what.endsWith(' done') ? [marks[n + 1][1] - at] : []);
  const before = marks.filter(([, at]) => at < longestAt - longest).at(-1);
  return {slowest: Math.max(...slices), slices: slices.length, packed, longest, ticks, committed: flushed - started, written: changed - writing,
    after: before[0] + ' ' + (longestAt - longest - before[1]).toFixed(1) + ' ms before'};
};
globalThis.pageReady = true;
} catch (error) { globalThis.pageError = String(error?.stack ?? error); }
// The browser driver waits on the page's changes: marking the document tells it the page is in.
document.documentElement.dataset.page = globalThis.pageReady ? 'ready' : 'failed';
</script>`);
  return out;
}

const text = value => JSON.stringify([...new TextEncoder().encode(value)]);

test('the page keeps its peer store in IndexedDB across reloads, offline, and says when a commit failed', async t => {
  const out = page();
  const server = await serve(out, {also: [web]});
  const browser = await launch();
  t.after(async () => {
    try { await browser.close(); } finally { await server.close(); rmSync(out, {recursive: true, force: true}); }
  });
  const tab = await browser.tab();
  // What the page said is the reason a load failed: a module that did not load, or a trap.
  const load = async () => {
    const said = () => tab.evaluate('JSON.stringify({at: location.href, state: document.readyState, stage: globalThis.stage ?? null, error: globalThis.pageError ?? null})')
      .catch(error => error.message).then(page => `${page}; the page said:\n${tab.log.join('\n')}`);
    await tab.waitFor('globalThis.pageReady === true || globalThis.pageError', {timeout: 180_000})
      .catch(async error => { throw new Error(`${error.message}: ${await said()}`); });
    if (await tab.evaluate('globalThis.pageReady !== true')) throw new Error(`the page did not load: ${await said()}`);
  };
  const reload = async () => { await tab.reload(); await load(); };
  const offline = async down => {
    await tab.send('Network.emulateNetworkConditions', {offline: down, latency: 0, downloadThroughput: -1, uploadThroughput: -1});
    assert.equal(await tab.evaluate(`fetch(location.href, {cache: 'no-store'}).then(() => 'online', () => 'offline')`), down ? 'offline' : 'online');
  };
  const value = (profile, device, name) => tab.evaluate(
    `(() => { const bytes = runtime.profile_value(${JSON.stringify(profile)}, ${JSON.stringify(device)}, ${JSON.stringify(name)}); return bytes ? new TextDecoder().decode(bytes) : null; })()`);
  const set = (profile, device, name, kept) => tab.evaluate(
    `runtime.set_profile_value(${JSON.stringify(profile)}, ${JSON.stringify(device)}, ${JSON.stringify(name)}, ${kept === null ? 'undefined' : `new Uint8Array(${text(kept)})`})`);
  const handle = (profile, device) => `handles.get(${JSON.stringify(`${profile}/${device}`)})`;
  const namespace = (profile, device) => JSON.stringify(JSON.stringify([profile, device]));
  await tab.send('Network.enable', {});
  await tab.navigate(`${server.url}/index.html`);
  await load();

  // A fresh namespace opens empty; what is flushed is there after a reload, and only in its namespace.
  await tab.evaluate(`openProfile('alice', 'laptop')`);
  assert.equal(await value('alice', 'laptop', 'greeting'), null);
  assert.equal(await tab.evaluate(`runtime.savedPeerCertificate('alice', 'laptop', 'AB12') ?? null`), null);
  await set('alice', 'laptop', 'greeting', 'hello');
  await tab.evaluate(`${handle('alice', 'laptop')}.flush()`);
  await reload();
  await tab.evaluate(`openProfile('alice', 'laptop')`);
  await tab.evaluate(`openProfile('bob', 'laptop')`);
  assert.equal(await value('alice', 'laptop', 'greeting'), 'hello');
  assert.equal(await value('bob', 'laptop', 'greeting'), null);

  // A change commits on its own, without a flush; a deletion too.
  await set('alice', 'laptop', 'unflushed', 'saved anyway');
  await set('alice', 'laptop', 'greeting', null);
  assert.equal(await tab.evaluate(`lands(${namespace('alice', 'laptop')}, 'state/page/unflushed', 'saved anyway')`), true);
  assert.equal(await tab.evaluate(`lands(${namespace('alice', 'laptop')}, 'state/page/greeting', null)`), true);
  await reload();
  await tab.evaluate(`openProfile('alice', 'laptop')`);
  assert.equal(await value('alice', 'laptop', 'unflushed'), 'saved anyway');
  assert.equal(await value('alice', 'laptop', 'greeting'), null);

  // What the page keeps while it has no network is committed all the same, and the store holds
  // it when the page opens it again, still offline.
  await offline(true);
  await set('alice', 'laptop', 'offline', 'kept with no network');
  await tab.evaluate(`${handle('alice', 'laptop')}.release()`);
  await tab.evaluate(`openProfile('alice', 'laptop')`);
  assert.equal(await value('alice', 'laptop', 'offline'), 'kept with no network');
  // The page's code, its storage worker's too, comes from the test's file server: only loading
  // it goes online, and opening another profile starts the worker.
  await offline(false);
  await reload();
  await tab.evaluate(`openProfile('bob', 'laptop')`);
  await offline(true);
  await tab.evaluate(`openProfile('alice', 'laptop')`);
  assert.equal(await value('alice', 'laptop', 'offline'), 'kept with no network');
  await offline(false);

  // Hiding the page flushes every open store; leaving it does too, and what changed just before
  // is there when the person comes back.
  const flushes = () => tab.evaluate(`Number(localStorage.getItem('flushes') ?? 0)`);
  let before = await flushes();
  await set('alice', 'laptop', 'hidden', 'flushed when hidden');
  await tab.evaluate(`Object.defineProperty(document, 'visibilityState', {configurable: true, get: () => 'hidden'});
    document.dispatchEvent(new Event('visibilitychange'));
    new Promise(resolve => setTimeout(resolve, 0))`);
  assert.equal(await flushes(), before + 2, 'hiding the page flushed both open stores');
  assert.equal(await tab.evaluate(`lands(${namespace('alice', 'laptop')}, 'state/page/hidden', 'flushed when hidden')`), true);
  await tab.evaluate(`delete document.visibilityState`);
  before = await flushes();
  await set('alice', 'laptop', 'left', 'flushed when leaving');
  await tab.navigate(`${server.url}/elsewhere.html`);
  await tab.waitFor(`location.pathname === '/elsewhere.html' && document.readyState === 'complete'`);
  // Leaving hides the document and then hides the page: each flushes both stores.
  assert.equal(await flushes(), before + 4, 'leaving the page flushed both open stores');
  await tab.navigate(`${server.url}/index.html`);
  await load();
  await tab.evaluate(`openProfile('alice', 'laptop')`);
  assert.equal(await value('alice', 'laptop', 'left'), 'flushed when leaving');
  assert.equal(await value('alice', 'laptop', 'hidden'), 'flushed when hidden');

  // A commit of ten thousand changed records keeps the page thread free: the worker writes them,
  // and no page task it takes reaches one frame (16 ms).
  await tab.evaluate(`openProfile('large', 'laptop')`);
  const measured = await tab.evaluate(`measureCommit('large', 'laptop', 10000)`);
  console.log(`# a commit of 10000 records (written in ${measured.written.toFixed(0)} ms): ${measured.slices} runs of the store's work, the slowest ${measured.slowest.toFixed(1)} ms, the first ${measured.packed.toFixed(1)} ms; ${measured.committed.toFixed(0)} ms until committed, the page running ${measured.ticks} tasks meanwhile, the longest wait for one ${measured.longest.toFixed(1)} ms (${measured.after})`);
  assert.ok(measured.slowest < 16, JSON.stringify(measured));
  assert.equal(await tab.evaluate(`kept(${namespace('large', 'laptop')}, 'state/page/record-9999')`), '\u0007'.repeat(96));

  // IndexedDB refusing the write (the origin's storage is full): the flush fails and says why, the
  // store says so, the store is not closed over it, and what was committed before stays. Once
  // there is room, the next flush commits the change. The page opened alice's store once since it
  // came back, so the release below is its last: it flushes, and the refusal keeps it open.
  await tab.evaluate('globalThis.refuseCommits = true');
  await set('alice', 'laptop', 'refused', 'kept for the next commit');
  await assert.rejects(tab.evaluate(`${handle('alice', 'laptop')}.flush()`), /QuotaExceededError: the disk is full/);
  assert.equal(await tab.evaluate(`${handle('alice', 'laptop')}.status.persistence`), 'failed');
  assert.match(await tab.evaluate(`${handle('alice', 'laptop')}.status.error`), /QuotaExceededError: the disk is full/);
  assert.ok((await tab.evaluate('heard')).includes('failed'));
  await assert.rejects(tab.evaluate(`${handle('alice', 'laptop')}.release()`), /QuotaExceededError/);
  assert.equal(await value('alice', 'laptop', 'refused'), 'kept for the next commit');
  assert.equal(await tab.evaluate(`kept(${namespace('alice', 'laptop')}, 'state/page/refused')`), null);
  await tab.evaluate('globalThis.refuseCommits = false');
  await tab.evaluate(`${handle('alice', 'laptop')}.flush()`);
  assert.equal(await tab.evaluate(`${handle('alice', 'laptop')}.status.persistence`), 'committed');
  assert.deepEqual((await tab.evaluate('heard')).slice(-1), ['committed']);

  // The last release flushes and closes the store; it opens again with what it kept.
  await set('alice', 'laptop', 'at close', 'flushed by the release');
  await tab.evaluate(`${handle('alice', 'laptop')}.release()`);
  assert.match(await tab.evaluate(`(() => { try { runtime.profile_value('alice', 'laptop', 'at close'); return 'open'; } catch (error) { return String(error); } })()`),
    /open the profile's peer store/);
  await reload();
  await tab.evaluate(`openProfile('alice', 'laptop')`);
  assert.equal(await value('alice', 'laptop', 'refused'), 'kept for the next commit');
  assert.equal(await value('alice', 'laptop', 'at close'), 'flushed by the release');
  assert.equal(await value('alice', 'laptop', 'unflushed'), 'saved anyway');
});

import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {deployedVersion, servedVersion, UNAVAILABLE, watchForUpdate} from './bundle-update.mjs';

const OLD = '72d28c1996d5372ef9de';
const NEW = 'a1b2c3d4e5f60718293a';
const json = body => new Response(JSON.stringify(body), {headers: {'content-type': 'application/json'}});
const missing = () => new Response('not found', {status: 404});
const settle = () => new Promise(resolve => setTimeout(resolve, 0));

test('a bundle file names the entry and the version it is served under', () => {
  assert.deepEqual(servedVersion(`https://tilo.coffee/v/${OLD}/pkg/tilo_browser_runtime.calculator.wasm`),
    {base: 'https://tilo.coffee/', version: OLD});
  assert.deepEqual(servedVersion(`https://example.org/tilo/v/${OLD}/entry-page.mjs`),
    {base: 'https://example.org/tilo/', version: OLD});
  // A development bundle is not laid out in versions: nothing to compare.
  assert.equal(servedVersion('http://127.0.0.1:8000/pkg/tilo_browser_runtime.calculator.wasm'), null);
  assert.equal(servedVersion('https://tilo.coffee/v/short/pkg/x.wasm'), null);
  assert.equal(servedVersion(undefined), null);
});

test('the deployed version is read past every cache, and only a well-formed one counts', async () => {
  const asked = [];
  const deployed = await deployedVersion('https://tilo.coffee/', {fetch: async (url, init) => {
    asked.push([String(url), init?.cache]);
    return json({version: NEW, revision: 'abc'});
  }});
  assert.equal(deployed, NEW);
  assert.deepEqual(asked, [['https://tilo.coffee/version.json', 'no-store']]);
  assert.equal(await deployedVersion('https://tilo.coffee/', {fetch: async () => missing()}), null);
  assert.equal(await deployedVersion('https://tilo.coffee/', {fetch: async () => json({version: '../x'})}), null);
  assert.equal(await deployedVersion('https://tilo.coffee/', {fetch: async () => { throw new TypeError('offline'); }}), null);
});

test('a file missing under a version the entry no longer serves offers the update, once at a time', async () => {
  const target = new EventTarget();
  const offers = [];
  let answer;
  const stop = watchForUpdate({target, fetch: async () => json({version: NEW}),
    offer: update => { offers.push(update); return new Promise(resolve => { answer = resolve; }); }});
  const url = `https://tilo.coffee/v/${OLD}/pkg/tilo_browser_runtime.calculator.wasm`;
  target.dispatchEvent(new CustomEvent(UNAVAILABLE, {detail: {url}}));
  // A module and the shared module it needs fail together: one offer.
  target.dispatchEvent(new CustomEvent(UNAVAILABLE, {detail: {url: url.replace('calculator', 'shared0')}}));
  await settle();
  assert.deepEqual(offers, [{running: OLD, deployed: NEW}]);
  // Declined: the next app that cannot open asks again.
  answer();
  await settle();
  target.dispatchEvent(new CustomEvent(UNAVAILABLE, {detail: {url}}));
  await settle();
  assert.equal(offers.length, 2);
  stop();
  target.dispatchEvent(new CustomEvent(UNAVAILABLE, {detail: {url}}));
  await settle();
  assert.equal(offers.length, 2);
});

test('a failure under the version still deployed, or offline, is no update', async () => {
  for (const fetch of [async () => json({version: OLD}), async () => { throw new TypeError('offline'); }]) {
    const target = new EventTarget();
    const checked = [];
    let offered = false;
    watchForUpdate({target, fetch, offer: () => { offered = true; }, onChecked: update => checked.push(update)});
    target.dispatchEvent(new CustomEvent(UNAVAILABLE, {detail: {url: `https://tilo.coffee/v/${OLD}/pkg/x.wasm`}}));
    await settle();
    assert.deepEqual(checked, [null]);
    assert.equal(offered, false);
  }
});

/**
 * The stale tab end to end, through the loader tools/web/wasm-split writes
 * into every split bundle: a page on version OLD opens an app it has not
 * loaded yet after the entry moved on to NEW.
 */
async function staleTab({served}) {
  const source = readFileSync(new URL('../../../../tools/web/wasm-split/src/main.rs', import.meta.url), 'utf8');
  const template = /const LOADER: &str = r#"([\s\S]*?)"#;/.exec(source)?.[1];
  assert.ok(template, 'the loader template is where the test reads it');
  // Real path: a module's import.meta.url is (macOS: /var is /private/var).
  const site = realpathSync(mkdtempSync(join(tmpdir(), 'bundle-update-')));
  const pkg = join(site, 'v', OLD, 'pkg');
  mkdirSync(pkg, {recursive: true});
  // As main.rs writes one split point after the template.
  writeFileSync(join(pkg, '__wasm_split.js'),
    `${template}export const load_calculator = loader("tilo_browser_runtime.calculator.wasm", []).export;\n`);
  const reports = [];
  writeFileSync(join(pkg, 'tilo_browser_runtime.js'), `
    const table = {get: () => (data, loaded) => globalThis.splitReports.push([data, loaded])};
    export function initSync() { return {memory: new WebAssembly.Memory({initial: 1}), __indirect_function_table: table}; }
  `);
  const page = new EventTarget();
  const fetched = [];
  const saved = {fetch: globalThis.fetch, dispatchEvent: globalThis.dispatchEvent, splitReports: globalThis.splitReports};
  globalThis.splitReports = reports;
  globalThis.dispatchEvent = event => page.dispatchEvent(event);
  globalThis.fetch = async url => {
    const href = String(url);
    fetched.push(href);
    if (href === pathToFileURL(join(site, 'version.json')).href) return json({version: NEW, revision: 'b'});
    if (href.includes(`/v/${OLD}/`) && served) {
      // The smallest valid module: the old version's file, still served.
      return new Response(new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]), {headers: {'content-type': 'application/wasm'}});
    }
    return missing();
  };
  try {
    const offers = [];
    const stop = watchForUpdate({target: page, offer: async update => { offers.push(update); }});
    const loader = await import(pathToFileURL(join(pkg, '__wasm_split.js')).href);
    loader.load_calculator(7, 'slot');
    for (let i = 0; i < 20 && (reports.length === 0 || (!served && offers.length === 0)); i++) await settle();
    stop();
    return {reports, offers, fetched, site};
  } finally {
    Object.assign(globalThis, saved);
    rmSync(site, {recursive: true, force: true});
  }
}

test('a stale tab whose app module is gone is offered the reload into the deployed version', async () => {
  const {reports, offers, fetched, site} = await staleTab({served: false});
  // The window says it failed, and the page is told why.
  assert.deepEqual(reports, [['slot', false]]);
  assert.deepEqual(offers, [{running: OLD, deployed: NEW}]);
  // It never reached for the new version's files: one version per page.
  assert.ok(fetched.every(url => !url.includes(`/v/${NEW}/`)), fetched.join('\n'));
  assert.ok(fetched.includes(pathToFileURL(join(site, 'version.json')).href));
});

test('a stale tab whose version is still served opens the app with no offer', async () => {
  const {reports, offers, fetched} = await staleTab({served: true});
  assert.deepEqual(reports, [['slot', true]]);
  assert.deepEqual(offers, []);
  assert.ok(fetched.every(url => !url.endsWith('/version.json')), fetched.join('\n'));
});

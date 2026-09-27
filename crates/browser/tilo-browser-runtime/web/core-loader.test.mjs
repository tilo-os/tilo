import test from 'node:test';
import assert from 'node:assert/strict';
import {SideModuleLinker} from './dylink.mjs';
import {importGlue, loadCore} from './core-loader.mjs';
import {CORE_DATA_VALUE, coreModule, sideModule} from './testing/wasm-modules.mjs';

// A fresh copy of the glue per test: its instance lives in module state.
let copies = 0;
const freshGlue = () => import(`./testing/bindgen-glue.mjs?copy=${++copies}`);

test('the ABI id comes from the module the glue instantiated, though the glue does not expose it', async () => {
  const glue = await freshGlue();
  const loaded = await loadCore({glue, module: coreModule({abi: 'tilo-runtime/2026.07+wasm.cafe'})});
  assert.equal(loaded.abi, 'tilo-runtime/2026.07+wasm.cafe');
  assert.equal(glue.loaded(), loaded.module, 'the glue ran the module that was compiled here');
  assert.equal(glue.default.__wbindgen_wasm_module, undefined);
  assert.equal(loaded.runtime, glue);
  // The raw exports are what side modules link against.
  const linker = new SideModuleLinker(loaded.exports, loaded.abi);
  const app = await linker.instantiate(await WebAssembly.compile(sideModule({abi: loaded.abi})), 'so.tilo.test');
  assert.equal(app.tilo_app_register(), CORE_DATA_VALUE + 1);
});

test('the core module may be a Response, bytes, a compiled Module or a URL', async () => {
  const bytes = coreModule();
  for (const module of [
    new Response(bytes, {headers: {'content-type': 'application/wasm'}}),
    bytes,
    await WebAssembly.compile(bytes),
  ]) {
    assert.equal((await loadCore({glue: await freshGlue(), module})).abi, 'test-abi');
  }
  const requested = [];
  const fetch = async url => {requested.push(String(url)); return new Response(bytes);};
  assert.equal((await loadCore({glue: await freshGlue(), module: 'https://cdn.test/core.wasm', fetch})).abi, 'test-abi');
  assert.deepEqual(requested, ['https://cdn.test/core.wasm']);
});

test('a glue without an init function, or a core without the section, is reported', async () => {
  await assert.rejects(loadCore({glue: {}, module: coreModule()}), /exports no init function/);
  assert.equal((await loadCore({glue: await freshGlue(), module: coreModule({abi: null})})).abi, undefined);
});

test('a glue from a URL is imported; a namespace passes through', async () => {
  const namespace = await freshGlue();
  assert.equal(await importGlue(namespace), namespace);
  const imported = await importGlue(new URL('./testing/bindgen-glue.mjs?copy=url', import.meta.url));
  assert.equal(typeof imported.default, 'function');
});

test('an untyped blob URL (a verified registry asset) is imported as JavaScript', async () => {
  const {readFile} = await import('node:fs/promises');
  const source = await readFile(new URL('./testing/bindgen-glue.mjs', import.meta.url));
  const untyped = URL.createObjectURL(new Blob([source], {type: 'application/octet-stream'}));
  // Node cannot import blob: URLs; a browser does, if the blob is typed.
  const imported = [];
  const importModule = async url => {
    const blob = await (await fetch(url)).blob();
    imported.push({url, type: blob.type});
    return import(`data:text/javascript;base64,${Buffer.from(await blob.arrayBuffer()).toString('base64')}`);
  };
  const glue = await importGlue(untyped, {importModule});
  assert.deepEqual(imported.map(entry => entry.type), ['text/javascript']);
  assert.notEqual(imported[0].url, untyped);
  assert.equal((await loadCore({glue, module: coreModule()})).abi, 'test-abi');
});

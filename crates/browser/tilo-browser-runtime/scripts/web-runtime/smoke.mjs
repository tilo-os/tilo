#!/usr/bin/env node
// smoke.mjs OUTPUT: wasm-level check of a build-web-dev.sh output in Node.
// Instantiates the core through its generated glue as host.mjs does
// (core-loader.mjs), links every app in
// apps/index.json with the page's loader, and checks that each registered
// into the core's catalogue, and that a module stamped with another ABI id is
// refused. No GPU or DOM is involved.
import {readFileSync} from 'node:fs';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import assert from 'node:assert/strict';

const out = process.argv[2];
const {SideModuleLinker, RuntimeAbiMismatch} = await import(pathToFileURL(join(out, 'dylink.mjs')).href);
const {loadCore} = await import(pathToFileURL(join(out, 'core-loader.mjs')).href);
const {addSection} = await import(new URL('./wasm-tools.mjs', import.meta.url).href);
// The page's own path: the generated, self-contained glue and loadCore.
const glue = await import(pathToFileURL(join(out, 'pkg/tilo_browser_runtime.js')).href);
const {exports: core, abi} = await loadCore({glue, module: readFileSync(join(out, 'pkg/tilo_browser_runtime_bg.wasm'))});
assert.equal(abi, JSON.parse(readFileSync(join(out, 'runtime.json'), 'utf8')).abi);
const linker = new SideModuleLinker(core, abi);
const before = JSON.parse(glue.applications()).map(app => app.id);
const index = JSON.parse(readFileSync(join(out, 'apps/index.json'), 'utf8'));
const report = {abi, compiledIn: before, linked: []};
for (const [id, manifestPath] of Object.entries(index.apps)) {
  const manifest = JSON.parse(readFileSync(join(out, manifestPath), 'utf8'));
  const bytes = readFileSync(join(out, manifestPath, '..', manifest.module.path));
  const started = performance.now();
  const module = await WebAssembly.compile(bytes);
  const exports = await linker.instantiate(module, id);
  assert.equal(exports.tilo_app_register(), 0, `${id} registered`);
  const described = JSON.parse(glue.application(id));
  assert.equal(described.id, id);
  report.linked.push({id, title: described.title, scope: described.scope, ms: Math.round(performance.now() - started)});
  assert.equal(exports.tilo_app_register(), 1, `${id} refuses a second registration`);
  const stale = await WebAssembly.compile(addSection(new Uint8Array(bytes), 'tilo.runtime_abi', new TextEncoder().encode(`${abi.split('+')[0]}+wasm.stale`)));
  await assert.rejects(linker.instantiate(stale, id), error => error instanceof RuntimeAbiMismatch);
}
report.installed = JSON.parse(glue.applications()).map(app => app.id);
assert.deepEqual(report.installed, [...before, ...Object.keys(index.apps)]);
console.log(JSON.stringify(report, null, 2));

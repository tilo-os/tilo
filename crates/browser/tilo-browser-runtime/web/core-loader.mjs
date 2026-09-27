import {compileModule, runtimeAbi} from './dylink.mjs';

/**
 * Instantiates the web core (WEB_RUNTIME.md) through its wasm-bindgen glue.
 *
 * The core module is compiled here, not by the glue, so its runtime ABI id is
 * read from the module that actually runs, with no reliance on glue
 * internals. `glue` is the glue's module namespace: an imported
 * `pkg/tilo_browser_runtime.js`, or the same self-contained file loaded from a
 * verified source. `module` is what to compile: a WebAssembly.Module, a
 * Response (stream-compiled), bytes, or a URL fetched here.
 *
 * Resolves to `{runtime, exports, module, abi}`: `runtime` the glue
 * namespace, `exports` the core's raw instance exports (the glue returns them
 * because it was generated with --keep-lld-exports), `abi` undefined for a
 * core built without the ABI section (the Cloud build).
 */
export async function loadCore({glue, module, fetch: fetchResource = globalThis.fetch}) {
  if (typeof glue?.default !== 'function') throw TypeError('the core glue exports no init function');
  let source = await module;
  if (typeof source === 'string' || source instanceof URL) {
    source = await fetchResource(source, {credentials: 'same-origin'});
  }
  const compiled = await compileModule(source);
  const exports = await glue.default({module_or_path: compiled});
  return {runtime: glue, exports, module: compiled, abi: runtimeAbi(compiled)};
}

/**
 * The glue's namespace from a URL (a verified asset's object URL, say) or an
 * already imported namespace. A URL must name the self-contained glue
 * build-web-core.sh writes: it imports nothing by relative path.
 */
export async function importGlue(glue, {fetch: fetchResource = globalThis.fetch, importModule = url => import(url)} = {}) {
  if (!(typeof glue === 'string' || glue instanceof URL)) return glue;
  const url = String(glue);
  if (!url.startsWith('blob:')) return importModule(url);
  // A verified asset's blob carries no JavaScript type, which module imports
  // require; re-wrap the same bytes as one.
  const source = await (await fetchResource(url)).blob();
  const typed = URL.createObjectURL(new Blob([source], {type: 'text/javascript'}));
  try {return await importModule(typed);} finally {URL.revokeObjectURL(typed);}
}

// Page host for the Tilo browser runtime: the wasm module the entry pages
// sign in, admit and pair with, and its profile storage.
import {retryableMemo} from './asset-recovery.mjs';
import init,* as runtime from './pkg/tilo_browser_runtime.js';
import {createProfileStorage} from './peer-files.mjs';
// The entry page started compiling the start module as it parsed its head
// (compile.inline.js), while it downloaded and the page's modules loaded:
// init takes that, once. A page without it, or whose compile failed, has
// init fetch and compile the module itself, as a retry after a failure does.
let startModule=globalThis.__tiloStartModule;
const ready=retryableMemo(async()=>{
  const early=startModule;startModule=undefined;
  const module=early&&await early.catch(()=>null);
  return init(module?{module_or_path:module}:undefined);
});
// Streaming compile starts on import, while the page reads its entry.
void ready().catch(()=>{});
const profileStorage=createProfileStorage({runtime});
/** Open the store a profile's certified peer keeps its state in. `flush()`
 * resolves when writes are durable; call `release()` once. The store's code is
 * the peer module's: a page that keeps a profile and opens its store is
 * entering, so it is loaded here. */
export async function openProfileStorage(profile,device){await loadRuntime('peer');return profileStorage.open(profile,device);}
/** The code a bundle carries as modules of its own (`../src/split.rs`), which
 * a page loads when it first needs one: the start module answers those exports
 * from the module once it is in. A whole bundle (a development build) has no
 * loader and every export in the start module already. */
const splitCode=retryableMemo(async()=>{await ready();return import('./pkg/__wasm_split.js').catch(()=>null);});
/** The wasm runtime module's start code: the link grammar, the workspaces this
 * browser entered, and its device's tickets. With `part`, its module `name`
 * (`peer`, `accounts`) is loaded first: a bundle carries that code as a module
 * of its own (`../src/split.rs`), and the start module answers its exports
 * from it. A whole bundle (a development build) has no loader and every
 * export in the start module already. */
export async function loadRuntime(part){await ready();if(part) await (await splitCode())?.load(part);return runtime;}

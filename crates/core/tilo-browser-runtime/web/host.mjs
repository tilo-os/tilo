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
 * resolves when writes are durable; call `release()` once. */
export async function openProfileStorage(profile,device){await ready();return profileStorage.open(profile,device);}
/** The certified peer's code is a module of its own when a bundle splits it
 * (`lazy_exports!`): fetched as soon as this module runs, beside the start
 * module's compile, and every entry waits for it. */
const peerCode=retryableMemo(async()=>{
  const split=await import('./pkg/__wasm_split.js').catch(()=>null);
  // Downloading and compiling it needs nothing of the start module: it runs
  // while the start module compiles, and only its instantiation waits.
  void split?.prefetch('peer');
  await ready();
  await split?.load('peer');
});
void peerCode().catch(()=>{});
/** The wasm runtime module, once loaded with the peer's code (its codec and
 * certificates). */
export async function loadRuntime(){await peerCode();return runtime;}

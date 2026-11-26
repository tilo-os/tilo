// Page host for the Tilo browser runtime: the wasm module the entry pages
// sign in, admit and pair with, and its profile storage.
import {retryableMemo} from './asset-recovery.mjs';
import init,* as runtime from './pkg/tilo_browser_runtime.js';
import {acquireStorage} from './storage.mjs';
import {createDurableHost} from './durable-host.mjs';
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
const durableHost=createDurableHost({runtime,acquireStorage});
/** Open a profile's durable storage for state kept outside any mount (the
 * certified peer). `flush()` resolves when writes are durable; call
 * `release()` once. */
export async function openProfileStorage(profile,device,options={}){await ready();return durableHost.open(profile,device,options);}
/** The entry module includes the certified peer, its codec and its certificates. */
export async function loadRuntime(){await ready();return runtime;}

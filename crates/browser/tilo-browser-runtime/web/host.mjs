// Page host for the Tilo browser runtime: the wasm module the entry pages
// sign in, admit and pair with, its profile storage and the record verifier.
import {retryableMemo} from './asset-recovery.mjs';
import {createRecordVerifier} from './record-verify.mjs';
import init,* as runtime from './pkg/tilo_browser_runtime.js';
import {acquireStorage} from './storage.mjs';
import {createDurableHost} from './durable-host.mjs';
/** Where the page reports what it did not catch, for the entry it came in
 * through to log (`entry-page.mjs`). A panic is reported the same way. */
let pageErrorReport=null;
export function reportPageErrors(report){pageErrorReport=report;}
// The runtime's panic hook hands over `{message}`: the module is dead after
// it, and the entry's log hears why.
globalThis.__tiloRuntimePanic=raw=>{
  let message=raw;try{message=JSON.parse(raw)?.message??raw;}catch{}
  pageErrorReport?.('panic',message);
};
// The entry page started compiling the start module as it parsed its head
// (compile.inline.js), while it downloaded and the page's modules loaded:
// init takes that, once. A page without it, or whose compile failed, has
// init fetch and compile the module itself, as a retry after a failure does.
let startModule=globalThis.__tiloStartModule;
/** The compiled module, kept for the record worker's own instance. */
let compiledModule;
const ready=retryableMemo(async()=>{
  const early=startModule;startModule=undefined;
  const module=early&&await early.catch(()=>null);
  compiledModule=module||undefined;
  return init(module?{module_or_path:module}:undefined);
});
// Large records' changes are verified in a worker, not on this thread
// (record-verify.mjs); the runtime hands them over through this.
globalThis.__tiloVerifyRecord=createRecordVerifier({module:()=>compiledModule});
// Streaming compile starts on import, while the page reads its entry.
void ready().catch(()=>{});
const durableHost=createDurableHost({runtime,acquireStorage});
/** Open a profile's durable storage for state kept outside any mount (the
 * certified peer). `flush()` resolves when writes are durable; call
 * `release()` once. */
export async function openProfileStorage(profile,device,options={}){await ready();return durableHost.open(profile,device,options);}
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

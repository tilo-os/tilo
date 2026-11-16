// Verifies large records' changes off the page's main thread
// (web/record-verify.mjs): the page's own module, instantiated here, loads
// each record as the page would and answers the hash of every change.
import {initSync,verifiedChangeHashes} from './pkg/tilo_browser_runtime.js';

let ready;
async function start(module){
  // A split bundle's peer code is a module of its own: loaded here as the
  // page loads it, before anything calls into it.
  const split=await import('./pkg/__wasm_split.js').catch(()=>null);
  const compiled=module??await WebAssembly.compileStreaming(fetch(new URL('./pkg/tilo_browser_runtime_bg.wasm',import.meta.url)));
  initSync({module:compiled});
  await split?.load('peer');
}

self.onmessage=async({data})=>{
  if(!('id' in data)){ready??=start(data.module);return;}
  try{
    await ready;
    const hashes=verifiedChangeHashes(data.bytes);
    self.postMessage({id:data.id,hashes},[hashes.buffer]);
  }catch(error){
    self.postMessage({id:data.id,error:String(error?.message??error)});
  }
};

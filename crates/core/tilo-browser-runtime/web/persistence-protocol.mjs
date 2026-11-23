/** Page ↔ persistence worker protocol. Every request carries a page-chosen
 * `id`; the worker answers exactly once per id.
 *
 *   {id,type:'open',profile,device}              → {namespace,epoch}
 *   {id,type:'hydrate',namespace}                → rows (bytes transferred)
 *   {id,type:'commit',namespace,writes,abortNext,options?:{transfer}}→ undefined once the strict transaction completed
 *   {id,type:'transfer',namespace,method,args}  → bounded open/status/read result or undefined (write/drop)
 *       methods list/open/status/write/read/drop/compareAndSwapResumeDigest; write/read move <=240KiB buffers.
 *       CAS(handle,expectedDigest,nextDigest) returns inert status after a strict atomic
 *       current-lease/current-digest check; <=4096 UTF-8 bytes per opaque digest.
 *       list/open/status include resumeDigest (old records default to immutable cutToken).
 *       No method validates a wire manifest or acknowledges native admission.
 *   {id,type:'release',namespace}                → undefined once the writer lock is released
 *   {id,type:'flush',namespace?}                 → undefined once queued work for the namespace(s) settled
 *
 *   reply: {id,ok:true,result} | {id,ok:false,error:{name,message}}
 *
 * Errors keep their `name` so `QuotaExceededError`/`SecurityError` map to the
 * same failure codes on the page as an in-page transaction would. */
export const STORAGE_UNSUPPORTED='StorageUnsupportedError';
export const STORAGE_WORKER_FAILED='StorageWorkerError';
export const unsupported=message=>Object.assign(Error(message),{name:STORAGE_UNSUPPORTED});
export const workerFailure=detail=>Object.assign(Error(`storage worker failed${detail?`: ${detail}`:''}`),{name:STORAGE_WORKER_FAILED});
export const serializeError=error=>({name:typeof error?.name==='string'&&error.name?error.name:'Error',message:typeof error?.message==='string'?error.message:String(error)});
export const reviveError=(error={})=>Object.assign(Error(typeof error.message==='string'?error.message:'storage worker error'),{name:typeof error.name==='string'&&error.name?error.name:'Error'});
const transferable=buffer=>buffer instanceof ArrayBuffer&&buffer.detached!==true;
/** Prepare byte-carrying rows for postMessage: whole-buffer views move without
 * a copy, everything else (JSON number arrays, shared or partial views) is
 * copied into a fresh buffer that moves instead. */
export function packBinary(rows){
  const transfer=[],seen=new Set();
  const view=value=>{
    const bytes=value instanceof Uint8Array?value:new Uint8Array(value);
    const whole=bytes.byteOffset===0&&bytes.byteLength===bytes.buffer.byteLength;
    if(whole&&transferable(bytes.buffer)&&!seen.has(bytes.buffer)){seen.add(bytes.buffer);transfer.push(bytes.buffer);return bytes;}
    const copy=new Uint8Array(bytes);seen.add(copy.buffer);transfer.push(copy.buffer);return copy;
  };
  return {rows:rows.map(row=>({...row,bytes:view(row.bytes),actor:row.actor?view(row.actor):null})),transfer};
}

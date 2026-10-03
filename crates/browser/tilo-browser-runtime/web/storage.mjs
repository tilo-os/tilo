import {transactionComplete} from './host-core.mjs';
import {createInlineStorage,requestResult,storageRevision} from './storage-inline.mjs';
import {STORAGE_UNSUPPORTED,STORAGE_WORKER_FAILED,packBinary,reviveError,workerFailure} from './persistence-protocol.mjs';
import {captureTransferHandle,captureTransferRequest} from './transfer-staging.mjs';
export {storageRevision};
const LOCK_CONTENDED=/already open in another browser tab/;
function defaultCreateWorker(){
  if(typeof Worker!=='function')return null;
  return new Worker(new URL('./persistence-worker.mjs',import.meta.url),{type:'module'});
}
/** Request/reply bridge over one Worker. Death (error event, undeliverable
 * message, explicit kill) rejects every in-flight request with a
 * StorageWorkerError and terminates the worker so its locks are released. */
function createBridge(worker,onDeath){
  const pending=new Map();let next=0,dead=null;
  const die=error=>{
    if(dead)return;dead=error;
    for(const request of pending.values())request.reject(error);pending.clear();
    try{worker.terminate();}catch{}
    onDeath(error);
  };
  worker.addEventListener('message',event=>{
    const {id,ok,result,error}=event.data??{};const request=pending.get(id);if(!request)return;
    pending.delete(id);ok?request.resolve(result):request.reject(reviveError(error));
  });
  worker.addEventListener('error',event=>die(workerFailure(event.message)));
  worker.addEventListener('messageerror',()=>die(workerFailure('undeliverable message')));
  return {
    request(message,transfer=[]){
      if(dead)return Promise.reject(dead);
      const id=++next;
      return new Promise((resolve,reject)=>{
        pending.set(id,{resolve,reject});
        try{worker.postMessage({...message,id},transfer);}catch(error){pending.delete(id);reject(error);}
      });
    },
    kill:die,
  };
}
/** Storage for the page: leases proxy to the persistence worker when one can
 * run, otherwise (or after the worker dies) they run the in-page lease. The
 * ordinary lease interface is preserved: `epoch`, `fresh`, `abortNext`,
 * `hydrate()`, `commit(writes)`, `release()`. Private `transfers` add bounded
 * staging; `commit(writes,{transfer})` consumes a complete stage in the same
 * confirmed strict commit. Neither path is a network ACK or admission proof. */
const warnFallback=error=>console.warn('Tilo storage: persistence worker unavailable, continuing with in-page IndexedDB transactions',error);
export function createStorageAccess({createWorker=defaultCreateWorker,inline,events=globalThis,delay=ms=>new Promise(resolve=>setTimeout(resolve,ms)),lockRetries=20,lockRetryMs=50,onFallback=warnFallback}={}){
  const leases=new Map(),live=new Set(),queues=new WeakMap();
  let bridge=null,inlineOnly=false,inlineStorage=inline;
  const inlineAccess=()=>inlineStorage??=createInlineStorage();
  const onPageHide=()=>{void flush().catch(()=>{});};
  function connectWorker(){
    if(inlineOnly||bridge)return bridge;
    let worker;
    try{worker=createWorker();}catch(error){fallBack(error);return null;}
    if(!worker){inlineOnly=true;return null;}
    bridge=createBridge(worker,fallBack);
    events?.addEventListener?.('pagehide',onPageHide);
    return bridge;
  }
  /** Rest-of-session fallback: reject in-flight worker requests (their
   * callers report storage-transaction-failed and keep the writes pending),
   * then move every open lease to an in-page lease with the same epoch so the
   * runtime's epoch pin and the same-epoch revision guard stay valid. */
  function fallBack(error){
    if(inlineOnly)return;inlineOnly=true;
    const failed=bridge;bridge=null;
    events?.removeEventListener?.('pagehide',onPageHide);
    failed?.kill(error);
    onFallback(error);
    for(const lease of live)lease.migrate();
  }
  async function acquireInlineRetrying(profile,device,epoch){
    // A terminated worker releases its lock asynchronously; wait for it briefly.
    for(let attempt=0;;attempt++){
      try{return await inlineAccess().acquireStorage(profile,device,{epoch});}
      catch(error){if(!LOCK_CONTENDED.test(String(error?.message))||attempt>=lockRetries)throw error;await delay(lockRetryMs);}
    }
  }
  const inlineBackend=lease=>({kind:'inline',epoch:lease.epoch,
    hydrate:()=>lease.hydrate(),
    commit:(writes,abortNext,options)=>{if(abortNext)lease.abortNext=true;return lease.commit(writes,options);},
    staging:(method,args)=>lease.transfers[method](...args),
    release:()=>lease.release(),
  });
  const workerBackend=(link,namespace,epoch)=>({kind:'worker',epoch,
    hydrate:()=>link.request({type:'hydrate',namespace}),
    commit(writes,abortNext,options){const {rows,transfer}=packBinary(writes);return link.request({type:'commit',namespace,writes:rows,abortNext,...(options?{options}:{})},transfer);},
    staging(method,args){const transfer=method==='write'?[args[2].buffer]:[];return link.request({type:'transfer',namespace,method,args},transfer);},
    // A dead worker has already lost its lock; nothing is left to release.
    release:()=>link.request({type:'release',namespace}).catch(error=>{if(error?.name!==STORAGE_WORKER_FAILED)throw error;}),
  });
  async function openBackend(profile,device,namespace){
    const link=connectWorker();
    if(link){
      try{const {epoch}=await link.request({type:'open',profile,device});return workerBackend(link,namespace,epoch);}
      catch(error){
        if(error?.name!==STORAGE_UNSUPPORTED&&error?.name!==STORAGE_WORKER_FAILED)throw error;
        fallBack(error);
        return inlineBackend(await acquireInlineRetrying(profile,device,undefined));
      }
    }
    return inlineBackend(await inlineAccess().acquireStorage(profile,device));
  }
  async function acquireStorage(profile,device){
    const namespace=JSON.stringify([profile,device]);
    if(leases.has(namespace)){
      const lease=await leases.get(namespace);
      if(lease.closing){await lease.closing;return acquireStorage(profile,device);}
      lease.references++;return lease;
    }
    const pending=(async()=>{
      const opened=await openBackend(profile,device,namespace);
      let backend=Promise.resolve(opened),queue=Promise.resolve(),pendingStages=0;
      const enqueue=(task,staging=false,prepare=()=>undefined)=>{
        if(lease.closing)return Promise.reject(Error('Storage lease is closing'));
        if(staging&&pendingStages>=2)return Promise.reject(Object.assign(Error('Transfer staging has two pending operations; await completion'),{name:'QuotaExceededError'}));
        let captured;try{captured=prepare();}catch(error){return Promise.reject(error);}
        if(staging)pendingStages++;
        const result=queue.then(async()=>task(await backend,captured));queue=result.then(()=>{},()=>{});
        return staging?result.finally(()=>{pendingStages--;}):result;
      };
      const lease={namespace,epoch:opened.epoch,references:1,fresh:true,abortNext:false,closing:undefined,
        get mode(){return inlineOnly?'inline':'worker';},
        hydrate(){return enqueue(current=>current.hydrate());},
        commit(writes,{transfer}={}){
          if(!writes.length&&transfer===undefined)return Promise.resolve();
          return enqueue((current,{abortNext,options})=>current.commit(writes,abortNext,options),false,()=>{
            const options=transfer===undefined?undefined:{transfer:captureTransferHandle(transfer)};
            const abortNext=lease.abortNext;lease.abortNext=false;return {abortNext,options};
          });
        },
        async release(){
          if(lease.closing)return lease.closing;
          if(--lease.references>0)return;
          lease.closing=(async()=>{try{await queue;await (await backend).release();}finally{live.delete(lease);leases.delete(namespace);}})();
          return lease.closing;
        },
        migrate(){
          // A closing lease has nothing left to move; re-acquiring would leak the lock.
          if(lease.closing)return;
          backend=backend.then(current=>current.kind==='worker'?acquireInlineRetrying(profile,device,lease.epoch).then(inlineBackend):current);
          void backend.catch(()=>{});
        },
      };
      lease.transfers=Object.fromEntries(['list','open','status','write','read','drop','compareAndSwapResumeDigest'].map(method=>[method,(...args)=>enqueue((current,captured)=>current.staging(method,captured),true,()=>captureTransferRequest(method,args))]));
      queues.set(lease,()=>queue);
      live.add(lease);
      return lease;
    })();
    leases.set(namespace,pending);
    try{return await pending;}catch(error){leases.delete(namespace);throw error;}
  }
  /** Ask the worker to settle everything queued so far (page unload posts this). */
  function flush(){const pending=[...live].map(lease=>queues.get(lease)());if(bridge)pending.push(bridge.request({type:'flush'}));return Promise.all(pending).then(()=>{});}
  return {acquireStorage,flush,mode:()=>inlineOnly?'inline':'worker'};
}
const access=createStorageAccess();
export const acquireStorage=access.acquireStorage;

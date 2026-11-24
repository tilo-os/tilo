// The page's profile stores (`src/peer_store.rs`): what the page's certified peer keeps between
// sessions and the page's own values, in IndexedDB (`peer-files-db.mjs`). The runtime holds a
// store's files in memory and hands each commit, packed, to a worker that writes it in one strict
// transaction (`peer-files-worker.mjs`); a change counts as saved once that transaction completed.
import {servePeerFiles} from './peer-files-worker.mjs';

function defaultCreateWorker(){
  if(typeof Worker!=='function')return null;
  return new Worker(new URL('./peer-files-worker.mjs',import.meta.url),{type:'module'});
}

/** Resolves once the page ran what waits for it: input, painting, the next task. A message, not a
 * timer, so a chain of pauses is never held back. */
function pause(){
  return new Promise(resolve=>{
    const {port1,port2}=new MessageChannel();
    port1.onmessage=()=>{port1.close();resolve();};
    port2.postMessage(null);
  });
}

/** Requests to the worker, or where no worker runs, to the same requests served on the page. */
function connect(createWorker,{indexedDB,IDBKeyRange}){
  let port=createWorker?.();
  if(!port){
    const channel=new MessageChannel();
    servePeerFiles(channel.port2,{indexedDB,IDBKeyRange});
    port=channel.port1;
  }
  const pending=new Map();let next=0;
  const failAll=error=>{for(const {reject} of pending.values())reject(error);pending.clear();};
  port.addEventListener('message',({data:{id,ok,result,error}})=>{
    const waiting=pending.get(id);if(!waiting)return;pending.delete(id);
    if(ok)waiting.resolve(result);else waiting.reject(Object.assign(Error(error.message),{name:error.name}));
  });
  port.addEventListener('error',event=>failAll(Object.assign(Error(`the profile's storage worker stopped: ${event?.message??'it did not load'}`),{name:'UnknownError'})));
  port.start?.();
  return {
    request(message,transfer=[]){
      const id=++next;
      return new Promise((resolve,reject)=>{
        pending.set(id,{resolve,reject});
        try{port.postMessage({...message,id},transfer);}catch(error){pending.delete(id);reject(error);}
      });
    },
  };
}

/** The peer stores the page opened, one per profile namespace, shared by everything on the page
 * that opens one. `open(profile, device)` resolves to a handle: `flush()` resolves once every
 * change made before it is committed, `subscribe(listener)` hears each commit that failed and the
 * one that succeeded after it, and every open needs one `release()`, the last of
 * which flushes and closes the store. Leaving the page or hiding it flushes every store.
 * `runtime` is the wasm module. */
export function createProfileStorage({runtime,indexedDB=globalThis.indexedDB,IDBKeyRange=globalThis.IDBKeyRange,events=globalThis,createWorker=defaultCreateWorker}){
  const opened=new Map();
  let connection;
  const files=()=>connection??=connect(createWorker,{indexedDB,IDBKeyRange});
  const flushAll=()=>Promise.allSettled([...opened.values()].map(async pending=>(await pending).flush()));
  events?.addEventListener?.('pagehide',()=>void flushAll());
  events?.document?.addEventListener?.('visibilitychange',()=>{if(events.document.visibilityState!=='visible')void flushAll();});
  async function start(profile,device,namespace){
    const rows=await files().request({type:'open',namespace});
    let status={persistence:'committed',error:null},refs=0,closing;
    const listeners=new Set();
    const publish=next=>{status=next;for(const listener of listeners)listener(status);};
    const commit=async packed=>{
      try{await files().request({type:'commit',namespace,packed},[packed.buffer]);}
      catch(error){publish({persistence:'failed',error:String(error)});throw error;}
      if(status.persistence!=='committed')publish({persistence:'committed',error:null});
    };
    try{runtime.open_peer_store(profile,device,rows,commit,pause);}
    catch(error){await files().request({type:'close',namespace});throw error;}
    const flush=()=>runtime.flush_peer_store(profile,device);
    const shared={
      get status(){return status;},
      get closing(){return closing;},
      flush,
      subscribe(listener){listeners.add(listener);return ()=>listeners.delete(listener);},
      retain(){refs++;return shared;},
      release(){
        if(closing)return closing;
        if(--refs>0)return Promise.resolve();
        closing=(async()=>{
          try{await flush();runtime.close_peer_store(profile,device);}
          catch(error){refs++;closing=undefined;throw error;}
          opened.delete(namespace);
          await files().request({type:'close',namespace});
        })();
        return closing;
      },
    };
    return shared;
  }
  async function open(profile,device){
    const namespace=JSON.stringify([profile,device]);
    if(!opened.has(namespace)){
      const pending=start(profile,device,namespace);
      opened.set(namespace,pending);
      pending.catch(()=>{if(opened.get(namespace)===pending)opened.delete(namespace);});
    }
    const shared=await opened.get(namespace);
    if(shared.closing){await shared.closing.catch(()=>{});return open(profile,device);}
    return shared.retain();
  }
  return {open,flush:flushAll};
}

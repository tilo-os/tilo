// A profile's durable storage, shared by everything on the page that keeps
// state in it: mounted applications, the relay peer, settings. One namespace
// (profile/device) holds one writer lease (Web Lock + IndexedDB, see
// storage.mjs) and one Rust store for as long as anyone has it open.
//
// Rust tells the page of each accepted write through the store's write hook;
// the page drains, commits in a strict transaction and acknowledges each
// revision. A write counts as saved only after that commit: `flush()`
// resolves once everything accepted before it is durable, and rejects if a
// commit failed (a later flush retries the failed keys).

const namespaceKey=(profile,device)=>JSON.stringify([profile,device]);

/** `runtime` is the wasm module (load_storage, drain_storage, retry_storage,
 * storage_write_hook, acknowledge_storage, refresh_storage_lease,
 * provide_settings); `acquireStorage` is storage.mjs's lease factory. */
export function createDurableHost({runtime,acquireStorage,schedule=queueMicrotask,events=globalThis}){
  const namespaces=new Map();
  let listening=false;
  const flushAll=()=>Promise.allSettled([...namespaces.values()].map(async pending=>(await pending).flush()));
  const onHidden=()=>{if(events.document?.visibilityState!=='visible')void flushAll();};
  function listen(){
    if(listening||!events?.addEventListener)return;listening=true;
    events.addEventListener('pagehide',onHidden);
    events.document?.addEventListener?.('visibilitychange',onHidden);
  }
  async function start(profile,device){
    const lease=await acquireStorage(profile,device);
    try{
      if(lease.fresh){runtime.refresh_storage_lease(profile,device);lease.fresh=false;}
      runtime.load_storage(profile,device,await lease.hydrate());
    }catch(error){await lease.release();throw error;}
    let chain=Promise.resolve(),scheduled=false,failed=false,refs=0,closing;
    let status={persistence:'committed',pendingWrites:0,error:null};
    const listeners=new Set();
    const publish=next=>{status=next;for(const listener of listeners)listener(status);};
    const acknowledge=(writes,failure)=>{for(const write of writes)runtime.acknowledge_storage(profile,device,BigInt(write.revision),failure);};
    async function commitNow(writes){
      if(!writes.length)return;
      publish({persistence:'pending',pendingWrites:writes.length,error:null});
      try{await lease.commit(writes);}
      catch(error){
        failed=true;acknowledge(writes,String(error));
        publish({persistence:'failed',pendingWrites:writes.length,error:String(error)});
        throw error;
      }
      acknowledge(writes,undefined);failed=false;
      publish({persistence:'committed',pendingWrites:0,error:null});
    }
    const enqueue=task=>{const result=chain.then(task);chain=result.catch(()=>{});return result;};
    const handle={
      profile,device,lease,
      get status(){return status;},
      /** Stored rows as IndexedDB holds them now (`[{key,bytes,actor}]`). */
      hydrate:()=>lease.hydrate(),
      /** Commit writes already drained elsewhere (a mount's close). */
      commit:writes=>enqueue(()=>commitNow(writes)),
      /** Resolve once every write accepted so far is durable, retrying keys
       * whose commit failed. */
      flush:()=>enqueue(()=>{
        if(failed)runtime.retry_storage(profile,device);
        return commitNow(Array.from(runtime.drain_storage(profile,device)));
      }),
      subscribe(listener){listeners.add(listener);return ()=>listeners.delete(listener);},
      /** Drop one reference. The last flushes and gives up the lease; if that
       * flush fails the namespace keeps custody and release may be retried. */
      async release(){
        if(closing)return closing;
        if(--refs>0)return;
        closing=(async()=>{
          try{await handle.flush();}
          catch(error){refs++;closing=undefined;throw error;}
          runtime.storage_write_hook(profile,device,undefined);
          namespaces.delete(namespaceKey(profile,device));
          await lease.release();
        })();
        return closing;
      },
      get closing(){return closing;},
      retain(){refs++;return handle;},
    };
    // Only new writes: a failed key is retried by an explicit flush, or by
    // its owner's next write (which the store widens to a replacement).
    const commitAccepted=()=>enqueue(()=>commitNow(Array.from(runtime.drain_storage(profile,device))));
    runtime.storage_write_hook(profile,device,()=>{
      if(scheduled)return;scheduled=true;
      schedule(()=>{scheduled=false;void commitAccepted().catch(()=>{});});
    });
    listen();
    return handle;
  }
  /** Open (or share) a namespace. `settings` seeds a profile with no stored
   * settings document. Every open needs one `release()`. */
  async function open(profile,device,{settings}={}){
    const key=namespaceKey(profile,device);
    if(!namespaces.has(key)){
      const pending=start(profile,device);
      namespaces.set(key,pending);
      pending.catch(()=>{if(namespaces.get(key)===pending)namespaces.delete(key);});
    }
    const shared=await namespaces.get(key);
    if(shared.closing){await shared.closing.catch(()=>{});return open(profile,device,{settings});}
    shared.retain();
    try{if(settings!==undefined)runtime.provide_settings(profile,device,JSON.stringify(settings));}
    catch(error){await shared.release();throw error;}
    return shared;
  }
  return {open,flush:flushAll};
}

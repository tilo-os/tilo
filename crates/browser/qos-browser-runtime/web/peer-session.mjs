import {commitPeerWrites} from './peer-persistence.mjs';
const recoveryOwners=new Set();
export const pendingPeerRecoveryCount=()=>recoveryOwners.size;
export async function retryPendingPeerRecovery(){const results=await Promise.allSettled([...recoveryOwners].map(close=>close()));const failure=results.find(result=>result.status==='rejected');if(failure)throw failure.reason;}
export function createConnectPeer({init,runtime,mountDesktop,acquireStorage,locks=navigator.locks,fetchEnrollment=fetch}){
let initialized;
async function leaseIdentity(slots,signal){
  if(!locks)throw Error('Certified actor custody requires Web Locks');
  for(const slot of slots){
    if(signal?.aborted)throw signal.reason??new DOMException('Aborted','AbortError');
    let resolve,reject,release;
    const acquired=new Promise((yes,no)=>{resolve=yes;reject=no;});
    const held=new Promise(yes=>{release=yes;});
    const task=locks.request(`qos-certified-peer:${slot}`,{ifAvailable:true,mode:'exclusive'},async lock=>{resolve(Boolean(lock));if(lock)await held;});
    task.catch(reject);
    if(await acquired)return {slot,release:async()=>{release();await task;}};
    await task;
  }
  throw Error('All certified browser identities are in use; close another session before connecting');
}
return async function connectPeer(options={}){
  const {enrollmentBaseUrl='/qos-peer/enrollment',identitySlots=['browser-a','browser-b'],signal}=options;
  const lease=await leaseIdentity(identitySlots,signal);
  let id,closed=false,closing=false,snapshot={phase:'authenticating',records:[]},connection,storage,namespace,raw,flush=Promise.resolve(),failed=false,generation=0;
  const subscribers=new Set(),mounts=new Set(),failedMounts=new Set(),pendingMounts=new Set(),mountControllers=new Set();
  let reconnecting,closePromise;
  const report=next=>{snapshot={...snapshot,...next};options.onStatus?.(snapshot);for(const listener of subscribers)listener(snapshot);};
  async function persist(publish=true){
    if(id===undefined||!storage||closed||failed)return;
    flush=flush.catch(()=>{}).then(async()=>{
      if(closed||failed)return;
      try{
        // The journal (including replay floors/outbox) and all canonical records
        // share this one strict IDB transaction. Network publication follows ack.
        await commitPeerWrites({pending:()=>JSON.parse(runtime.peer_pending_writes(id)),commit:writes=>storage.commit(writes),acknowledge:write=>runtime.acknowledge_storage(namespace.profile,namespace.device,BigInt(write.revision),undefined),publish:()=>{if(publish)runtime.peer_committed(id);}});
      }catch(error){failed=true;report({phase:'incomplete',error:String(error),persistence:'failed'});throw error;}
    });
    return flush;
  }
  const update=value=>{if(closed||closing)return;let next=JSON.parse(value);if(failed)next={...next,phase:'incomplete',error:snapshot.error,persistence:'failed'};report(next);if(next.persistenceNeeded)void persist().catch(()=>{});};
  const connect=async(old)=>{
    const current=++generation;
    connection=runtime.connect_peer(raw,await storage.hydrate(),value=>queueMicrotask(()=>{if(current===generation)update(value);}));
    const next=await connection;
    if(closed||closing){await runtime.close_peer(next);return;}
    id=next;
    if(old!==undefined){runtime.rebind_peer(old,next);await runtime.close_peer(old);}
    await persist();
  };
  async function settleMounts(items){
    const results=await Promise.allSettled([...items].map(mount=>mount.cleanup()));
    const failure=results.find(result=>result.status==='rejected');
    if(failure)throw failure.reason;
  }
  function close(){
    if(closed)return Promise.resolve();
    if(closePromise)return closePromise;
    closing=true;
    closePromise=(async()=>{
      for(const controller of mountControllers)controller.abort();
      await Promise.allSettled([...pendingMounts]);
      await reconnecting?.catch(()=>{});
      await connection?.catch(()=>{});
      // Stop the replaceable endpoint even when component settlement retains
      // custody. Keep its durable owner addressable for the next retry.
      if(id!==undefined)await runtime.disconnect_peer(id);
      await settleMounts(mounts);
      failed=false;await persist(false);
      generation++;if(id!==undefined){await runtime.close_peer(id);id=undefined;}
      await storage?.release();await lease.release();closed=true;recoveryOwners.delete(close);report({phase:'closed',error:null});
    })().catch(error=>{recoveryOwners.add(close);report({phase:'incomplete',error:String(error),recoveryAvailable:true});throw error;}).finally(()=>{closing=false;closePromise=undefined;});
    return closePromise;
  }
  function reconnect(){
    if(closed||closing)return Promise.reject(Error('Session closed'));
    if(reconnecting)return reconnecting;
    reconnecting=(async()=>{
      const old=id;
      if(old!==undefined)runtime.suspend_peer(old,true);
      failed=false;await persist(false);
      if(old!==undefined)await runtime.disconnect_peer(old);
      generation++;report({phase:'authenticating',error:null});
      if(closing)return;
      try{await connect(old);}catch(error){report({phase:'failed',error:String(error)});throw error;}
    })().finally(()=>{reconnecting=undefined;});
    return reconnecting;
  }
  try{
    await (initialized??=init());
    const response=await fetchEnrollment(`${enrollmentBaseUrl}/${encodeURIComponent(lease.slot)}`,{signal,cache:'no-store',credentials:'same-origin'});
    if(!response.ok)throw Error(`Enrollment HTTP ${response.status}`);
    // Signed u64 timestamps must never pass through JS numeric JSON decoding.
    raw=await response.text();namespace=JSON.parse(runtime.peer_enrollment(raw));
    storage=await acquireStorage(namespace.profile,namespace.device);
    if(storage.fresh){runtime.refresh_storage_lease(namespace.profile,namespace.device);storage.fresh=false;}
    signal?.addEventListener('abort',()=>{void close().catch(error=>report({phase:'incomplete',error:String(error)}));},{once:true});
    if(signal?.aborted)throw signal.reason??Error('Session aborted');
    await connect();
    if(closed||closing)throw signal?.reason??Error('Session disposed during connection');
    return {getSnapshot:()=>snapshot,subscribe(listener){subscribers.add(listener);return()=>subscribers.delete(listener);},
      async mountDesktop(target,mountOptions={}){
        if(closed||closing)throw Error('Session closed');
        if(snapshot.phase!=='ready')throw Error('Certified record hydration is incomplete');
        const app=mountOptions.app??'writer';
        const record=snapshot.records.find(row=>row.app_id===(app==='writer'?'dev.qos.word-processor':'dev.qos.chat'));
        if(!record)throw Error('Canonical record missing');
        const controller=new AbortController();mountControllers.add(controller);
        const abort=()=>controller.abort(mountOptions.signal?.reason);
        mountOptions.signal?.addEventListener('abort',abort,{once:true});if(mountOptions.signal?.aborted)abort();
        const pending=mountDesktop(target,{...mountOptions,...namespace,signal:controller.signal,peer:id,record:record.record_id||undefined,author:snapshot.deviceId,onStatus:status=>{mountOptions.onStatus?.(status);if(status.phase==='presented')void persist().catch(()=>{});}});
        pendingMounts.add(pending);let mounted;
        try{mounted=await pending;}finally{pendingMounts.delete(pending);mountControllers.delete(controller);mountOptions.signal?.removeEventListener('abort',abort);}
        if(closed||closing){await mounted.cleanup();throw Error('Session disposed while mounting');}
        const cleanup=mounted.cleanup.bind(mounted);mounted.cleanup=async()=>{try{await cleanup();mounts.delete(mounted);failedMounts.delete(mounted);}catch(error){failedMounts.add(mounted);throw error;}};mounts.add(mounted);return mounted;
      },
      reconnect,
      retryPersistence:async()=>{await settleMounts(failedMounts);failed=false;await persist(!reconnecting);},failNextWrite:()=>{storage.abortNext=true;},durableState:()=>storage.hydrate(),dispose:close};
  }catch(error){try{await close();}catch(cleanupError){throw new AggregateError([error,cleanupError],'Peer setup failed; retained recovery is available');}throw error;}
}

}

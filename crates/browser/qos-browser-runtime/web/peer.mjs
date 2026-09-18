import init,* as runtime from './pkg/qos_browser_runtime.js';
import {mountDesktop} from './host.mjs';
import {acquireStorage} from './storage.mjs';
import {commitPeerWrites} from './peer-persistence.mjs';
let initialized;
async function leaseIdentity(slots,signal){
  if(!navigator.locks)throw Error('Certified actor custody requires Web Locks');
  for(const slot of slots){
    if(signal?.aborted)throw signal.reason??new DOMException('Aborted','AbortError');
    let resolve,reject,release;
    const acquired=new Promise((yes,no)=>{resolve=yes;reject=no;});
    const held=new Promise(yes=>{release=yes;});
    const task=navigator.locks.request(`qos-certified-peer:${slot}`,{ifAvailable:true,mode:'exclusive'},async lock=>{resolve(Boolean(lock));if(lock)await held;});
    task.catch(reject);
    if(await acquired)return {slot,release:async()=>{release();await task;}};
    await task;
  }
  throw Error('All certified browser identities are in use; close another session before connecting');
}
export async function connectPeer(options={}){
  const {enrollmentBaseUrl='/qos-peer/enrollment',identitySlots=['browser-a','browser-b'],signal}=options;
  const lease=await leaseIdentity(identitySlots,signal);
  let id,closed=false,closing=false,snapshot={phase:'authenticating',records:[]},connection,storage,namespace,raw,flush=Promise.resolve(),failed=false,generation=0;
  const subscribers=new Set(),mounts=new Set(),pendingMounts=new Set(),mountControllers=new Set();
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
  const connect=async()=>{
    const current=++generation;
    connection=runtime.connect_peer(raw,await storage.hydrate(),value=>queueMicrotask(()=>{if(current===generation)update(value);}));
    const next=await connection;id=next;
    if(closed||closing){await runtime.close_peer(next);return;}
    await persist();
  };
  async function close(){
    if(closed)return;
    if(closing){await connection?.catch(()=>{});return;}
    closing=true;
    try{
      for(const controller of mountControllers)controller.abort();
      await Promise.allSettled([...pendingMounts]);
      for(const mount of [...mounts])await mount.cleanup();
      await connection?.catch(()=>{});
      failed=false;await persist(false);
      generation++;if(id!==undefined)await runtime.close_peer(id);
      await storage?.release();await lease.release();closed=true;report({phase:'closed',error:null});
    }finally{closing=false;}
  }
  try{
    await (initialized??=init());
    const response=await fetch(`${enrollmentBaseUrl}/${encodeURIComponent(lease.slot)}`,{signal,cache:'no-store',credentials:'same-origin'});
    if(!response.ok)throw Error(`Enrollment HTTP ${response.status}`);
    // Signed u64 timestamps must never pass through JS numeric JSON decoding.
    raw=await response.text();namespace=JSON.parse(runtime.peer_enrollment(raw));
    storage=await acquireStorage(namespace.profile,namespace.device);
    if(storage.fresh){runtime.refresh_storage_lease(namespace.profile,namespace.device);storage.fresh=false;}
    signal?.addEventListener('abort',()=>{void close().catch(error=>report({phase:'incomplete',error:String(error)}));},{once:true});
    if(signal?.aborted)throw signal.reason??Error('Session aborted');
    await connect();
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
        const cleanup=mounted.cleanup.bind(mounted);mounted.cleanup=async()=>{await cleanup();mounts.delete(mounted);};mounts.add(mounted);return mounted;
      },
      async reconnect(){if(closed||closing)throw Error('Session closed');failed=false;await persist(false);const old=id;generation++;if(old!==undefined)await runtime.close_peer(old);report({phase:'authenticating',error:null});await connect();if(old!==undefined)runtime.rebind_peer(old,id);},
      retryPersistence:async()=>{failed=false;await persist();},failNextWrite:()=>{storage.abortNext=true;},durableState:()=>storage.hydrate(),dispose:close};
  }catch(error){await close();throw error;}
}

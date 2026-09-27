import {transactionComplete} from './host-core.mjs';
import {createTransferStaging,captureTransferHandle,commitTransfer} from './transfer-staging.mjs';
export const requestResult=request=>new Promise((resolve,reject)=>{
  request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);
});
export function storageRevision(value){
 if(typeof value==='number'){if(!Number.isSafeInteger(value)||value<0)throw Error('Invalid storage revision');return BigInt(value);}
 if(typeof value!=='string'||!/^(0|[1-9][0-9]*)$/.test(value)||value.length>20)throw Error('Invalid storage revision');
 const revision=BigInt(value);if(revision>18446744073709551615n)throw Error('Invalid storage revision');return revision;
}
/** One exclusive writer lease per profile/device namespace, in whichever agent
 * runs it: the persistence worker normally, the page when no worker is usable.
 * The lease owns the Web Lock `tilo-writer:<namespace>`, one IndexedDB
 * connection, and the strict transactions that make writes durable. */
export function createInlineStorage({locks,indexedDB,crypto}={}){
  const leases=new Map();
  async function acquireStorage(profile,device,{epoch:reusedEpoch}={}){
    const namespace=JSON.stringify([profile,device]);
    if(leases.has(namespace)){
      const lease=await leases.get(namespace);
      if(lease.closing){await lease.closing;return acquireStorage(profile,device,{epoch:reusedEpoch});}
      lease.references++;return lease;
    }
    const pending=(async()=>{
      const lockManager=locks??globalThis.navigator?.locks,idb=indexedDB??globalThis.indexedDB;
      if(!lockManager)throw Error('Web Locks unavailable: exclusive Automerge actor custody is required');
      let unlock,acquiredResolve,acquiredReject;
      const acquired=new Promise((resolve,reject)=>{acquiredResolve=resolve;acquiredReject=reject;});
      const held=new Promise(resolve=>{unlock=resolve;});
      const lockTask=lockManager.request(`tilo-writer:${namespace}`,{mode:'exclusive',ifAvailable:true},async lock=>{
        if(!lock){acquiredReject(Error('This profile/device is already open in another browser tab'));return;}
        acquiredResolve();await held;
      });
      await acquired;
      const request=idb.open('tilo-browser-records-v1',1);
      request.onupgradeneeded=()=>request.result.createObjectStore('records',{keyPath:'id'});
      // Release before rejecting so a retry never sees this attempt's lock.
      let db;try{db=await requestResult(request);}catch(error){unlock();await lockTask;throw error;}
      const epoch=reusedEpoch??(crypto??globalThis.crypto).randomUUID();
      let queue=Promise.resolve(),pendingStages=0;
      const enqueue=(task,staging=false,prepare=()=>undefined)=>{
        if(lease.closing)return Promise.reject(Error('Storage lease is closing'));
        if(staging&&pendingStages>=2)return Promise.reject(Object.assign(Error('Transfer staging has two pending operations; await completion'),{name:'QuotaExceededError'}));
        let captured;try{captured=prepare();}catch(error){return Promise.reject(error);}
        if(staging)pendingStages++;
        const result=queue.then(()=>task(captured));
        queue=result.then(()=>{},()=>{});
        return staging?result.finally(()=>{pendingStages--;}):result;
      };
      const lease={namespace,db,epoch,references:1,fresh:true,abortNext:false,
        hydrate(){return enqueue(async()=>{
          const tx=db.transaction('records','readonly'),store=tx.objectStore('records'),done=transactionComplete(tx),entries=[];
          // String canonical keys sort before private array keys. A key cursor
          // skips unrelated payload values, including Blob-backed staging.
          const lower=JSON.stringify([namespace]).slice(0,-1)+',',upper=lower+'\uffff';
          const request=store.openKeyCursor();
          request.onsuccess=()=>{
            const cursor=request.result;if(!cursor)return;
            if(typeof cursor.key!=='string'||cursor.key>=upper)return;
            if(cursor.key<lower){cursor.continue(lower);return;}
            const row=store.get(cursor.key);row.onsuccess=()=>{if(row.result?.namespace===namespace)entries.push(row.result);};cursor.continue();
          };
          await done;return entries;
        });},
        commit(writes,{transfer}={}){
          if(!writes.length&&transfer===undefined)return Promise.resolve();
          return enqueue(async({writes,handle,abortNext})=>{
          if(!writes.length){if(handle)throw Error('Empty canonical transfer commit');return;}
          const tx=db.transaction('records','readwrite',{durability:'strict'}),store=tx.objectStore('records');
          const done=transactionComplete(tx);let failure;
          const fail=error=>{failure=error;};
          const applyWrites=()=>{
            for(const write of writes){
              const record={...write,epoch,namespace,id:JSON.stringify([namespace,write.key])};
              const previous=store.get(record.id);
              previous.onsuccess=()=>{
                try{
                  if(previous.result?.epoch===epoch&&storageRevision(previous.result.revision)>storageRevision(record.revision)){
                    if(handle){fail(Error('Stale canonical revision cannot finalize transfer'));tx.abort();}return;
                  }
                  const request=store.put(record);
                  if(abortNext){abortNext=false;request.onsuccess=()=>tx.abort();}
                }catch(error){fail(error);tx.abort();}
              };
            }
          };
          if(handle)commitTransfer(store,tx,{namespace,epoch,handle},applyWrites,fail);else applyWrites();
          try{await done;}catch(error){throw failure??error;}
        },false,()=>{
          const handle=transfer===undefined?undefined:captureTransferHandle(transfer);
          const captured=writes.map(write=>{storageRevision(write.revision);return {...write,bytes:new Uint8Array(write.bytes),actor:write.actor?new Uint8Array(write.actor):null};});
          const abortNext=lease.abortNext;lease.abortNext=false;return {writes:captured,handle,abortNext};
        });},
        async release(){
          if(lease.closing)return lease.closing;
          if(--lease.references>0)return;
          lease.closing=(async()=>{await queue;db.close();unlock();await lockTask;leases.delete(namespace);})();
          return lease.closing;
        }
      };
      lease.transfers=createTransferStaging({db,namespace,epoch,enqueue});
      return lease;
    })();
    leases.set(namespace,pending);
    try{return await pending;}catch(error){leases.delete(namespace);throw error;}
  }
  return {acquireStorage};
}

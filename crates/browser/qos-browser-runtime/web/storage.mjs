import {transactionComplete} from './host-core.mjs';
const leases = new Map();
const requestResult = request => new Promise((resolve,reject) => {
  request.onsuccess=()=>resolve(request.result); request.onerror=()=>reject(request.error);
});
export async function acquireStorage(profile,device) {
  const namespace=JSON.stringify([profile,device]);
  if (leases.has(namespace)) {
    const lease=await leases.get(namespace);
    if(lease.closing){await lease.closing;return acquireStorage(profile,device);}
    lease.references++;return lease;
  }
  const pending=(async()=>{
    if (!navigator.locks) throw Error('Web Locks unavailable: exclusive Automerge actor custody is required');
    let unlock, acquiredResolve, acquiredReject;
    const acquired=new Promise((resolve,reject)=>{acquiredResolve=resolve;acquiredReject=reject;});
    const held=new Promise(resolve=>{unlock=resolve;});
    const lockTask=navigator.locks.request(`qos-writer:${namespace}`,{mode:'exclusive',ifAvailable:true},async lock=>{
      if (!lock){acquiredReject(Error('This profile/device is already open in another browser tab'));return;}
      acquiredResolve();await held;
    });
    await acquired;
    const request=indexedDB.open('qos-browser-records-v1',1);
    request.onupgradeneeded=()=>request.result.createObjectStore('records',{keyPath:'id'});
    let db;try{db=await requestResult(request);}catch(error){unlock();throw error;}
    const epoch=crypto.randomUUID();
    const lease={namespace,db,epoch,references:1,fresh:true,abortNext:false,
      async hydrate(){
        const tx=db.transaction('records','readonly');const done=transactionComplete(tx);
        const entries=await requestResult(tx.objectStore('records').getAll());await done;
        return entries.filter(row=>row.namespace===namespace);
      },
      async commit(writes){
        if(!writes.length)return;
        for(const write of writes)storageRevision(write.revision);
        const tx=db.transaction('records','readwrite',{durability:'strict'});
        const done=transactionComplete(tx);
        for(const write of writes){
          const record={...write,epoch,bytes:new Uint8Array(write.bytes),actor:write.actor?new Uint8Array(write.actor):null,namespace,id:JSON.stringify([namespace,write.key])};
          const store=tx.objectStore('records');
          const previous=store.get(record.id);
          previous.onsuccess=()=>{
            if(previous.result?.epoch===epoch&&storageRevision(previous.result.revision)>storageRevision(record.revision))return;
            const request=store.put(record);
            if(lease.abortNext){lease.abortNext=false;request.onsuccess=()=>tx.abort();}
          };
        }
        await done;
      },
      async release(){
        if(lease.closing)return lease.closing;
        if(--lease.references>0)return;
        lease.closing=(async()=>{db.close();unlock();await lockTask;leases.delete(namespace);})();
        return lease.closing;
      }
    };
    return lease;
  })();
  leases.set(namespace,pending);
  try{return await pending;}catch(error){leases.delete(namespace);throw error;}
}

/** Client-only key material; success is acknowledged at strict transaction commit. */
export function createIdentityStore({idb=globalThis.indexedDB}={}){
 async function open(){const request=idb.open('qos-certified-identities-v2',1);request.onupgradeneeded=()=>request.result.createObjectStore('identities');return requestResult(request);}
 return {
  async load(key){const db=await open();try{const tx=db.transaction('identities','readonly');const done=transactionComplete(tx);const value=await requestResult(tx.objectStore('identities').get(key));await done;return value;}finally{db.close();}},
  async save(key,material){const db=await open();try{const tx=db.transaction('identities','readwrite',{durability:'strict'});const done=transactionComplete(tx);tx.objectStore('identities').put(material,key);await done;}finally{db.close();}}
 };
}
export function storageRevision(value){
 if(typeof value==='number'){if(!Number.isSafeInteger(value)||value<0)throw Error('Invalid storage revision');return BigInt(value);}
 if(typeof value!=='string'||!/^(0|[1-9][0-9]*)$/.test(value)||value.length>20)throw Error('Invalid storage revision');
 const revision=BigInt(value);if(revision>18446744073709551615n)throw Error('Invalid storage revision');return revision;
}

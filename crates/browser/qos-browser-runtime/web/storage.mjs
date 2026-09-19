import {transactionComplete} from './host-core.mjs';
import {createInlineStorage,requestResult,storageRevision} from './storage-inline.mjs';
export {storageRevision};
export const acquireStorage=createInlineStorage().acquireStorage;

/** Client-only key material; success is acknowledged at strict transaction commit. */
export function createIdentityStore({idb=globalThis.indexedDB}={}){
 async function open(){const request=idb.open('qos-certified-identities-v2',1);request.onupgradeneeded=()=>request.result.createObjectStore('identities');return requestResult(request);}
 return {
  async load(key){const db=await open();try{const tx=db.transaction('identities','readonly');const done=transactionComplete(tx);const value=await requestResult(tx.objectStore('identities').get(key));await done;return value;}finally{db.close();}},
  async save(key,material){const db=await open();try{const tx=db.transaction('identities','readwrite',{durability:'strict'});const done=transactionComplete(tx);tx.objectStore('identities').put(material,key);await done;}finally{db.close();}}
 };
}

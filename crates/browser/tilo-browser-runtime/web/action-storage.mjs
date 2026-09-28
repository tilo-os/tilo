import {STORAGE_WORKER_FAILED} from './persistence-protocol.mjs';
import {storageRevision} from './storage-inline.mjs';
const bytesEqual=(a,b)=>a?.length===b?.length&&a.every((byte,i)=>byte===b[i]);
const same=(a,b)=>!a||!b?a===b:a.key===b.key&&a.epoch===b.epoch&&String(a.revision)===String(b.revision)&&bytesEqual(a.bytes,b.bytes)&&((!a.actor&&!b.actor)||bytesEqual(a.actor,b.actor));
const capture=row=>row?{key:row.key,epoch:row.epoch,revision:String(row.revision),bytes:new Uint8Array(row.bytes),actor:row.actor?new Uint8Array(row.actor):null}:undefined;
/** One exact private row, same live lease/FIFO. A worker's death says nothing
 * about whether its last strict transaction committed. Retain both candidate
 * byte images until hydration proves one; never advance from a stale image. */
export function createActionStorage({storage,key,beforeCommit=async()=>{}}){
 let confirmed,attempted,loaded=false;
 const read=async()=>{const rows=(await storage.hydrate()).filter(row=>row.key===key);if(rows.length>1)throw Error('Conflicting action storage rows');return capture(rows[0]);};
 return {
  readLegacyImage(){if(!loaded||attempted)throw Error('Action storage requires reconciliation');return confirmed?new TextDecoder('utf-8',{fatal:true}).decode(confirmed.bytes):undefined;},
  async load(){if(attempted)throw Error('Action storage requires reconciliation');confirmed=await read();if(confirmed)storageRevision(confirmed.revision);loaded=true;return confirmed?JSON.parse(new TextDecoder().decode(confirmed.bytes)):undefined;},
  async commit(value,control,options){
   if(!loaded||attempted)throw Error('Action storage requires reconciliation');
   const revision=storageRevision(confirmed?.revision??'0')+1n;if(revision>18446744073709551615n)throw Error('Action storage revision exhausted');
   const next={key,epoch:storage.epoch,revision:String(revision),bytes:new TextEncoder().encode(JSON.stringify(value)),actor:null};
   await beforeCommit(control,options);control?.check();
   try{await storage.commit([{key,revision:next.revision,bytes:new Uint8Array(next.bytes),actor:null}]);}
   catch(error){if(error?.name===STORAGE_WORKER_FAILED)attempted=next;throw error;}
   confirmed=next;
  },
  async reconcile(){
   if(!attempted)throw Error('No uncertain action write to reconcile');
   const actual=await read();
   if(same(actual,attempted)){confirmed=attempted;attempted=undefined;return 'committed';}
   if(same(actual,confirmed)){attempted=undefined;return 'aborted';}
   throw Error('Action storage differs from both confirmed and attempted write');
  },
 };
}
